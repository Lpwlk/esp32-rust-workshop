//! Shared firmware for two ESP32-S31-Function-CoreBoard-1 boards linked by **Gigabit Ethernet**.
//!
//! The same node as the UART firmware (`esp32-rust-workshop`), with the UART replaced by the
//! board's RJ45, static IP addresses and UDP or TCP. Each board:
//! - sends a `DAT` frame (counter, timestamp, button state, period) periodically,
//!   or replies to the peer's frames (`tx_mode on-rx`), or stays quiet;
//! - sends an `EVT` frame with a random 64-bit key on each rising/falling edge of a jumper
//!   "button" and of the BOOT button (GPIO interrupt on both edges);
//! - decodes and displays everything it receives from the other board;
//! - sends bursts of frames to measure the link (`burst`);
//! - drives the on-board RGB LED and two LEDs on the breadboard;
//! - offers the same CLI on the USB console, with settings saved in flash.
//!
//! The frames, the CLI, the settings and their validation are the ones of the UART firmware: they
//! live in `workshop-core` and are shared. What this crate adds is the link: [`net`] (PHY, EMAC,
//! network stack) and [`link`] (frames over UDP or TCP).
//!
//! **Configuration**: each node is a preset, `presets/eth/node-a.toml` (see `presets/README.md`).
//! `build.rs` turns it into a `const NodeConfig`, checked by the compiler.
//!
//! ```text
//!   Board A  192.168.77.11                 Board B  192.168.77.12
//!   RJ45 ═══════════ a cable (or a switch) ═══════════ RJ45        GPIO5-19: the PHY
//!
//!   Each board:
//!   "4"  GPIO4  ── jumper ── "G" GND           button (plugged = pressed)
//!   "D0" GPIO20 ── 330 Ω ── LED1 ── GND        breadboard LED 1
//!   "D1" GPIO21 ── 330 Ω ── LED2 ── GND        breadboard LED 2
//! ```
#![no_std]

// Hardware glue (this crate).
pub mod button;
pub mod cli;
pub mod console;
pub mod ext_leds;
pub mod fx;
pub mod led;
pub mod link;
pub mod net;
pub mod rtt_console;
pub mod settings;
pub mod term;
pub mod trng;

// Hardware-independent logic, tested on the host (`cargo test-host` in the repository root).
pub use workshop_core::{burst, config, line, persist, protocol, rtt, seq, units, wire};

use embassy_executor::Spawner;
use esp_hal::{
    clock::{ClockConfig, CpuClock, ll::MpllClkConfig},
    gpio::AnyPin,
    rng::Rng,
    time::{Duration, Instant},
    timer::timg::TimerGroup,
    usb::usb_serial_jtag::UsbSerialJtag,
};
use workshop_core::config::Transport;

use crate::{
    burst::{BurstRx, BurstTx},
    button::{Button, ButtonUpdate, Source},
    config::{NodeConfig, TxMode},
    ext_leds::{ExtLeds, LedInputs},
    link::{Link, SendError},
    protocol::{Edge, Frame, MAX_FRAME, Payload},
    rtt::RttTracker,
    seq::SeqCheck,
    term::Stats,
    wire::{DatClock, DatDelta, Dir},
};

/// Expands to a complete node firmware: compile-time config check, app descriptor, panic
/// handler and the async `main`. A binary is then just its preset:
///
/// ```ignore
/// const CONFIG: NodeConfig = include!(concat!(env!("OUT_DIR"), "/node_a.rs"));
/// eth_lab::node_firmware!(CONFIG);
/// ```
#[macro_export]
macro_rules! node_firmware {
    ($config:path) => {
        use esp_backtrace as _;

        const _: () = $config.check();
        // This firmware is the Ethernet one: a UART config can't be built into it.
        const _: $crate::config::NetLink = $config.net();

        // App descriptor required by the ESP-IDF second-stage bootloader.
        esp_bootloader_esp_idf::esp_app_desc!();

        #[esp_rtos::main]
        async fn main(spawner: embassy_executor::Spawner) {
            $crate::run(spawner, &$config).await
        }
    };
}

