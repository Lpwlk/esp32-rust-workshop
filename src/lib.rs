//! Shared firmware for two ESP32-S31-Function-CoreBoard-1 boards linked by UART.
//!
//! Each board:
//! - sends a `DAT` frame (counter, timestamp, button state) every 500 ms,
//! - sends an `EVT` frame with a random 64-bit key on each rising/falling edge
//!   of a jumper "button" on GPIO4 (GPIO interrupt on both edges),
//! - decodes and displays everything it receives from the other board.
//!
//! Frame format: see [`protocol`]. UART0 (GPIO58/59) is wired to the on-board
//! USB-to-UART bridge, so the link uses UART1 on header J2:
//!
//! ```text
//!   Board A                              Board B
//!   J2 "2"  GPIO2 (TX) ───────────────▶  J2 "3"  GPIO3 (RX)
//!   J2 "3"  GPIO3 (RX) ◀───────────────  J2 "2"  GPIO2 (TX)
//!   J2 "G"  GND        ────────────────  J2 "G"  GND
//!
//!   Button (each board): jumper between J2 "4" (GPIO4, pin 32) and J2 "G" (pin 33).
//!   Plugged = pressed (falling edge), removed = released (rising edge).
//! ```
#![no_std]

pub mod button;
pub mod cli;
pub mod fx;
pub mod led;
pub mod protocol;
pub mod term;
mod trng;

use esp_hal::{
    Blocking,
    clock::CpuClock,
    rng::Rng,
    time::{Duration, Instant},
    uart::{Config, Uart},
};
use heapless::Vec;

use crate::{
    button::Button,
    protocol::{Frame, MAX_FRAME, Payload},
    term::Stats,
};

/// Must be identical on both boards.
pub const BAUD_RATE: u32 = 115_200;

const DATA_PERIOD: Duration = Duration::from_millis(2500);
const STATUS_PERIOD: Duration = Duration::from_secs(10);
/// No valid frame for 3 data periods means the link is down.
const LINK_TIMEOUT: Duration = Duration::from_millis(DATA_PERIOD.as_millis() * 3);

fn now_ms() -> u64 {
    Instant::now().duration_since_epoch().as_millis()
}

/// LED settings at boot; all of them can be changed at runtime from the CLI.
const LED_FREQ_HZ: u32 = 50;
const LED_CYCLE_MS: u32 = 6000;
const LED_BRIGHTNESS: u8 = 40;

/// Entry point shared by both binaries. `node` identifies this board in frames,
/// `pattern` is the RGB LED pattern at boot (see [`fx::PATTERNS`]).
pub fn run(node: &str, pattern: &str) -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    let mut uart = Uart::new(peripherals.UART1, Config::default().with_baudrate(BAUD_RATE))
        .expect("invalid UART1 configuration")
        .with_tx(peripherals.GPIO2)
        .with_rx(peripherals.GPIO3);
    let mut button = Button::new(peripherals.IO_MUX, peripherals.GPIO4);
    let mut rgb_led = led::Led::new(peripherals.RMT, peripherals.GPIO60);
    // esp-hal doesn't start the TRNG on the S31: without this, every key is identical.
    trng::enable();
    let rng = Rng::new();
    let mut fx = fx::Animator::new(
        fx::Settings {
            pattern: fx::find(pattern).unwrap_or(0),
            freq_hz: LED_FREQ_HZ,
            cycle_ms: LED_CYCLE_MS,
            brightness: LED_BRIGHTNESS,
        },
        rng.random(),
    );

    term::banner(node, BAUD_RATE, DATA_PERIOD.as_millis());
    term::build_card(&chip_info(&rng, fx.settings));
    term::cli_hint();
    let mut cli = cli::Cli::new(peripherals.USB_DEVICE);

    let mut stats = Stats::default();
    let mut seq: u32 = 0;
    let mut events_sent: u32 = 0;
    let mut peer_seq: Option<u32> = None;
    let mut last_rx: Option<Instant> = None;
    let mut rx_line: Vec<u8, MAX_FRAME> = Vec::new();
    let mut rx_overflow = false;
    let mut last_data = Instant::now();
    let mut last_status = Instant::now();

    loop {
        // --- RGB LED and host CLI -------------------------------------------
        fx.tick(&mut rgb_led);
        if let Some(cmd) = cli.poll() {
            apply(cmd, &mut fx.settings);
        }

        // --- Button edge -> EVT frame -------------------------------------
        if let Some(ev) = button.poll() {
            let key = (u64::from(rng.random()) << 32) | u64::from(rng.random());
            let frame = Frame { node, seq, t_ms: now_ms() as u32, payload: Payload::Event { edge: ev.edge, key } };
            if send(&mut uart, &frame, &mut stats) {
                term::sent(now_ms(), &frame, Some(ev.raw_edges));
            }
            seq = seq.wrapping_add(1);
            events_sent += 1;
        }

        // --- Periodic DAT frame ---------------------------------------------
        if last_data.elapsed() >= DATA_PERIOD {
            last_data += DATA_PERIOD;
            let frame = Frame {
                node,
                seq,
                t_ms: now_ms() as u32,
                payload: Payload::Data { pressed: button.is_pressed(), events: events_sent },
            };
            if send(&mut uart, &frame, &mut stats) {
                term::sent(now_ms(), &frame, None);
            }
            seq = seq.wrapping_add(1);
        }

        // --- Receive (non-blocking) -----------------------------------------
        while uart.read_ready() {
            let mut chunk = [0u8; 64];
            let n = match uart.read(&mut chunk) {
                Ok(n) => n,
                Err(e) => {
                    term::uart_error(now_ms(), match e {
                        esp_hal::uart::RxError::FifoOverflowed => "RX FIFO overflow",
                        esp_hal::uart::RxError::GlitchOccurred => "glitch on RX line",
                        esp_hal::uart::RxError::FrameFormatViolated => "framing error (baud rate? GND?)",
                        esp_hal::uart::RxError::ParityMismatch => "parity error",
                        _ => "RX error",
                    });
                    stats.bad += 1;
                    rx_line.clear();
                    rx_overflow = false;
                    break;
                }
            };

            for &byte in &chunk[..n] {
                if byte != b'\n' {
                    rx_overflow |= rx_line.push(byte).is_err();
                    continue;
                }
                if rx_overflow {
                    stats.bad += 1;
                    term::uart_error(now_ms(), "line too long, dropped");
                } else {
                    handle_line(&rx_line, &mut stats, &mut peer_seq, &mut last_rx);
                }
                rx_line.clear();
                rx_overflow = false;
            }
        }

        // --- Status line ---------------------------------------------------
        if last_status.elapsed() >= STATUS_PERIOD {
            last_status += STATUS_PERIOD;
            let link_up = last_rx.is_some_and(|t| t.elapsed() < LINK_TIMEOUT);
            term::status(now_ms(), &stats, link_up);
        }
    }
}

