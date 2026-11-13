//! Shared firmware for two ESP32-S31-Function-CoreBoard-1 boards linked by UART.
//!
//! Each board:
//! - sends a `DAT` frame (counter, timestamp, button state, period) periodically,
//!   or replies to the peer's frames (`tx_mode on-rx`), or stays quiet;
//! - sends an `EVT` frame with a random 64-bit key on each rising/falling edge
//!   of a jumper "button" (GPIO interrupt on both edges);
//! - decodes and displays everything it receives from the other board;
//! - can change the link speed together with the peer (`baud`), and send bursts
//!   of frames to measure the link (`burst`);
//! - drives the on-board RGB LED and two LEDs on the breadboard;
//! - offers a small CLI on the USB console.
//!
//! **Configuration**: see [`config`]. Wire-level settings shared by both nodes are
//! in [`config::shared`]; everything else is per node, in its preset (`presets/uart/node-*.toml`).
//!
//! Default wiring (header J2 labels = GPIO numbers):
//!
//! ```text
//!   Board A                              Board B
//!   "2"  GPIO2 (TX) ──────────────────▶  "3"  GPIO3 (RX)
//!   "3"  GPIO3 (RX) ◀──────────────────  "2"  GPIO2 (TX)
//!   "G"  GND        ───────────────────  "G"  GND
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
pub mod ext_leds;
pub mod fx;
pub mod led;
pub mod link;
pub mod rtt_console;
pub mod settings;
pub mod term;
pub mod trng;

// Hardware-independent logic, tested on the host (`cargo test-host`). Re-exported
// so the rest of the firmware and the node configs keep using `crate::config`, etc.
pub use workshop_core::{baud, burst, config, line, persist, protocol, rtt, seq, units, wire};

use esp_hal::{
    clock::CpuClock,
    gpio::AnyPin,
    rng::Rng,
    time::{Duration, Instant},
};

use crate::{
    baud::{BaudAction, BaudControl, BaudError},
    burst::{BurstRx, BurstTx},
    button::{Button, ButtonUpdate, Source},
    config::{NodeConfig, TxMode, shared},
    ext_leds::{ExtLeds, LedInputs},
    line::{LineBuffer, LineEvent},
    link::Link,
    protocol::{Edge, Frame, MAX_FRAME, Payload},
    rtt::RttTracker,
    seq::SeqCheck,
    term::Stats,
    wire::{DatClock, DatDelta, Dir},
};

/// Expands to a complete node firmware: compile-time config check, app
/// descriptor, panic handler and `main`. A binary is then just its config:
///
/// ```ignore
/// const CONFIG: NodeConfig = NodeConfig { name: "node-A", ..NodeConfig::DEFAULT };
/// esp32_rust_workshop::node_firmware!(CONFIG);
/// ```
// --8<-- [start:macro]
#[macro_export]
macro_rules! node_firmware {
    ($config:path) => {
        use esp_backtrace as _;

        const _: () = $config.check();
        // This firmware is the UART one: an Ethernet config can't be built into it.
        const _: $crate::config::UartLink = $config.uart();

        // App descriptor required by the ESP-IDF second-stage bootloader.
        esp_bootloader_esp_idf::esp_app_desc!();

        #[esp_hal::main]
        fn main() -> ! {
            $crate::run(&$config)
        }
    };
}
// --8<-- [end:macro]

/// After switching speed on the peer's request, don't transmit for this long, so
/// the peer has switched too before our first frame at the new speed.
const SWITCH_HOLD_US: u64 = 50_000;
/// A back-to-back burst sends for at most this long per loop iteration, so the
/// CLI, the LEDs and the receive side keep running during long bursts.
const BURST_SLICE_US: u64 = 2_000;
/// With the `watchdog` feature: the chip resets if the main loop doesn't come
/// back for this long (its longest normal iteration, a `save`, takes ~50 ms).
pub const WATCHDOG_MS: u64 = 2_000;

// --8<-- [start:watchdog]
/// Hardware watchdog on TIMG0, fed once per main loop iteration (`watchdog`
/// feature). Without the feature, feeding does nothing.
struct Watchdog {
    #[cfg(feature = "watchdog")]
    wdt: esp_hal::timer::timg::Wdt<esp_hal::peripherals::TIMG0<'static>>,
}