/// A back-to-back burst sends for at most this long per loop iteration, so the CLI, the LEDs and
/// the receive side keep running during long bursts.
const BURST_SLICE_US: u64 = 2_000;
/// With the `watchdog` feature: the chip resets if the main loop doesn't come back for this long
/// (its longest normal iteration, a `save`, takes ~50 ms).
pub const WATCHDOG_MS: u64 = 2_000;

/// Hardware watchdog on TIMG0, fed once per main loop iteration (`watchdog` feature). Without the
/// feature, feeding does nothing. (TIMG1 is esp-rtos's tick.)
struct Watchdog {
    #[cfg(feature = "watchdog")]
    wdt: esp_hal::timer::timg::Wdt<esp_hal::peripherals::TIMG0<'static>>,
}

impl Watchdog {
    #[cfg_attr(not(feature = "watchdog"), allow(unused_variables))]
    fn start(timg0: esp_hal::peripherals::TIMG0<'static>) -> Self {
        #[cfg(feature = "watchdog")]
        {
            use esp_hal::timer::timg::MwdtStage;
            let mut wdt = TimerGroup::new(timg0).wdt;
            wdt.set_timeout(MwdtStage::Stage0, Duration::from_millis(WATCHDOG_MS));
            wdt.enable();
            Self { wdt }
        }
        #[cfg(not(feature = "watchdog"))]
        Self {}
    }

    fn feed(&mut self) {
        #[cfg(feature = "watchdog")]
        self.wdt.feed();
    }
}

fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

fn now_ms() -> u64 {
    now_us() / 1000
}

fn ms(v: u32) -> Duration {
    Duration::from_millis(u64::from(v))
}

/// A period in ms with decimals (config / CLI) as a timer duration.
fn ms_f(v: f32) -> Duration {
    Duration::from_micros(units::ms_to_us(v))
}

/// Takes a GPIO by number, as given in the config.
fn pin(n: u8) -> AnyPin<'static> {
    // SAFETY: `NodeConfig::check` guarantees at compile time that every configured pin is
    // distinct, not reserved, and outside the PHY's GPIO5-19.
    unsafe { AnyPin::steal(n) }
}

fn fx_settings(rgb: &config::RgbConfig) -> fx::Settings {
    fx::Settings {
        pattern: fx::find(rgb.pattern).unwrap_or(0),
        freq_hz: rgb.freq_hz,
        cycle_ms: rgb.cycle_ms,
        brightness: rgb.brightness,
    }
}

/// What we know about the other node, from its frames.
#[derive(Default)]
struct Peer {
    seq: Option<u32>,
    last_rx: Option<Instant>,
    /// Its data period (µs), announced in its DAT frames.
    period_us: Option<u32>,
    button: bool,
}

impl Peer {
    fn timeout_us(&self, cfg: &NodeConfig) -> u64 {
        let period_us = self.period_us.map_or(units::ms_to_us(cfg.data_period_ms), u64::from);
        period_us * u64::from(cfg.link_timeout_periods)
    }

    fn link_up(&self, cfg: &NodeConfig) -> bool {
        self.last_rx.is_some_and(|t| t.elapsed().as_micros() < self.timeout_us(cfg))
    }
}

/// What `set console` really does when the `rtt` feature isn't built in.
fn console_note(console: config::Console) -> &'static str {
    match (console, rtt_console::BUILT) {
        (config::Console::Rtt, false) => " (no RTT in this build: staying on serial; rebuild with --features rtt)",
        (config::Console::Both, false) => " (no RTT in this build: serial only; rebuild with --features rtt)",
        _ => "",
    }
}

