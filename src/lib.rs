//! Shared firmware for two ESP32-S31-Function-CoreBoard-1 boards linked by UART.
//!
//! Each board:
//! - sends a `DAT` frame (counter, timestamp, button state, period) periodically,
//! - sends an `EVT` frame with a random 64-bit key on each rising/falling edge
//!   of a jumper "button" (GPIO interrupt on both edges),
//! - decodes and displays everything it receives from the other board,
//! - drives the on-board RGB LED and two LEDs on the breadboard,
//! - offers a small CLI on the USB console.
//!
//! **Configuration**: see [`config`]. Wire-level settings shared by both nodes are
//! in [`config::shared`]; everything else is per node, in `src/bin/uart_node_*.rs`.
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

pub mod button;
pub mod cli;
pub mod config;
pub mod ext_leds;
pub mod fx;
pub mod led;
pub mod link;
pub mod protocol;
pub mod term;
mod trng;

use esp_hal::{
    clock::CpuClock,
    gpio::AnyPin,
    rng::Rng,
    time::{Duration, Instant},
};
use heapless::Vec;

use crate::{
    button::{Button, ButtonUpdate},
    config::{NodeConfig, shared},
    ext_leds::{ExtLeds, LedInputs},
    link::Link,
    protocol::{Edge, Frame, MAX_FRAME, Payload},
    term::Stats,
};

/// Expands to a complete node firmware: compile-time config check, app
/// descriptor, panic handler and `main`. A binary is then just its config:
///
/// ```ignore
/// const CONFIG: NodeConfig = NodeConfig { name: "node-A", ..NodeConfig::DEFAULT };
/// esp32_rust_workshop::node_firmware!(CONFIG);
/// ```
#[macro_export]
macro_rules! node_firmware {
    ($config:path) => {
        use esp_backtrace as _;

        const _: () = $config.check();

        // App descriptor required by the ESP-IDF second-stage bootloader.
        esp_bootloader_esp_idf::esp_app_desc!();

        #[esp_hal::main]
        fn main() -> ! {
            $crate::run(&$config)
        }
    };
}

fn now_ms() -> u64 {
    Instant::now().duration_since_epoch().as_millis()
}

/// Takes a GPIO by number, as given in the config.
fn pin(n: u8) -> AnyPin<'static> {
    // SAFETY: `NodeConfig::check` guarantees at compile time that every configured
    // pin is distinct and not used elsewhere by the firmware (RESERVED_PINS).
    unsafe { AnyPin::steal(n) }
}

/// What we know about the other node, from its frames.
#[derive(Default)]
struct Peer {
    seq: Option<u32>,
    last_rx: Option<Instant>,
    /// Its data period, announced in its DAT frames.
    period_ms: Option<u32>,
    button: bool,
}

impl Peer {
    fn link_up(&self, cfg: &NodeConfig) -> bool {
        let period = self.period_ms.unwrap_or(cfg.data_period_ms);
        let timeout = Duration::from_millis(u64::from(period) * u64::from(cfg.link_timeout_periods));
        self.last_rx.is_some_and(|t| t.elapsed() < timeout)
    }
}

fn fx_settings(rgb: &config::RgbConfig) -> fx::Settings {
    fx::Settings {
        pattern: fx::find(rgb.pattern).unwrap_or(0),
        freq_hz: rgb.freq_hz,
        cycle_ms: rgb.cycle_ms,
        brightness: rgb.brightness,
    }
}

fn ms(v: u32) -> Duration {
    Duration::from_millis(u64::from(v))
}