impl Watchdog {
    #[cfg_attr(not(feature = "watchdog"), allow(unused_variables))]
    fn start(timg0: esp_hal::peripherals::TIMG0<'static>) -> Self {
        #[cfg(feature = "watchdog")]
        {
            use esp_hal::timer::timg::{MwdtStage, TimerGroup};
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
// --8<-- [end:watchdog]

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

// --8<-- [start:pin]
/// Takes a GPIO by number, as given in the config.
fn pin(n: u8) -> AnyPin<'static> {
    // SAFETY: `NodeConfig::check` guarantees at compile time that every configured
    // pin is distinct and not used elsewhere by the firmware (RESERVED_PINS).
    unsafe { AnyPin::steal(n) }
}
// --8<-- [end:pin]

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
/// `defaults` is the compiled-in config. The firmware works on a copy that the
/// CLI can change (`set`, `reset`); `defaults` is kept to show and restore it.
// --8<-- [start:run]
pub fn run(defaults: &'static NodeConfig) -> ! {
    let mut cfg = *defaults;
    // Before any output, so the boot card also goes to RTT (feature `rtt`).
    rtt_console::init();
    rtt_console::set_route(cfg.console);
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // Settings saved with `save` (feature `persist`), on top of the build config.
    let mut store = settings::Store::new(peripherals.FLASH);
    let boot_load = store.load(&mut cfg);
    rtt_console::set_route(cfg.console);
    let p = cfg.pins;
    let uart = cfg.uart();

    let link = Link::new(peripherals.UART1, pin(uart.tx), pin(uart.rx), shared::BAUD_RATE);
    // One GPIO interrupt handler for both buttons (src/button.rs).
    let gpio = button::Gpio::new(peripherals.IO_MUX);
    let button = Button::new(&gpio, Source::Jumper, pin(p.button), ms(cfg.debounce_ms), ms(cfg.lockout_ms));
    // BOOT button, active low on GPIO61 (board pull-up): same filter, same EVT frames.
    let boot = Button::new(&gpio, Source::Boot, peripherals.GPIO61, ms(cfg.debounce_ms), ms(cfg.lockout_ms));
    let leds = ExtLeds::new([(pin(p.led1), p.led1), (pin(p.led2), p.led2)], [cfg.led1, cfg.led2]);
    let rgb_led = led::Led::new(peripherals.RMT, peripherals.GPIO60);

    // esp-hal doesn't start the TRNG on the S31: without this, every key is identical.
    trng::enable();
    let rng = Rng::new();
    let fx = fx::Animator::new(fx_settings(&cfg.rgb));

    term::boot_card(&cfg, &chip_info(&rng), &system_info(&cfg, defaults, boot_load));
    let cli = cli::Cli::new(peripherals.USB_DEVICE);
    // --8<-- [end:run]

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
        rx_lines: LineBuffer::new(),
        last_data: Instant::now(),
        dat_clock: DatClock::new(),
        last_status: Instant::now(),
        baud: BaudControl::new(shared::BAUD_RATE),
        tx_hold_until_us: 0,
        rtt: RttTracker::new(),
        burst_id: 0,
        burst_tx: None,
        burst_rx: None,
        store,
        boot_load,
        _gpio: gpio,
        boot,
        // --8<-- [start:loop]
        // Last, so the boot itself is never cut short.
        watchdog: Watchdog::start(peripherals.TIMG0),
    };
    loop {
        node.step();
    }
}
// --8<-- [end:loop]

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
    /// Next sequence number (DAT, EVT, BAU, ECH).
    seq: u32,
    events_sent: u32,
    rx_lines: LineBuffer<MAX_FRAME>,
    last_data: Instant,
    /// Time between consecutive DAT frames, sent or received (wire timing).
    dat_clock: DatClock,
    last_status: Instant,
    baud: BaudControl,
    /// No transmission before this time (just switched speed on the peer's request).
    tx_hold_until_us: u64,
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
}

impl Node {
    // --8<-- [start:step]
    /// One iteration of the main loop.
    fn step(&mut self) {
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
            if self.cfg.tx_mode == TxMode::Periodic && now_us() >= self.tx_hold_until_us {
                let period_us = units::ms_to_us(self.cfg.data_period_ms) as u32;
                let payload = Payload::Data { pressed: self.pressed(), events: self.events_sent, period_us };
                self.send(payload, None);
            }
        }

        self.burst_tx_step();
        self.receive();
        self.timers();
        // --8<-- [end:step]