/// Entry point shared by both binaries (see [`node_firmware!`]).
///
/// `defaults` is the compiled-in config. The firmware works on a copy that the CLI can change
/// (`set`, `reset`); `defaults` is kept to show and restore it.
pub async fn run(spawner: Spawner, defaults: &'static NodeConfig) -> ! {
    let mut cfg = *defaults;
    let net_cfg = cfg.net();
    // Before any output, so the boot card also goes to RTT (feature `rtt`).
    rtt_console::init();
    rtt_console::set_route(cfg.console);

    // 1 Gbit/s needs the MPLL at 500 MHz (the HAL default of 400 MHz caps RGMII at 100 Mbit/s).
    let mut clocks = ClockConfig::from(CpuClock::max());
    if net_cfg.max_speed >= 1000 {
        clocks.mpll_clk = Some(MpllClkConfig::_500);
    }
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(clocks));
    esp_rtos::start(TimerGroup::new(p.TIMG1).timer0);

    // The USB console: the TX half is drained by a task, so printing never blocks the node.
    let (usb_rx, usb_tx) = UsbSerialJtag::new(p.USB_DEVICE).split();
    spawner.spawn(console::console_task(usb_tx).unwrap());

    // Settings saved with `save` (feature `persist`), on top of the build config.
    let mut store = settings::Store::new(p.FLASH);
    let boot_load = store.load(&mut cfg);
    rtt_console::set_route(cfg.console);
    let pins = cfg.pins;

    // esp-hal doesn't start the TRNG on the S31: without this, every key is identical.
    trng::enable();
    let rng = Rng::new();
    let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());

    let (stack, runner) = net::start(
        net::PhyPeripherals {
            eth: p.ETH,
            mdc: p.GPIO5,
            mdio: p.GPIO6,
            reset: p.GPIO7,
            txd0: p.GPIO8,
            txd1: p.GPIO9,
            txd2: p.GPIO10,
            txd3: p.GPIO11,
            tx_ctl: p.GPIO12,
            tx_clk: p.GPIO13,
            rx_clk: p.GPIO14,
            rx_ctl: p.GPIO15,
            rxd3: p.GPIO16,
            rxd2: p.GPIO17,
            rxd1: p.GPIO18,
            rxd0: p.GPIO19,
        },
        &net_cfg,
        seed,
    )
    .await;
    spawner.spawn(net::net_task(runner).unwrap());
    match net_cfg.transport {
        Transport::Udp => spawner.spawn(link::udp_task(stack, net_cfg).unwrap()),
        Transport::Tcp => spawner.spawn(link::tcp_task(stack, net_cfg).unwrap()),
    }
    let link = Link::new(&net_cfg);

    // One GPIO interrupt handler for both buttons (src/button.rs).
    let gpio = button::Gpio::new(p.IO_MUX);
    let button = Button::new(&gpio, Source::Jumper, pin(pins.button), ms(cfg.debounce_ms), ms(cfg.lockout_ms));
    // BOOT button, active low on GPIO61 (board pull-up): same filter, same EVT frames.
    let boot = Button::new(&gpio, Source::Boot, p.GPIO61, ms(cfg.debounce_ms), ms(cfg.lockout_ms));
    let leds = ExtLeds::new([(pin(pins.led1), pins.led1), (pin(pins.led2), pins.led2)], [cfg.led1, cfg.led2]);
    let rgb_led = led::Led::new(p.RMT, p.GPIO60);
    let fx = fx::Animator::new(fx_settings(&cfg.rgb));

    term::boot_card(&cfg, &chip_info(&rng), &system_info(&cfg, defaults, boot_load));
    let cli = cli::Cli::new(usb_rx);

    let mut node = Node {
        cfg,
        defaults,
        link,
        button,
        leds,
        rgb_led,
        fx,
        rng,
        cli,
        stats: Stats::default(),
        peer: Peer::default(),
        seq: 0,
        events_sent: 0,
        last_data: Instant::now(),
        dat_clock: DatClock::new(),
        last_status: Instant::now(),
        rtt: RttTracker::new(),
        burst_id: 0,
        burst_tx: None,
        burst_rx: None,
        store,
        boot_load,
        _gpio: gpio,
        boot,
        seen_phy: None,
        seen_connected: link.connected(),
        seen_dropped: 0,
        loops: 0,
        // Last, so the boot itself is never cut short.
        watchdog: Watchdog::start(p.TIMG0),
    };
    loop {
        node.step();
        // Lets the network tasks run: they and this loop share the executor.
        embassy_futures::yield_now().await;
    }
}