/// Entry point shared by both binaries (see [`node_firmware!`]).
///
/// `defaults` is the compiled-in config. The firmware works on a copy that the
/// CLI can change (`set`, `reset`); `defaults` is kept to show and restore it.
pub fn run(defaults: &'static NodeConfig) -> ! {
    let mut cfg = *defaults;
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let p = cfg.pins;

    let mut link = Link::new(peripherals.UART1, pin(p.uart_tx), pin(p.uart_rx), shared::BAUD_RATE);
    let mut button = Button::new(peripherals.IO_MUX, pin(p.button), ms(cfg.debounce_ms), ms(cfg.lockout_ms));
    let mut leds = ExtLeds::new([(pin(p.led1), p.led1), (pin(p.led2), p.led2)], [cfg.led1, cfg.led2]);
    let mut rgb_led = led::Led::new(peripherals.RMT, peripherals.GPIO60);
    // esp-hal doesn't start the TRNG on the S31: without this, every key is identical.
    trng::enable();
    let rng = Rng::new();
    let mut fx = fx::Animator::new(fx_settings(&cfg.rgb));

    term::boot_card(&cfg, &chip_info(&rng));
    let mut cli = cli::Cli::new(peripherals.USB_DEVICE);

    let mut stats = Stats::default();
    let mut peer = Peer::default();
    let mut seq: u32 = 0;
    let mut events_sent: u32 = 0;
    let mut rx_line: Vec<u8, MAX_FRAME> = Vec::new();
    let mut rx_overflow = false;
    let mut last_data = Instant::now();
    let mut last_status = Instant::now();

    loop {
        // --- LEDs and host CLI ------------------------------------------------
        fx.tick(&mut rgb_led);
        leds.update(&LedInputs { button: button.is_pressed(), peer_button: peer.button, link_up: peer.link_up(&cfg) });
        if let Some(cmd) = cli.poll() {
            apply(cmd, &mut cfg, defaults, &mut fx, &mut leds, &mut button);
        }

        // --- Button edge -> EVT frame -------------------------------------------
        match button.poll() {
            Some(ButtonUpdate::Edge(ev)) => {
                let key = (u64::from(rng.random()) << 32) | u64::from(rng.random());
                let frame =
                    Frame { node: cfg.name, seq, t_ms: now_ms() as u32, payload: Payload::Event { edge: ev.edge, key } };
                if let Some(line) = send(&mut link, &frame, &mut stats, &mut leds) {
                    term::sent(now_ms(), &frame, Some(ev.raw_edges));
                    if cfg.verbose {
                        term::wire(line.as_bytes(), None);
                    }
                }
                seq = seq.wrapping_add(1);
                events_sent += 1;
            }
            Some(ButtonUpdate::Ignored { raw_edges }) if cfg.verbose => term::bounce_ignored(now_ms(), raw_edges),
            Some(ButtonUpdate::Ignored { .. }) => {}
            None => {}
        }

        // --- Periodic DAT frame -------------------------------------------------
        // Periods are read from `cfg` every time, so CLI changes apply immediately.
        if last_data.elapsed() >= ms(cfg.data_period_ms) {
            last_data = Instant::now();
            let frame = Frame {
                node: cfg.name,
                seq,
                t_ms: now_ms() as u32,
                payload: Payload::Data {
                    pressed: button.is_pressed(),
                    events: events_sent,
                    period_ms: cfg.data_period_ms,
                },
            };
            if let Some(line) = send(&mut link, &frame, &mut stats, &mut leds) {
                term::sent(now_ms(), &frame, None);
                if cfg.verbose {
                    term::wire(line.as_bytes(), None);
                }
            }
            seq = seq.wrapping_add(1);
        }

        // --- Receive (bytes buffered by the UART interrupt) ----------------------
        if let Some(e) = link.take_error() {
            term::uart_error(now_ms(), match e {
                esp_hal::uart::RxError::FifoOverflowed => "RX FIFO overflow",
                esp_hal::uart::RxError::GlitchOccurred => "glitch on RX line",
                esp_hal::uart::RxError::FrameFormatViolated => "framing error (baud rate? GND? wires too long?)",
                esp_hal::uart::RxError::ParityMismatch => "parity error",
                _ => "RX error",
            });
            stats.bad += 1;
        }
        if link.take_dropped() > 0 {
            stats.bad += 1;
            term::uart_error(now_ms(), "RX buffer full, bytes dropped");
        }
        let mut chunk = [0u8; 64];
        loop {
            let n = link.read(&mut chunk);
            if n == 0 {
                break;
            }
            for &byte in &chunk[..n] {
                if byte != b'\n' {
                    rx_overflow |= rx_line.push(byte).is_err();
                    continue;
                }
                if rx_overflow {
                    stats.bad += 1;
                    term::uart_error(now_ms(), "line too long, dropped");
                } else if handle_line(&rx_line, &mut stats, &mut peer, cfg.verbose) {
                    leds.on_rx();
                }
                rx_line.clear();
                rx_overflow = false;
            }
        }

        // --- Status line ----------------------------------------------------------
        if last_status.elapsed() >= ms(cfg.status_period_ms) {
            last_status = Instant::now();
            term::status(now_ms(), &stats, peer.link_up(&cfg));
            if cfg.verbose {
                term::link_counters(&link.counters(), peer.period_ms);
            }
        }
    }
}

/// Pushes the (possibly changed) config to the parts that keep their own copy.
fn sync(cfg: &NodeConfig, fx: &mut fx::Animator, leds: &mut ExtLeds, button: &mut Button) {
    fx.settings = fx_settings(&cfg.rgb);
    leds.modes = [cfg.led1, cfg.led2];
    button.set_timing(ms(cfg.debounce_ms), ms(cfg.lockout_ms));
}