        // --- Status line ----------------------------------------------------------
        if self.last_status.elapsed() >= ms_f(self.cfg.status_period_ms) {
            self.last_status = Instant::now();
            term::status(now_ms(), &self.stats, self.peer.link_up(&self.cfg), self.baud.current(), self.rtt.stats());
            if self.cfg.verbose {
                term::link_counters(&self.link.counters(), self.peer.period_us);
            }
        }
    }

    /// Sends a frame with the next sequence number, and prints it (with, for a DAT,
    /// the time since the previous DAT, sent or received).
    /// Either button held down: what `DAT` frames and the `button` LED mode show.
    fn pressed(&self) -> bool {
        self.button.is_pressed() || self.boot.is_pressed()
    }

    /// `button`: which button an `EVT` comes from, and its raw interrupt count.
    fn send(&mut self, payload: Payload, button: Option<(&'static str, u32)>) {
        let frame = Frame { node: self.cfg.name, seq: self.seq, t_ms: now_ms() as u32, payload };
        let line = protocol::encode(&frame);
        match self.link.send(line.as_bytes()) {
            Ok(wire_end_us) => {
                self.stats.tx += 1;
                self.leds.on_tx();
                // Both timed from when the frame ends on the wire, like received ones.
                self.rtt.sent(self.seq, wire_end_us);
                let delta = self.dat_mark(&payload, Dir::Tx, wire_end_us);
                term::sent(now_ms(), &frame, button, delta);
                if self.cfg.verbose {
                    term::wire(line.as_bytes(), None);
                }
            }
            Err(_) => term::uart_error(now_ms(), "TX error"),
        }
        self.seq = self.seq.wrapping_add(1);
    }

    /// For a DAT frame, the time since the previous DAT (either direction).
    fn dat_mark(&mut self, payload: &Payload, dir: Dir, wire_end_us: u64) -> Option<DatDelta> {
        matches!(payload, Payload::Data { .. }).then(|| self.dat_clock.mark(dir, wire_end_us)).flatten()
    }

    // --- Receive ------------------------------------------------------------------

    // --8<-- [start:receive]
    fn receive(&mut self) {
        if let Some(e) = self.link.take_error() {
            self.stats.bad += 1;
            match self.active_burst_rx() {
                Some(rx) => rx.bad += 1, // counted in the burst summary, not printed
                None => term::uart_error(
                    now_ms(),
                    match e {
                        esp_hal::uart::RxError::FifoOverflowed => "RX FIFO overflow",
                        esp_hal::uart::RxError::GlitchOccurred => "glitch on RX line",
                        esp_hal::uart::RxError::FrameFormatViolated => {
                            "framing error (baud rate? GND? wires too long?)"
                        }
                        esp_hal::uart::RxError::ParityMismatch => "parity error",
                        _ => "RX error",
                    },
                ),
            }
        }
        if self.link.take_dropped() > 0 {
            self.stats.bad += 1;
            term::uart_error(now_ms(), "RX buffer full, bytes dropped");
        }
        let mut chunk = [0u8; 64];
        loop {
            let n = self.link.read(&mut chunk);
            if n == 0 {
                break;
            }
            for &byte in &chunk[..n] {
                // Copy the line out: handling it may switch speed and reset `rx_lines`.
                let line: Option<heapless::Vec<u8, MAX_FRAME>> = match self.rx_lines.push(byte) {
                    Some(LineEvent::Line(l)) => heapless::Vec::from_slice(l).ok(),
                    Some(LineEvent::TooLong) => {
                        let _ = self.link.take_line_end(); // keep line ends in step
                        self.stats.bad += 1;
                        term::uart_error(now_ms(), "line too long, dropped");
                        None
                    }
                    None => None,
                };
                if let Some(line) = line {
                    // When this line's `\n` was on the wire (noted by the UART interrupt).
                    let wire_end_us = self.link.take_line_end().unwrap_or_else(now_us);
                    self.handle_line(&line, wire_end_us);
                }
            }
        }
    }
    // --8<-- [end:receive]

    /// The burst being received, if frames still arrive.
    fn active_burst_rx(&mut self) -> Option<&mut BurstRx> {
        let now = now_us();
        self.burst_rx.as_mut().filter(|rx| !rx.is_stale(now))
    }

    fn handle_line(&mut self, line: &[u8], wire_end_us: u64) {
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
            self.baud_action(&frame.payload, now);
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
            Payload::Echo { echo_seq } => rtt = self.rtt.echo(echo_seq, wire_end_us),
            Payload::Baud { .. } | Payload::Burst { .. } => {}
        }
        let delta = self.dat_mark(&frame.payload, Dir::Rx, wire_end_us);
        term::received(now_ms(), &frame, rtt, delta);
        if self.cfg.verbose {
            term::wire(line, since_previous);
        }

        let reply = matches!(frame.payload, Payload::Data { .. } | Payload::Event { .. });
        let seq = frame.seq;
        let payload = frame.payload;
        self.baud_action(&payload, now);
        if reply && self.cfg.tx_mode == TxMode::OnRx && now_us() >= self.tx_hold_until_us {
            self.send(Payload::Echo { echo_seq: seq }, None);
        }
    }

    // --- Link speed -------------------------------------------------------------

    fn set_baud(&mut self, baud: u32) {
        if self.link.set_baud(baud).is_err() {
            term::uart_error(now_ms(), "baud rate not supported by the UART");
        }
        // A line started at the old speed is garbage now.
        self.rx_lines = LineBuffer::new();
    }

    fn baud_action(&mut self, payload: &Payload, now: u64) {
        match self.baud.on_frame(payload, now) {
            Some(BaudAction::AckThenSwitch(baud)) => {
                self.send(Payload::Baud { baud, ack: true }, None);
                self.set_baud(baud); // waits for the ACK to leave first
                self.tx_hold_until_us = now_us() + SWITCH_HOLD_US;
                term::link_event(
                    now_ms(),
                    format_args!("peer asked for {}: acknowledged and switched", term::Baud(baud)),
                );
            }
            Some(BaudAction::Switch(baud)) => {
                self.set_baud(baud);
                term::link_event(now_ms(), format_args!("peer acknowledged: switched to {}", term::Baud(baud)));
            }
            Some(BaudAction::Confirmed(baud)) => {
                term::link_event(now_ms(), format_args!("✓ frames arrive at {}: switch confirmed", term::Baud(baud)));
            }
            _ => {}
        }
    }

    fn timers(&mut self) {
        let now = now_us();
        match self.baud.tick(now, self.peer.timeout_us(&self.cfg)) {
            Some(BaudAction::NoAck(baud)) => term::link_event(
                now_ms(),
                format_args!(
                    "✗ no answer to the switch to {}: staying at {}",
                    term::Baud(baud),
                    term::Baud(self.baud.current())
                ),
            ),
            Some(BaudAction::Revert { from, to }) => {
                self.set_baud(to);
                term::link_event(
                    now_ms(),
                    format_args!("✗ no valid frame at {}: back to {}", term::Baud(from), term::Baud(to)),
                );
            }
            _ => {}
        }
        if self.burst_rx.as_ref().is_some_and(|rx| rx.is_stale(now)) {
            self.finish_burst_rx();
        }
    }

    // --- Bursts -------------------------------------------------------------------

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
        while now_us() < slice_end {
            let Some(index) = tx.next(now_us()) else { break };
            let frame = Frame {
                node: self.cfg.name,
                seq: tx.id,
                t_ms: now_ms() as u32,
                payload: Payload::Burst { index, count: tx.count },
            };
            let line = protocol::encode(&frame);
            if self.link.send(line.as_bytes()).is_err() {
                term::uart_error(now_ms(), "TX error during burst");
                break;
            }
            tx.sent(line.len());
            self.leds.on_tx();
        }
        if tx.is_done() {
            let (id, frames, throughput) = (tx.id, tx.sent, tx.throughput());
            self.burst_tx = None;
            term::burst_sent(now_ms(), id, frames, &throughput, self.baud.current());
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
            term::burst_received(now_ms(), &rx.node, rx.id, &rx.summary(), self.baud.current());
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
                    // Confirm on both sides before switching, so the console being
                    // left knows where the output went.
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
            Err(SetError::ReadOnly) => term::cli_error(format_args!(
                "`{key}` is read-only: change it in presets/uart/*.toml (or config::shared) and rebuild"
            )),
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
            Command::Link => term::cli_error(format_args!(
                "`link` is for the Ethernet app: this firmware has a UART link (see `baud`)"
            )),
            Command::Baud(None) => term::baud_info(self.baud.current(), self.baud.boot()),
            Command::Baud(Some(baud)) => match self.baud.request(baud, now_us()) {
                Ok(()) => {
                    term::link_event(now_ms(), format_args!("asking the peer to switch to {}…", term::Baud(baud)));
                    self.send(Payload::Baud { baud, ack: false }, None);
                }
                Err(BaudError::Same) => term::cli_error(format_args!("the link is already at {}", term::Baud(baud))),
                Err(BaudError::Busy) => term::cli_error(format_args!("a speed change is already in progress")),
            },
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
    let mut mac = [0u8; 6];
    mac.copy_from_slice(esp_hal::efuse::base_mac_address().as_bytes());
    term::ChipInfo {
        rev_major: rev.major,
        rev_minor: rev.minor,
        cpu_mhz: esp_hal::clock::cpu_clock().as_mhz(),
        mac,
        reset,
        rng_samples: [rng.random(), rng.random(), rng.random()],
    }
}