/// A frame already handed to the network task, still to be printed.
struct Queued {
    seq: u32,
    t_ms: u32,
    payload: Payload,
    line: heapless::String<{ MAX_FRAME }>,
    sent_us: u64,
}

/// Everything one node owns and knows.
struct Node {
    cfg: NodeConfig,
    defaults: &'static NodeConfig,
    link: Link,
    button: Button,
    leds: ExtLeds,
    rgb_led: led::Led,
    fx: fx::Animator,
    rng: Rng,
    cli: cli::Cli,
    stats: Stats,
    peer: Peer,
    /// Next sequence number (DAT, EVT, ECH).
    seq: u32,
    events_sent: u32,
    last_data: Instant,
    /// Time between consecutive DAT frames, sent or received (timing).
    dat_clock: DatClock,
    last_status: Instant,
    rtt: RttTracker,
    burst_id: u32,
    burst_tx: Option<BurstTx>,
    burst_rx: Option<BurstRx>,
    /// Saved settings in flash (`save`, `forget`).
    store: settings::Store,
    /// What the boot did with the saved settings (shown by `info`).
    boot_load: settings::BootLoad,
    watchdog: Watchdog,
    /// Keeps the GPIO interrupt handler installed.
    _gpio: button::Gpio,
    /// The board's BOOT button (`button` is the jumper).
    boot: Button,
    /// The PHY / TCP state last reported, to print a line when it changes.
    seen_phy: Option<net::PhyLink>,
    seen_connected: bool,
    /// Frames the network task dropped (RX queue full) already reported.
    seen_dropped: u32,
    /// Main loop iterations since the last status line (shown in verbose mode).
    loops: u32,
}

impl Node {
    /// One iteration of the main loop.
    fn step(&mut self) {
        self.loops += 1;
        self.watchdog.feed();
        // --- LEDs and host CLI ------------------------------------------------
        self.fx.tick(&mut self.rgb_led);
        let inputs =
            LedInputs { button: self.pressed(), peer_button: self.peer.button, link_up: self.peer.link_up(&self.cfg) };
        self.leds.update(&inputs);
        if let Some(cmd) = self.cli.poll() {
            self.apply(cmd);
        }

        // --- Button edges (jumper, BOOT) -> EVT frames ------------------------------
        for source in [Source::Jumper, Source::Boot] {
            let update = match source {
                Source::Jumper => self.button.poll(),
                Source::Boot => self.boot.poll(),
            };
            match update {
                Some(ButtonUpdate::Edge(ev)) => {
                    let key = (u64::from(self.rng.random()) << 32) | u64::from(self.rng.random());
                    self.send(Payload::Event { edge: ev.edge, key }, Some((source.name(), ev.raw_edges)));
                    self.events_sent += 1;
                }
                Some(ButtonUpdate::Ignored { raw_edges }) if self.cfg.verbose => {
                    term::bounce_ignored(now_ms(), source.name(), raw_edges)
                }
                _ => {}
            }
        }

        // --- Periodic DAT frame (read from `cfg` every time: CLI changes apply at once)
        if self.last_data.elapsed() >= ms_f(self.cfg.data_period_ms) {
            self.last_data = Instant::now();
            if self.cfg.tx_mode == TxMode::Periodic {
                let period_us = units::ms_to_us(self.cfg.data_period_ms) as u32;
                let payload = Payload::Data { pressed: self.pressed(), events: self.events_sent, period_us };
                self.send(payload, None);
            }
        }

        self.burst_tx_step();
        self.receive();
        self.timers();
        self.watch_network();

        // --- Status line ----------------------------------------------------------
        if self.last_status.elapsed() >= ms_f(self.cfg.status_period_ms) {
            self.last_status = Instant::now();
            term::status(now_ms(), &self.stats, self.peer.link_up(&self.cfg), net::link_state(), self.rtt.stats());
            if self.cfg.verbose {
                let per_s = u64::from(self.loops) * 1000 / self.cfg.status_period_ms.max(1.0) as u64;
                self.loops = 0;
                term::link_counters(&self.link.counters(), net::link_state(), self.peer.period_us, per_s);
            }
        }
    }