/// `set <key> <value>`: validates, applies, and reports the result.
fn set_key(cfg: &mut NodeConfig, key: &str, value: &str, fx: &mut fx::Animator, leds: &mut ExtLeds, button: &mut Button) {
    use config::{Key, SetError};
    match cfg.set(key, value) {
        Ok(k) => {
            sync(cfg, fx, leds, button);
            term::cli_ok(format_args!("{} = {}", k.name(), cfg.value(k)));
            if k == Key::RgbPattern {
                term::cli_status(&fx.settings, leds);
            }
        }
        Err(SetError::UnknownKey) => term::cli_error(format_args!("unknown key `{key}` (see `config`)")),
        Err(SetError::ReadOnly) => term::cli_error(format_args!(
            "`{key}` is read-only: change it in src/bin/uart_node_*.rs (or config::shared) and rebuild"
        )),
        Err(SetError::BadValue(expected)) => term::cli_error(format_args!("`{key}`: expected {expected}")),
        Err(SetError::Invalid(msg)) => term::cli_error(format_args!("{}", msg.trim_start_matches("config: "))),
    }
}

/// Applies a CLI command and reports the result.
fn apply(
    cmd: cli::Command,
    cfg: &mut NodeConfig,
    defaults: &NodeConfig,
    fx: &mut fx::Animator,
    leds: &mut ExtLeds,
    button: &mut Button,
) {
    use cli::Command;
    use config::Key;

    match cmd {
        Command::Help(topic) => term::cli_help(topic.as_deref(), cfg, defaults),
        Command::List => term::cli_list(fx.settings.pattern),
        Command::Status => term::cli_status(&fx.settings, leds),
        Command::Config => term::cli_config(cfg, defaults),
        Command::Set { key, value } => set_key(cfg, &key, &value, fx, leds, button),
        Command::NextPattern => {
            let next = (fx.settings.pattern + 1) % fx::PATTERNS.len();
            set_key(cfg, "rgb.pattern", fx::PATTERNS[next].name, fx, leds, button);
        }
        Command::Reset(None) => {
            *cfg = *defaults;
            sync(cfg, fx, leds, button);
            term::cli_ok(format_args!("all settings back to their build values"));
        }
        Command::Reset(Some(key)) => match Key::from_name(&key) {
            Some(k) => {
                cfg.reset(k, defaults);
                sync(cfg, fx, leds, button);
                term::cli_ok(format_args!("{} = {}  (build value)", k.name(), cfg.value(k)));
            }
            None if config::READ_ONLY.contains(&key.as_str()) => {
                term::cli_error(format_args!("`{key}` is read-only, it always has its build value"))
            }
            None => term::cli_error(format_args!("unknown key `{key}` (see `config`)")),
        },
    }
}

fn chip_info(rng: &Rng) -> term::ChipInfo {
    use core::fmt::Write as _;
    let rev = esp_hal::efuse::chip_revision();
    let mut reset = heapless::String::new();
    let _ = match esp_hal::system::reset_reason() {
        Some(reason) => write!(reset, "{reason:?}"),
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

/// Decodes one received line. Returns whether it was a valid frame.
fn handle_line(line: &[u8], stats: &mut Stats, peer: &mut Peer, verbose: bool) -> bool {
    let parsed = core::str::from_utf8(line)
        .map_err(|_| protocol::ParseError::Malformed)
        .and_then(protocol::parse);
    let frame = match parsed {
        Ok(frame) => frame,
        Err(e) => {
            stats.bad += 1;
            term::rx_error(now_ms(), e, line);
            return false;
        }
    };

    stats.rx += 1;
    let since_previous = peer.last_rx.map(|t| t.elapsed().as_millis());
    peer.last_rx = Some(Instant::now());
    match peer.seq {
        Some(prev) if frame.seq > prev.wrapping_add(1) => {
            let missing = frame.seq - prev - 1;
            stats.lost += missing;
            term::lost(now_ms(), missing);
        }
        Some(prev) if frame.seq <= prev => term::peer_restarted(now_ms(), frame.node),
        _ => {}
    }
    peer.seq = Some(frame.seq);
    match frame.payload {
        Payload::Data { pressed, period_ms, .. } => {
            peer.button = pressed;
            peer.period_ms = Some(period_ms);
        }
        Payload::Event { edge, .. } => peer.button = edge == Edge::Falling,
    }
    term::received(now_ms(), &frame);
    if verbose {
        term::wire(line, since_previous);
    }
    true
}

/// Encodes and sends `frame`; returns the line sent (shown in verbose mode).
fn send(link: &mut Link, frame: &Frame<'_>, stats: &mut Stats, leds: &mut ExtLeds) -> Option<heapless::String<MAX_FRAME>> {
    let line = protocol::encode(frame);
    match link.send(line.as_bytes()) {
        Ok(()) => {
            stats.tx += 1;
            leds.on_tx();
            Some(line)
        }
        Err(_) => {
            term::uart_error(now_ms(), "TX error");
            None
        }
    }
}