/// Applies a CLI command to the LED settings and reports the result.
fn apply(cmd: cli::Command, led: &mut fx::Settings) {
    use cli::Command;
    match cmd {
        Command::Help => term::cli_help(),
        Command::List => term::cli_list(led.pattern),
        Command::Status => term::cli_status(led),
        Command::Pattern(i) => {
            led.pattern = i;
            term::cli_status(led);
        }
        Command::NextPattern => {
            led.pattern = (led.pattern + 1) % fx::PATTERNS.len();
            term::cli_status(led);
        }
        Command::Freq(hz) => {
            led.freq_hz = hz;
            term::cli_ok(format_args!("LED refresh rate: {hz} Hz"));
        }
        Command::Cycle(ms) => {
            led.cycle_ms = ms;
            term::cli_ok(format_args!("fade cycle: {ms} ms"));
        }
        Command::Bright(pct) => {
            led.brightness = pct;
            term::cli_ok(format_args!("brightness: {pct}%"));
        }
    }
}

fn chip_info(rng: &Rng, led: fx::Settings) -> term::ChipInfo {
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
        led,
    }
}

fn handle_line(line: &[u8], stats: &mut Stats, peer_seq: &mut Option<u32>, last_rx: &mut Option<Instant>) {
    let parsed = core::str::from_utf8(line)
        .map_err(|_| protocol::ParseError::Malformed)
        .and_then(protocol::parse);
    let frame = match parsed {
        Ok(frame) => frame,
        Err(e) => {
            stats.bad += 1;
            term::rx_error(now_ms(), e, line);
            return;
        }
    };

    stats.rx += 1;
    *last_rx = Some(Instant::now());
    match *peer_seq {
        Some(prev) if frame.seq > prev.wrapping_add(1) => {
            let missing = frame.seq - prev - 1;
            stats.lost += missing;
            term::lost(now_ms(), missing);
        }
        Some(prev) if frame.seq <= prev => term::peer_restarted(now_ms(), frame.node),
        _ => {}
    }
    *peer_seq = Some(frame.seq);
    term::received(now_ms(), &frame);
}

/// Sends one frame; `Uart::write` may accept only part of the buffer when the TX FIFO is full.
fn send(uart: &mut Uart<'_, Blocking>, frame: &Frame<'_>, stats: &mut Stats) -> bool {
    let line = protocol::encode(frame);
    let mut data = line.as_bytes();
    while !data.is_empty() {
        match uart.write(data) {
            Ok(n) => data = &data[n..],
            Err(_) => {
                term::uart_error(now_ms(), "TX error");
                return false;
            }
        }
    }
    stats.tx += 1;
    true
}