    /// Either button held down: what `DAT` frames and the `button` LED mode show.
    fn pressed(&self) -> bool {
        self.button.is_pressed() || self.boot.is_pressed()
    }

    /// Prints a line when the cable, the negotiated speed or the TCP connection changes.
    fn watch_network(&mut self) {
        let phy = net::link_state();
        if self.seen_phy != Some(phy) {
            let first = self.seen_phy.is_none();
            self.seen_phy = Some(phy);
            if phy.up {
                term::link_event(
                    now_ms(),
                    format_args!(
                        "cable connected: {} · {} duplex",
                        term::Speed(phy.mbps),
                        if phy.full_duplex { "full" } else { "half" }
                    ),
                );
            } else if !first {
                term::link_event(now_ms(), format_args!("cable unplugged (or the partner went away)"));
            }
        }
        let connected = self.link.connected();
        if connected != self.seen_connected {
            self.seen_connected = connected;
            term::link_event(
                now_ms(),
                format_args!("{}", if connected { "TCP connected to the peer" } else { "TCP connection lost" }),
            );
        }
        let dropped = self.link.counters().rx_dropped;
        if dropped != self.seen_dropped {
            self.stats.bad += dropped - self.seen_dropped;
            self.seen_dropped = dropped;
            term::net_error(now_ms(), "RX queue full: frames dropped (the main loop is too slow for this rate)");
        }
    }

    /// Sends a frame with the next sequence number, and prints it (with, for a DAT, the time
    /// since the previous DAT, sent or received). `button`: which button an `EVT` comes from,
    /// and its raw interrupt count.
    fn send(&mut self, payload: Payload, button: Option<(&'static str, u32)>) {
        if let Some(queued) = self.queue(payload) {
            self.report(&queued, button);
        }
    }

    /// First half of [`Self::send`]: hands the frame to the network task. Nothing is printed, so a
    /// reply can leave before the console (which takes milliseconds) has shown anything.
    fn queue(&mut self, payload: Payload) -> Option<Queued> {
        let frame = Frame { node: self.cfg.name, seq: self.seq, t_ms: now_ms() as u32, payload };
        let line = protocol::encode(&frame);
        let result = self.link.send(line.as_bytes());
        self.seq = self.seq.wrapping_add(1);
        match result {
            Ok(()) => {
                let sent_us = now_us();
                self.stats.tx += 1;
                self.leds.on_tx();
                self.rtt.sent(frame.seq, sent_us);
                Some(Queued { seq: frame.seq, t_ms: frame.t_ms, payload, line, sent_us })
            }
            Err(SendError::NotConnected) => {
                term::net_error(now_ms(), "not sent: no TCP connection to the peer yet");
                None
            }
            Err(SendError::Full) => {
                term::net_error(now_ms(), "not sent: the network task is behind");
                None
            }
        }
    }

    /// Second half of [`Self::send`]: prints the frame that [`Self::queue`] sent.
    fn report(&mut self, q: &Queued, button: Option<(&'static str, u32)>) {
        let frame = Frame { node: self.cfg.name, seq: q.seq, t_ms: q.t_ms, payload: q.payload };
        let delta = self.dat_mark(&q.payload, Dir::Tx, q.sent_us);
        term::sent(now_ms(), &frame, button, delta);
        if self.cfg.verbose {
            term::wire(q.line.as_bytes(), None, None);
        }
    }

    /// For a DAT frame, the time since the previous DAT (either direction).
    fn dat_mark(&mut self, payload: &Payload, dir: Dir, at_us: u64) -> Option<DatDelta> {
        matches!(payload, Payload::Data { .. }).then(|| self.dat_clock.mark(dir, at_us)).flatten()
    }

    // --- Receive ------------------------------------------------------------------

    fn receive(&mut self) {
        // Bounded, so a flood can't starve the rest of the loop.
        for _ in 0..32 {
            let Some(packet) = self.link.recv() else { break };
            self.handle_line(&packet.data, packet.at_us);
        }
    }

    /// The burst being received, if frames still arrive.
    fn active_burst_rx(&mut self) -> Option<&mut BurstRx> {
        let now = now_us();
        self.burst_rx.as_mut().filter(|rx| !rx.is_stale(now))
    }

    fn handle_line(&mut self, line: &[u8], arrived_us: u64) {
        let parsed = core::str::from_utf8(line).map_err(|_| protocol::ParseError::Malformed).and_then(protocol::parse);
        let frame = match parsed {
            Ok(frame) => frame,
            Err(e) => {
                self.stats.bad += 1;
                match self.active_burst_rx() {
                    Some(rx) => rx.bad += 1,
                    None => term::rx_error(now_ms(), e, line),
                }
                return;
            }
        };
        let now = now_us();
        let since_previous = self.peer.last_rx.map(|t| t.elapsed().as_millis());
        self.peer.last_rx = Some(Instant::now());
        self.leds.on_rx();

        // Burst frames: counted, not printed, outside the frame sequence.
        if let Payload::Burst { index, count } = frame.payload {
            self.burst_frame(frame.node, frame.seq, index, count, line.len() + 1, now);
            return;
        }

        self.stats.rx += 1;
        match seq::check(self.peer.seq, frame.seq) {
            SeqCheck::Lost(missing) => {
                self.stats.lost += missing;
                term::lost(now_ms(), missing);
            }
            SeqCheck::Restarted => term::peer_restarted(now_ms(), frame.node),
            SeqCheck::First | SeqCheck::InOrder => {}
        }
        self.peer.seq = Some(frame.seq);
        let mut rtt = None;
        match frame.payload {
            Payload::Data { pressed, period_us, .. } => {
                self.peer.button = pressed;
                self.peer.period_us = Some(period_us);
            }
            Payload::Event { edge, .. } => {
                self.peer.button = edge == Edge::Falling;
            }
            Payload::Echo { echo_seq } => rtt = self.rtt.echo(echo_seq, arrived_us),
            // `BAU` frames belong to the UART link: shown, not acted upon.
            Payload::Baud { .. } | Payload::Burst { .. } => {}
        }
        let delta = self.dat_mark(&frame.payload, Dir::Rx, arrived_us);

        // `on-rx`: answer first, print after. Printing a line to the USB console takes
        // milliseconds, which would otherwise end up in the peer's round-trip time.
        let reply = matches!(frame.payload, Payload::Data { .. } | Payload::Event { .. });
        let echo =
            (reply && self.cfg.tx_mode == TxMode::OnRx).then(|| self.queue(Payload::Echo { echo_seq: frame.seq }));

        term::received(now_ms(), &frame, rtt, delta);
        if self.cfg.verbose {
            term::wire(line, since_previous, Some(now.saturating_sub(arrived_us)));
        }
        if let Some(Some(queued)) = echo {
            self.report(&queued, None);
        }
    }

    fn timers(&mut self) {
        if self.burst_rx.as_ref().is_some_and(|rx| rx.is_stale(now_us())) {
            self.finish_burst_rx();
        }
    }

    // --- Bursts -------------------------------------------------------------------

    /// What the link can carry, bytes/s: the negotiated speed, or the allowed maximum while the
    /// cable is out.
    fn capacity(&self) -> u64 {
        let mbps = match net::link_state().mbps {
            0 => self.cfg.net().max_speed,
            m => m,
        };
        u64::from(mbps) * 125_000
    }

    fn start_burst(&mut self, count: u32, rate_hz: f32) {
        if self.burst_tx.is_some() {
            term::cli_error(format_args!("a burst is already being sent"));
            return;
        }
        self.burst_id = self.burst_id.wrapping_add(1);
        term::burst_start(now_ms(), self.burst_id, count, rate_hz);
        self.burst_tx = Some(BurstTx::new(self.burst_id, count, rate_hz, now_us()));
    }

    fn burst_tx_step(&mut self) {
        let Some(tx) = self.burst_tx.as_mut() else { return };
        let slice_end = now_us() + BURST_SLICE_US;
        // `can_send` first: a frame taken from the schedule must go out.
        while now_us() < slice_end && self.link.can_send() {
            let Some(index) = tx.next(now_us()) else { break };
            let frame = Frame {
                node: self.cfg.name,
                seq: tx.id,
                t_ms: now_ms() as u32,
                payload: Payload::Burst { index, count: tx.count },
            };
            let line = protocol::encode(&frame);
            if self.link.send(line.as_bytes()).is_err() {
                term::net_error(now_ms(), "TX error during burst");
                break;
            }
            tx.sent(line.len());
            self.leds.on_tx();
        }
        if tx.is_done() {
            let (id, frames, throughput) = (tx.id, tx.sent, tx.throughput());
            self.burst_tx = None;
            term::burst_sent(now_ms(), id, frames, &throughput, self.capacity());
        }
    }

    fn burst_frame(&mut self, node: &str, id: u32, index: u32, count: u32, bytes: usize, now: u64) {
        if !self.burst_rx.as_ref().is_some_and(|rx| rx.matches(node, id)) {
            self.finish_burst_rx(); // a previous burst that lost its end
            self.burst_rx = Some(BurstRx::new(node, id, count, now));
        }
        if let Some(rx) = self.burst_rx.as_mut() {
            rx.frame(index, bytes, now);
            if rx.is_complete() {
                self.finish_burst_rx();
            }
        }
    }

    fn finish_burst_rx(&mut self) {
        if let Some(rx) = self.burst_rx.take() {
            term::burst_received(now_ms(), &rx.node, rx.id, &rx.summary(), self.capacity());
        }
    }

    // --- CLI --------------------------------------------------------------------

    /// Pushes the (possibly changed) config to the parts that keep their own copy.
    fn sync(&mut self) {
        self.fx.settings = fx_settings(&self.cfg.rgb);
        self.leds.modes = [self.cfg.led1, self.cfg.led2];
        self.button.set_timing(ms(self.cfg.debounce_ms), ms(self.cfg.lockout_ms));
        self.boot.set_timing(ms(self.cfg.debounce_ms), ms(self.cfg.lockout_ms));
        rtt_console::set_route(self.cfg.console);
    }

    /// `set <key> <value>`: validates, applies, and reports the result.
    fn set_key(&mut self, key: &str, value: &str) {
        use config::{Key, SetError};
        match self.cfg.set(key, value) {
            Ok(k) => {
                if k == Key::Console {
                    // Confirm on both sides before switching, so the console being left knows
                    // where the output went.
                    rtt_console::set_route(config::Console::Both);
                    term::cli_ok(format_args!(
                        "{} = {}{}",
                        k.name(),
                        self.cfg.value(k),
                        console_note(self.cfg.console)
                    ));
                    self.sync();
                    return;
                }
                self.sync();
                term::cli_ok(format_args!("{} = {}", k.name(), self.cfg.value(k)));
                if k == Key::RgbPattern {
                    term::cli_status(&self.fx.settings, self.fx.last, &self.leds);
                }
            }
            Err(SetError::UnknownKey) => term::cli_error(format_args!("unknown key `{key}` (see `config`)")),
            Err(SetError::ReadOnly) => {
                term::cli_error(format_args!("`{key}` is read-only: change it in presets/eth/*.toml and rebuild"))
            }
            Err(SetError::BadValue(expected)) => term::cli_error(format_args!("`{key}`: expected {expected}")),
            Err(SetError::Invalid(msg)) => term::cli_error(format_args!("{}", msg.trim_start_matches("config: "))),
        }
    }

    /// Applies a CLI command and reports the result.
    fn apply(&mut self, cmd: cli::Command) {
        use cli::Command;
        use config::Key;

        match cmd {
            Command::Help(topic) => term::cli_help(topic.as_deref(), &self.cfg, self.defaults),
            Command::List => term::cli_list(self.fx.settings.pattern),
            Command::Status => term::cli_status(&self.fx.settings, self.fx.last, &self.leds),
            Command::Config => term::cli_config(&self.cfg, self.defaults),
            Command::Set { key, value } => self.set_key(&key, &value),
            Command::NextPattern => {
                let next = (self.fx.settings.pattern + 1) % fx::PATTERNS.len();
                self.set_key("rgb.pattern", fx::PATTERNS[next].name);
            }
            Command::Reset(None) => {
                self.cfg = *self.defaults;
                self.sync();
                term::cli_ok(format_args!("all settings back to their build values"));
            }
            Command::Reset(Some(key)) => match Key::from_name(&key) {
                Some(k) => {
                    self.cfg.reset(k, self.defaults);
                    self.sync();
                    term::cli_ok(format_args!("{} = {}  (build value)", k.name(), self.cfg.value(k)));
                }
                None if self.cfg.read_only().contains(&key.as_str()) => {
                    term::cli_error(format_args!("`{key}` is read-only, it always has its build value"))
                }
                None => term::cli_error(format_args!("unknown key `{key}` (see `config`)")),
            },
            Command::Info => term::boot_card(
                &self.cfg,
                &chip_info(&self.rng),
                &system_info(&self.cfg, self.defaults, self.boot_load),
            ),
            Command::Save => match self.store.save(&self.cfg, self.defaults) {
                Ok(0) => term::cli_ok(format_args!("saved: nothing differs from the build config, boots on it")),
                Ok(n) => term::cli_ok(format_args!("saved {n} setting(s): applied at every boot (`forget` to erase)")),
                Err(e) => term::cli_error(format_args!("not saved: {e}")),
            },
            Command::Forget => match self.store.forget() {
                Ok(()) => term::cli_ok(format_args!("saved settings erased: next boot uses the build config")),
                Err(e) => term::cli_error(format_args!("{e}")),
            },
            Command::Reboot => {
                term::cli_ok(format_args!("rebooting…"));
                // Let the console drain the message before the reset.
                let t = Instant::now();
                while t.elapsed() < Duration::from_millis(50) {}
                esp_hal::system::software_reset();
            }
            Command::Link => term::link_info(
                &self.cfg,
                net::link_state(),
                self.link.connected(),
                &self.link.counters(),
                self.peer.link_up(&self.cfg),
            ),
            Command::Baud(_) => term::cli_error(format_args!(
                "`baud` is for the UART firmware: this one has an Ethernet link (see `link`)"
            )),
            Command::Burst { count: None, rate_hz: None } => self.cli.start_burst_wizard(self.cfg.burst),
            Command::Burst { count, rate_hz } => {
                self.start_burst(count.unwrap_or(self.cfg.burst.count), rate_hz.unwrap_or(self.cfg.burst.rate_hz));
            }
        }
    }
}

/// Compiled-in features and saved settings, for the boot card.
fn system_info(cfg: &NodeConfig, defaults: &NodeConfig, boot_load: settings::BootLoad) -> term::SystemInfo {
    term::SystemInfo {
        features: [
            ("persist", cfg!(feature = "persist")),
            ("watchdog", cfg!(feature = "watchdog")),
            ("rtt", cfg!(feature = "rtt")),
        ],
        boot_load,
        unsaved: persist::changed(cfg, defaults),
        watchdog_ms: cfg!(feature = "watchdog").then_some(WATCHDOG_MS),
    }
}

fn chip_info(rng: &Rng) -> term::ChipInfo {
    use core::fmt::Write as _;
    let rev = esp_hal::efuse::chip_revision();
    let mut reset = heapless::String::new();
    let _ = match esp_hal::system::reset_reason() {
        // Name and raw code, as printed by the ROM at boot (`rst:0x17`).
        Some(reason) => write!(reset, "{reason:?} (rst 0x{:02X})", reason as u32),
        None => write!(reset, "unknown"),
    };
    // The Ethernet MAC address (the chip's base MAC + 3), the one the peer sees.
    let mac = net::mac_address();
    term::ChipInfo {
        rev_major: rev.major,
        rev_minor: rev.minor,
        cpu_mhz: esp_hal::clock::cpu_clock().as_mhz(),
        mac,
        reset,
        rng_samples: [rng.random(), rng.random(), rng.random()],
    }
}
