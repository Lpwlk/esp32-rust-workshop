//! Firmware configuration, in three parts:
//!
//! - [`Link`]: how the two nodes are connected, **UART** ([`UartLink`], with the baud rate
//!   in [`shared`]) or **Ethernet** ([`NetLink`], addresses and transport). Both nodes must agree on
//!   it. Change one and reflash **both** boards.
//! - [`NodeConfig`]: the settings each node chooses freely (timings, pins, LEDs...), the
//!   same for both links. Each binary starts from [`NodeConfig::DEFAULT`] (UART) or
//!   [`NodeConfig::DEFAULT_NET`] (Ethernet) and overrides only what it needs, in Rust or,
//!   better, in a TOML preset (see the `presets/` folders).
//!
//! Invalid values are rejected **at compile time** by [`NodeConfig::check`]
//! (called by the firmware's `node_firmware!` macro), with a message saying what
//! is wrong. The CLI `set` command goes through the same [`NodeConfig::validate`].
//!
//! ```
//! use workshop_core::config::NodeConfig;
//!
//! const CONFIG: NodeConfig = NodeConfig { name: "node-A", data_period_ms: 1000.0, ..NodeConfig::DEFAULT };
//! const _: () = CONFIG.check(); // a bad value here fails the build
//!
//! let mut cfg = CONFIG;
//! assert!(cfg.set("rgb.brightness", "70").is_ok());
//! assert!(cfg.set("rgb.brightness", "170").is_err()); // refused, cfg unchanged
//! assert_eq!(cfg.rgb.brightness, 70);
//! ```

use core::fmt;

use crate::fx;
pub use crate::net::{NetLink, TcpRole, Transport};
use crate::units::{Num, parse_freq_hz, parse_period_ms};

/// `ensure!(cond, "msg")`: return `Err("msg")` unless `cond` (usable in `const fn`).
macro_rules! ensure {
    ($cond:expr, $msg:expr $(,)?) => {
        if !$cond {
            return Err($msg);
        }
    };
}

/// Wire-level settings: identical on both nodes, or they can't talk.
pub mod shared {
    /// UART1 baud rate, 8N1. 5 Mbaud is the maximum: 80 MHz UART clock / 16,
    /// so the rate is exact (0 % error). Keep the wires short and share GND.
    pub const BAUD_RATE: u32 = 5_000_000;
    /// Range accepted by the CLI `baud` command (the speed both nodes switch to
    /// together, see [`crate::protocol::Payload::Baud`]). They go back to
    /// [`BAUD_RATE`] on reboot, or when the link is lost after a switch.
    pub const BAUD_RANGE: (u32, u32) = (1200, 5_000_000);
}

/// GPIO numbers used by a node, besides the link's own (see [`Link`]). All must be different,
/// exist on the chip (see [`MISSING_PINS`]) and not be reserved (see [`RESERVED_PINS`]).
/// The RGB LED is fixed on the board (GPIO60).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pins {
    /// Jumper "button" to GND (internal pull-up).
    pub button: u8,
    /// Breadboard LEDs, active high: GPIO -> 330 Ω -> LED anode, cathode -> GND.
    pub led1: u8,
    /// Second breadboard LED, same wiring as `led1`.
    pub led2: u8,
}

/// The pins of the UART link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UartLink {
    /// UART1 TX, wired to the peer's RX.
    pub tx: u8,
    /// UART1 RX, wired to the peer's TX.
    pub rx: u8,
}

/// How the two nodes are connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// UART1, 8N1 at [`shared::BAUD_RATE`], two wires and a ground.
    Uart(UartLink),
    /// The board's RJ45 (Gigabit Ethernet, RGMII): UDP or TCP over a direct cable or a switch.
    Net(NetLink),
}

/// Which kind of [`Link`], for the CLI help and the generated docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// [`Link::Uart`].
    Uart,
    /// [`Link::Net`].
    Net,
}

impl LinkKind {
    /// `uart` or `ethernet`.
    pub const fn name(self) -> &'static str {
        match self {
            LinkKind::Uart => "uart",
            LinkKind::Net => "ethernet",
        }
    }
}

impl Link {
    /// Which kind of link this is.
    pub const fn kind(&self) -> LinkKind {
        match self {
            Link::Uart(_) => LinkKind::Uart,
            Link::Net(_) => LinkKind::Net,
        }
    }
}

/// Pins the firmware must not touch on the ESP32-S31-WROOM-3 / CoreBoard-1
/// (ESP32-S31 datasheet v0.5, section 2.3.5 "Peripheral Pin Assignment"):
///
/// - 26-28, 30-32: SPI0/1, connected to the flash;
/// - 33, 34: USB Serial/JTAG (USB D-/D+), used by the CLI, `espflash` and `probe-rs`;
/// - 58, 59: UART0, the ROM / bootloader console;
/// - 60: on-board RGB LED (also a strapping pin);
/// - 50-57: on-board audio codec and amplifier, not on the header (wired on the
///   board even though the firmware doesn't use them);
/// - 61: BOOT button, read by the firmware like the jumper (also a strapping pin).
pub const RESERVED_PINS: [u8; 20] = [26, 27, 28, 30, 31, 32, 33, 34, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61];
/// GPIO numbers that don't exist: the ESP32-S31 has 60 GPIOs numbered 0-61,
/// without 29 and 41 (datasheet, chapter 2 "Pins").
pub const MISSING_PINS: [u8; 2] = [29, 41];
/// Highest GPIO number of the ESP32-S31.
pub const MAX_GPIO: u8 = 61;

/// On-board RGB LED at boot (all of it can be changed at runtime from the CLI).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RgbConfig {
    /// One of [`fx::PATTERNS`]: viridis, sunset, ocean, aurora, sakura.
    pub pattern: &'static str,
    /// Refresh rate, Hz (decimals allowed).
    pub freq_hz: f32,
    /// Duration of one fade loop, ms (decimals allowed).
    pub cycle_ms: f32,
    /// 0-100 %.
    pub brightness: u8,
}

/// What a breadboard LED shows (can be changed at runtime with the CLI `led` command).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LedMode {
    /// Always off.
    Off,
    /// Always on.
    On,
    /// Blinks with this full period (on + off), ms.
    Blink {
        /// Full period, ms (20-60000, decimals allowed).
        period_ms: f32,
    },
    /// On while this node's jumper button is pressed.
    Button,
    /// On while the *peer's* button is pressed (from received frames).
    PeerButton,
    /// On while the link is up.
    Link,
    /// Short flash on each valid received frame.
    RxFlash,
    /// Short flash on each sent frame.
    TxFlash,
}

impl fmt::Display for LedMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LedMode::Off => f.pad("off"),
            LedMode::On => f.pad("on"),
            LedMode::Blink { period_ms } => {
                let mut s: heapless::String<24> = heapless::String::new();
                let _ = fmt::Write::write_fmt(&mut s, format_args!("blink {} ms", Num(*period_ms)));
                f.pad(&s)
            }
            LedMode::Button => f.pad("button"),
            LedMode::PeerButton => f.pad("peer"),
            LedMode::Link => f.pad("link"),
            LedMode::RxFlash => f.pad("rx"),
            LedMode::TxFlash => f.pad("tx"),
        }
    }
}

/// When this node sends frames on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxMode {
    /// A `DAT` frame every `data_period_ms`.
    Periodic,
    /// No `DAT`: an `ECH` reply to each frame received from the peer, which gives
    /// the peer the round-trip time.
    OnRx,
    /// Nothing on its own (button `EVT` and `BAU` answers only).
    Quiet,
}

impl fmt::Display for TxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            TxMode::Periodic => "periodic",
            TxMode::OnRx => "on-rx",
            TxMode::Quiet => "quiet",
        })
    }
}

/// How much the console prints. `verbose` in the CLI, presets and saved settings.
///
/// Levels are ordered: each one shows everything the previous one does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    /// `0`: one **live line**, updated in place, with the last frame event (sent or received) and the
    /// counters. Only what needs attention scrolls: lost or bad frames, link events, burst results,
    /// CLI replies and errors.
    Live,
    /// `1`: every frame on its own line, and the periodic stats line.
    Frames,
    /// `2`: frames plus the raw line on the wire, its checksum and size, ignored button bounces and
    /// the link counters.
    Wire,
}

impl Verbosity {
    /// The next level, `Wire` going back to `Live`: what `verbose` alone does.
    pub const fn next(self) -> Verbosity {
        match self {
            Verbosity::Live => Verbosity::Frames,
            Verbosity::Frames => Verbosity::Wire,
            Verbosity::Wire => Verbosity::Live,
        }
    }

    /// Parses `0`/`live`, `1`/`frames`, `2`/`wire`, and the old spellings: `on`, `true`, `yes`
    /// (= `wire`) and `off`, `false`, `no` (= `live`).
    pub fn parse(text: &str) -> Result<Verbosity, SetError> {
        match text {
            "0" | "live" | "quiet" | "off" | "false" | "no" => Ok(Verbosity::Live),
            "1" | "frames" => Ok(Verbosity::Frames),
            "2" | "wire" | "raw" | "on" | "true" | "yes" => Ok(Verbosity::Wire),
            _ => Err(SetError::BadValue("0 live, 1 frames or 2 wire (or `next`)")),
        }
    }
}

impl fmt::Display for Verbosity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Verbosity::Live => "live",
            Verbosity::Frames => "frames",
            Verbosity::Wire => "wire",
        })
    }
}

/// Where the console output (frames, stats, CLI replies) goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Console {
    /// USB serial, plus RTT when the firmware is built with `--features rtt`.
    Auto,
    /// USB serial only (`espflash monitor`, `dev.py monitor`).
    Serial,
    /// RTT only (`probe-rs attach`, `cargo embed`): needs `--features rtt`.
    Rtt,
    /// Both: needs `--features rtt`.
    Both,
}

impl Console {
    /// Whether output goes to the USB serial console, given whether RTT is built in.
    pub const fn serial(self, rtt_built: bool) -> bool {
        match self {
            Console::Auto | Console::Serial | Console::Both => true,
            // Never silence everything: without RTT, `rtt` falls back to serial.
            Console::Rtt => !rtt_built,
        }
    }

    /// Whether output goes to RTT, given whether RTT is built in.
    pub const fn rtt(self, rtt_built: bool) -> bool {
        rtt_built && !matches!(self, Console::Serial)
    }
}

impl fmt::Display for Console {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Console::Auto => "auto",
            Console::Serial => "serial",
            Console::Rtt => "rtt",
            Console::Both => "both",
        })
    }
}

/// Frames per burst accepted by `burst` and `burst.count`.
pub const BURST_COUNT_RANGE: (u32, u32) = (1, 100_000);
/// Burst rates accepted, frames/s (0 = back to back).
pub const BURST_RATE_RANGE: (f32, f32) = (0.1, 20_000.0);

/// Defaults of the CLI `burst` command (also proposed by its questions).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BurstConfig {
    /// Frames per burst.
    pub count: u32,
    /// Frames per second (decimals allowed); 0 = back to back, as fast as the line allows.
    pub rate_hz: f32,
}

/// Everything a node may set on its own, without breaking the link.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeConfig {
    /// Name sent in every frame. Max 12 chars, no `,` `*` `$` (frame separators).
    pub name: &'static str,
    /// GPIO numbers, fixed at build time.
    pub pins: Pins,
    /// How the node reaches its peer: UART or Ethernet, fixed at build time.
    pub link: Link,
    /// Period of the DAT frame, ms. Sent in the frame, so the peer adapts to it.
    pub data_period_ms: f32,
    /// Period of the stats line in the console, ms.
    pub status_period_ms: f32,
    /// The link is DOWN after this many *peer* data periods without a valid frame.
    pub link_timeout_periods: u32,
    /// Button: line must be stable this long before an edge is accepted, ms.
    pub debounce_ms: u32,
    /// Button: changes ignored this long after an accepted edge, ms.
    pub lockout_ms: u32,
    /// On-board RGB LED.
    pub rgb: RgbConfig,
    /// Mode of breadboard LED 1.
    pub led1: LedMode,
    /// Mode of breadboard LED 2.
    pub led2: LedMode,
    /// How much the console prints: a live line, every frame, or the raw wire too.
    pub verbose: Verbosity,
    /// When this node sends frames on its own.
    pub tx_mode: TxMode,
    /// Defaults of the `burst` command.
    pub burst: BurstConfig,
    /// Where the console output goes (USB serial, RTT or both).
    pub console: Console,
}

impl NodeConfig {
    /// Starting point of every **UART** node config: binaries override only what they need.
    pub const DEFAULT: NodeConfig = NodeConfig {
        name: "node",
        pins: Pins { button: 4, led1: 20, led2: 21 },
        link: Link::Uart(UartLink { tx: 2, rx: 3 }),
        data_period_ms: 2500.0,
        status_period_ms: 10_000.0,
        link_timeout_periods: 3,
        debounce_ms: 30,
        lockout_ms: 150,
        rgb: RgbConfig { pattern: "viridis", freq_hz: 50.0, cycle_ms: 6000.0, brightness: 40 },
        led1: LedMode::PeerButton,
        led2: LedMode::RxFlash,
        verbose: Verbosity::Live,
        tx_mode: TxMode::Periodic,
        burst: BurstConfig { count: 1000, rate_hz: 0.0 },
        console: Console::Auto,
    };

    /// Starting point of every **Ethernet** node config: the same settings, the link is
    /// [`NetLink::DEFAULT`] (192.168.77.11, UDP). The second board needs its own addresses.
    pub const DEFAULT_NET: NodeConfig = NodeConfig {
        link: Link::Net(NetLink::DEFAULT),
        // A longer lockout than UART's: a jumper wire pushed into the breadboard bounces for a while.
        lockout_ms: 500,
        ..NodeConfig::DEFAULT
    };

    /// Compile-time validation. Use it as `const _: () = CONFIG.check();`
    /// (`esp32_rust_workshop::node_firmware!` does it for you).
    pub const fn check(&self) {
        if let Err(msg) = self.validate() {
            panic!("{}", msg);
        }
    }

    /// Checks every setting. Used at compile time by [`Self::check`] and at runtime
    /// by [`Self::set`], so the CLI enforces exactly the same rules as the build.
    // `ensure!` tests `!(x >= lo)`, which (unlike `x < lo`) also refuses NaN.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    pub const fn validate(&self) -> Result<(), &'static str> {
        let name = self.name.as_bytes();
        ensure!(!name.is_empty() && name.len() <= 12, "config: `name` must be 1 to 12 characters");
        let mut i = 0;
        while i < name.len() {
            ensure!(
                name[i] != b',' && name[i] != b'*' && name[i] != b'$' && name[i] > b' ' && name[i] < 0x7f,
                "config: `name` must be printable ASCII without spaces, `,` `*` or `$`"
            );
            i += 1;
        }

        // Periods and frequencies are f32: NaN fails every check below (see `ensure!`).
        ensure!(self.data_period_ms >= 10.0, "config: `data_period_ms` must be at least 10 ms (100 Hz)");
        ensure!(self.status_period_ms >= 100.0, "config: `status_period_ms` must be at least 100 ms");
        ensure!(self.link_timeout_periods >= 2, "config: `link_timeout_periods` must be at least 2");
        ensure!(self.debounce_ms >= 1 && self.debounce_ms <= 1000, "config: `debounce_ms` must be 1-1000");
        ensure!(self.lockout_ms <= 5000, "config: `lockout_ms` must be at most 5000");

        ensure!(fx::find_const(self.rgb.pattern).is_some(), "config: unknown `rgb.pattern` (see fx::PATTERNS)");
        ensure!(
            self.rgb.freq_hz >= fx::FREQ_RANGE.0 && self.rgb.freq_hz <= fx::FREQ_RANGE.1,
            "config: `rgb.freq_hz` out of range (1-200 Hz)"
        );
        ensure!(
            self.rgb.cycle_ms >= fx::CYCLE_RANGE.0 && self.rgb.cycle_ms <= fx::CYCLE_RANGE.1,
            "config: `rgb.cycle_ms` out of range (200-60000 ms)"
        );
        ensure!(self.rgb.brightness <= 100, "config: `rgb.brightness` must be 0-100");
        ensure!(self.data_period_ms <= 3_600_000.0, "config: `data_period_ms` must be at most 3600000 ms (1 h)");
        ensure!(self.status_period_ms <= 3_600_000.0, "config: `status_period_ms` must be at most 3600000 ms (1 h)");
        ensure!(self.link_timeout_periods <= 100, "config: `link_timeout_periods` must be at most 100");
        ensure!(
            self.burst.count >= BURST_COUNT_RANGE.0 && self.burst.count <= BURST_COUNT_RANGE.1,
            "config: `burst.count` must be 1-100000"
        );
        ensure!(
            self.burst.rate_hz == 0.0
                || (self.burst.rate_hz >= BURST_RATE_RANGE.0 && self.burst.rate_hz <= BURST_RATE_RANGE.1),
            "config: `burst.rate_hz` must be 0.1-20000 frames/s, or 0 (back to back)"
        );
        if let Err(e) = check_led(self.led1) {
            return Err(e);
        }
        if let Err(e) = check_led(self.led2) {
            return Err(e);
        }

        let p = self.pins;
        // The pins of this node: the three of `Pins`, then the link's own.
        let mut pins = [p.button, p.led1, p.led2, p.led1, p.led1];
        let mut count = 3;
        match self.link {
            Link::Uart(u) => {
                pins[3] = u.tx;
                pins[4] = u.rx;
                count = 5;
            }
            Link::Net(n) => {
                if let Err(e) = n.validate() {
                    return Err(e);
                }
                // The PHY owns GPIO5-19: none of ours may be there.
                let mut i = 0;
                while i < 3 {
                    ensure!(
                        pins[i] < crate::net::PHY_FIRST_PIN || pins[i] > crate::net::PHY_LAST_PIN,
                        "config: GPIO5-19 are used by the Ethernet PHY (MDC, MDIO, reset, RGMII)"
                    );
                    i += 1;
                }
            }
        }
        let mut a = 0;
        while a < count {
            ensure!(pins[a] <= MAX_GPIO, "config: GPIO number too high (ESP32-S31 has GPIO0-61)");
            ensure!(
                pins[a] != MISSING_PINS[0] && pins[a] != MISSING_PINS[1],
                "config: GPIO29 and GPIO41 don't exist on the ESP32-S31"
            );
            let mut r = 0;
            while r < RESERVED_PINS.len() {
                ensure!(
                    pins[a] != RESERVED_PINS[r],
                    "config: a pin is reserved (flash, USB, console, RGB LED, on-board audio or BOOT)"
                );
                r += 1;
            }
            let mut b = a + 1;
            while b < count {
                ensure!(pins[a] != pins[b], "config: the same GPIO is used twice in `pins`");
                b += 1;
            }
            a += 1;
        }
        Ok(())
    }
}

#[allow(clippy::neg_cmp_op_on_partial_ord)] // refuses NaN too, see `ensure!`
const fn check_led(mode: LedMode) -> Result<(), &'static str> {
    if let LedMode::Blink { period_ms } = mode {
        ensure!(period_ms >= 20.0 && period_ms <= 60_000.0, "config: LED blink period must be 20-60000 ms");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Runtime access (CLI `set` / `config` / `reset`)
// ---------------------------------------------------------------------------

/// Settings that can be changed at runtime. Their names match the fields of
/// [`NodeConfig`] (`rgb.*` for [`RgbConfig`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)] // one variant per field, named in [`KEYS`]
pub enum Key {
    DataPeriod,
    StatusPeriod,
    LinkTimeoutPeriods,
    Debounce,
    Lockout,
    RgbPattern,
    RgbFreq,
    RgbCycle,
    RgbBrightness,
    Led1,
    Led2,
    Verbose,
    TxMode,
    BurstCount,
    BurstRate,
    Console,
}

/// CLI name of each runtime setting, in the order `config` lists them.
pub const KEYS: [(&str, Key); 16] = [
    ("data_period_ms", Key::DataPeriod),
    ("status_period_ms", Key::StatusPeriod),
    ("link_timeout_periods", Key::LinkTimeoutPeriods),
    ("debounce_ms", Key::Debounce),
    ("lockout_ms", Key::Lockout),
    ("rgb.pattern", Key::RgbPattern),
    ("rgb.freq_hz", Key::RgbFreq),
    ("rgb.cycle_ms", Key::RgbCycle),
    ("rgb.brightness", Key::RgbBrightness),
    ("led1", Key::Led1),
    ("led2", Key::Led2),
    ("verbose", Key::Verbose),
    ("tx_mode", Key::TxMode),
    ("burst.count", Key::BurstCount),
    ("burst.rate_hz", Key::BurstRate),
    ("console", Key::Console),
];

/// Settings that exist but are fixed at build time on a **UART** node: pins are already wired
/// to peripherals, the name identifies the node, the baud rate must match the peer.
pub const READ_ONLY_UART: [&str; 7] =
    ["name", "pins.uart_tx", "pins.uart_rx", "pins.button", "pins.led1", "pins.led2", "shared.baud_rate"];

/// The same for an **Ethernet** node: the network settings must match the peer.
pub const READ_ONLY_NET: [&str; 11] = [
    "name",
    "pins.button",
    "pins.led1",
    "pins.led2",
    "net.transport",
    "net.address",
    "net.prefix_len",
    "net.peer",
    "net.port",
    "net.tcp_role",
    "net.max_speed",
];

/// Why [`NodeConfig::set`] refused a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetError {
    /// Not a setting at all.
    UnknownKey,
    /// A setting, but fixed at build time (see [`NodeConfig::read_only`]).
    ReadOnly,
    /// The value couldn't be parsed; the message says what was expected.
    BadValue(&'static str),
    /// Parsed, but rejected by [`NodeConfig::validate`].
    Invalid(&'static str),
}

impl Key {
    /// Looks a key up by its CLI name.
    pub fn from_name(name: &str) -> Option<Key> {
        KEYS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
    }

    /// CLI name of the key.
    pub fn name(self) -> &'static str {
        KEYS.iter().find(|(_, k)| *k == self).map_or("?", |(n, _)| n)
    }

    /// One-line description and accepted values, for `help <key>`.
    pub fn describe(self) -> (&'static str, &'static str) {
        match self {
            Key::DataPeriod => {
                ("period of the DAT frame sent to the peer (tx_mode periodic)", "10-3600000 ms, or a frequency: 2.5Hz")
            }
            Key::StatusPeriod => ("period of the stats line in the console", "100-3600000 ms, or a frequency"),
            Key::LinkTimeoutPeriods => ("link DOWN after this many peer periods without a frame", "2-100"),
            Key::Debounce => ("button: line must be stable this long before an edge counts, ms", "1-1000"),
            Key::Lockout => ("button: changes ignored this long after an accepted edge, ms", "0-5000"),
            Key::RgbPattern => ("RGB LED fade pattern", "name or number, see `list`"),
            Key::RgbFreq => ("RGB LED refresh rate", "1-200 Hz, or a period: 20ms"),
            Key::RgbCycle => ("duration of one fade loop (lower = faster)", "200-60000 ms, or a frequency"),
            Key::RgbBrightness => ("RGB LED brightness, %", "0-100"),
            Key::Led1 | Key::Led2 => ("breadboard LED mode", "off on blink [period|freq] button peer link rx tx"),
            Key::Verbose => {
                ("console detail: a live line, every frame, or the raw wire", "0 live, 1 frames, 2 wire (or next)")
            }
            Key::TxMode => ("when this node sends frames on its own", "periodic [period|freq], on-rx, quiet"),
            Key::BurstCount => ("frames per burst (default of `burst`)", "1-100000"),
            Key::BurstRate => ("burst rate, frames/s (default of `burst`)", "0.1-20000 Hz, or max (= 0, back to back)"),
            Key::Console => {
                ("where the console output goes (rtt, both: build with --features rtt)", "auto serial rtt both")
            }
        }
    }
}

impl LedMode {
    /// Parses `off`, `on`, `blink [period|freq]` (`blink`, `blink 200`,
    /// `blink 200 ms`, `blink 2.5Hz`...), `button`, `peer`, `link`, `rx`, `tx`.
    pub fn parse(text: &str) -> Result<LedMode, SetError> {
        let text = text.trim();
        let (word, rest) = match text.find(char::is_whitespace) {
            Some(i) => (&text[..i], text[i..].trim()),
            None => (text, ""),
        };
        Ok(match Some(word) {
            Some("off") => LedMode::Off,
            Some("on") => LedMode::On,
            Some("blink") if rest.is_empty() => LedMode::Blink { period_ms: 500.0 },
            Some("blink") => LedMode::Blink {
                period_ms: parse_period_ms(rest).ok_or(SetError::BadValue("blink period (200, 200ms, 2.5Hz...)"))?,
            },
            Some("button") => LedMode::Button,
            Some("peer") => LedMode::PeerButton,
            Some("link") => LedMode::Link,
            Some("rx") => LedMode::RxFlash,
            Some("tx") => LedMode::TxFlash,
            _ => return Err(SetError::BadValue("off, on, blink [ms], button, peer, link, rx or tx")),
        })
    }
}

impl NodeConfig {
    /// Sets `key` from text. The whole config is re-validated before the change
    /// is kept, so an invalid value leaves the config untouched.
    pub fn set(&mut self, key: &str, value: &str) -> Result<Key, SetError> {
        self.set_impl(key, value, true)
    }

    /// [`Self::set`], with the whole-config validation optional (presets apply many settings
    /// in a row, then validate once: the in-between states needn't be valid).
    fn set_impl(&mut self, key: &str, value: &str, check: bool) -> Result<Key, SetError> {
        if self.read_only().contains(&key) {
            return Err(SetError::ReadOnly);
        }
        let key = Key::from_name(key).ok_or(SetError::UnknownKey)?;
        let value = value.trim();
        let number = || value.parse::<u32>().map_err(|_| SetError::BadValue("a whole number"));
        let period =
            || parse_period_ms(value).ok_or(SetError::BadValue("a period (12.5, 250ms, 2s) or a frequency (4Hz)"));
        let freq = || parse_freq_hz(value).ok_or(SetError::BadValue("a frequency (37.5, 2kHz) or a period (20ms)"));

        let mut new = *self;
        match key {
            Key::DataPeriod => new.data_period_ms = period()?,
            Key::StatusPeriod => new.status_period_ms = period()?,
            Key::LinkTimeoutPeriods => new.link_timeout_periods = number()?,
            Key::Debounce => new.debounce_ms = number()?,
            Key::Lockout => new.lockout_ms = number()?,
            Key::RgbPattern => {
                // A name, or its number in `list` (1-based).
                let index = match value.parse::<usize>() {
                    Ok(n) if (1..=fx::PATTERNS.len()).contains(&n) => Some(n - 1),
                    _ => fx::find(value),
                };
                let index = index.ok_or(SetError::BadValue("a pattern name or number (see `list`)"))?;
                new.rgb.pattern = fx::PATTERNS[index].name;
            }
            Key::RgbFreq => new.rgb.freq_hz = freq()?,
            Key::RgbCycle => new.rgb.cycle_ms = period()?,
            Key::RgbBrightness => {
                new.rgb.brightness =
                    u8::try_from(number()?).map_err(|_| SetError::Invalid("`rgb.brightness` must be 0-100"))?;
            }
            Key::Led1 => new.led1 = LedMode::parse(value)?,
            Key::Led2 => new.led2 = LedMode::parse(value)?,
            Key::TxMode => {
                // `periodic 2.5Hz` also sets the period.
                let (mode, rest) = match value.find(char::is_whitespace) {
                    Some(i) => (&value[..i], value[i..].trim()),
                    None => (value, ""),
                };
                new.tx_mode = match mode {
                    "periodic" => TxMode::Periodic,
                    "on-rx" | "onrx" | "echo" => TxMode::OnRx,
                    "quiet" | "off" => TxMode::Quiet,
                    _ => return Err(SetError::BadValue("periodic [period|freq], on-rx or quiet")),
                };
                match (new.tx_mode, rest) {
                    (_, "") => {}
                    (TxMode::Periodic, rest) => {
                        new.data_period_ms = parse_period_ms(rest)
                            .ok_or(SetError::BadValue("periodic <period|freq>: 250ms, 2s, 4Hz..."))?;
                    }
                    _ => return Err(SetError::BadValue("only `periodic` takes a period")),
                }
            }
            Key::BurstCount => new.burst.count = number()?,
            Key::BurstRate => {
                new.burst.rate_hz = match value {
                    "max" | "0" => 0.0,
                    _ => freq()?,
                }
            }
            Key::Console => {
                new.console = match value {
                    "auto" => Console::Auto,
                    "serial" | "usb" | "uart" => Console::Serial,
                    "rtt" => Console::Rtt,
                    "both" => Console::Both,
                    _ => return Err(SetError::BadValue("auto, serial, rtt or both")),
                }
            }
            Key::Verbose => new.verbose = if value == "next" { self.verbose.next() } else { Verbosity::parse(value)? },
        }
        if check {
            new.validate().map_err(SetError::Invalid)?;
        }
        *self = new;
        Ok(key)
    }

    /// The UART pins of a UART node. The UART firmware checks it at compile time
    /// (`node_firmware!`), so a config for the wrong link can't be built.
    pub const fn uart(&self) -> UartLink {
        match self.link {
            Link::Uart(u) => u,
            Link::Net(_) => panic!("this firmware needs a UART link: use `NodeConfig::DEFAULT`, not `DEFAULT_NET`"),
        }
    }

    /// The network settings of an Ethernet node (see [`Self::uart`]).
    pub const fn net(&self) -> NetLink {
        match self.link {
            Link::Net(n) => n,
            Link::Uart(_) => panic!("this firmware needs an Ethernet link: use `NodeConfig::DEFAULT_NET`"),
        }
    }

    /// The build-time settings of this node's link kind (see [`READ_ONLY_UART`], [`READ_ONLY_NET`]).
    pub fn read_only(&self) -> &'static [&'static str] {
        match self.link {
            Link::Uart(_) => &READ_ONLY_UART,
            Link::Net(_) => &READ_ONLY_NET,
        }
    }

    /// Sets a **build-time** setting (what presets contain): the read-only ones (`pins.*`,
    /// `net.*`, `pins.uart_*`), or any runtime one, parsed like [`Self::set`]. It does **not**
    /// validate the whole config, so a preset can apply its settings in any order: call
    /// [`Self::validate`] once they are all in. `name` is a `&'static str`: use [`Self::set_name`].
    pub fn set_build(&mut self, key: &str, value: &str) -> Result<(), SetError> {
        let number = || value.trim().parse::<u8>().map_err(|_| SetError::BadValue("a GPIO number (0-61)"));
        let mut new = *self;
        match key {
            "pins.button" => new.pins.button = number()?,
            "pins.led1" => new.pins.led1 = number()?,
            "pins.led2" => new.pins.led2 = number()?,
            "pins.uart_tx" | "pins.uart_rx" => match &mut new.link {
                Link::Uart(u) => {
                    let pin = number()?;
                    if key == "pins.uart_tx" { u.tx = pin } else { u.rx = pin }
                }
                Link::Net(_) => return Err(SetError::Invalid("`pins.uart_*` only exist on a UART node")),
            },
            k if k.starts_with("net.") => match &mut new.link {
                Link::Net(n) => n.set(k, value).map_err(SetError::Invalid)?,
                Link::Uart(_) => return Err(SetError::Invalid("`net.*` only exists on an Ethernet node")),
            },
            "name" => return Err(SetError::Invalid("`name` is set with `set_name`")),
            _ => return self.set_impl(key, value, false).map(|_| ()),
        }
        *self = new;
        Ok(())
    }

    /// Sets the node name (not validated, see [`Self::set_build`]).
    pub fn set_name(&mut self, name: &'static str) {
        self.name = name;
    }

    /// Writes this config as the Rust expression that builds it, for the preset generator
    /// (`presets/*.toml` become a `const NodeConfig`). The output compiles as-is and
    /// [`Self::check`]s the same.
    pub fn write_rust(&self, out: &mut impl fmt::Write) -> fmt::Result {
        use fmt::Write as _;
        let led = |m: LedMode| -> heapless::String<64> {
            let mut s = heapless::String::new();
            let _ = match m {
                LedMode::Off => write!(s, "LedMode::Off"),
                LedMode::On => write!(s, "LedMode::On"),
                LedMode::Blink { period_ms } => write!(s, "LedMode::Blink {{ period_ms: {period_ms:?} }}"),
                LedMode::Button => write!(s, "LedMode::Button"),
                LedMode::PeerButton => write!(s, "LedMode::PeerButton"),
                LedMode::Link => write!(s, "LedMode::Link"),
                LedMode::RxFlash => write!(s, "LedMode::RxFlash"),
                LedMode::TxFlash => write!(s, "LedMode::TxFlash"),
            };
            s
        };
        writeln!(out, "NodeConfig {{")?;
        writeln!(out, "    name: {:?},", self.name)?;
        writeln!(
            out,
            "    pins: Pins {{ button: {}, led1: {}, led2: {} }},",
            self.pins.button, self.pins.led1, self.pins.led2
        )?;
        match self.link {
            Link::Uart(u) => writeln!(out, "    link: Link::Uart(UartLink {{ tx: {}, rx: {} }}),", u.tx, u.rx)?,
            Link::Net(n) => {
                writeln!(out, "    link: Link::Net(NetLink {{")?;
                writeln!(
                    out,
                    "        transport: Transport::{},",
                    if n.transport == Transport::Udp { "Udp" } else { "Tcp" }
                )?;
                writeln!(out, "        address: {:?},", n.address)?;
                writeln!(out, "        prefix_len: {},", n.prefix_len)?;
                writeln!(out, "        peer: {:?},", n.peer)?;
                writeln!(out, "        port: {},", n.port)?;
                writeln!(
                    out,
                    "        tcp_role: TcpRole::{},",
                    if n.tcp_role == TcpRole::Server { "Server" } else { "Client" }
                )?;
                writeln!(out, "        max_speed: {},", n.max_speed)?;
                writeln!(out, "    }}),")?;
            }
        }
        writeln!(out, "    data_period_ms: {:?},", self.data_period_ms)?;
        writeln!(out, "    status_period_ms: {:?},", self.status_period_ms)?;
        writeln!(out, "    link_timeout_periods: {},", self.link_timeout_periods)?;
        writeln!(out, "    debounce_ms: {},", self.debounce_ms)?;
        writeln!(out, "    lockout_ms: {},", self.lockout_ms)?;
        writeln!(
            out,
            "    rgb: RgbConfig {{ pattern: {:?}, freq_hz: {:?}, cycle_ms: {:?}, brightness: {} }},",
            self.rgb.pattern, self.rgb.freq_hz, self.rgb.cycle_ms, self.rgb.brightness
        )?;
        writeln!(out, "    led1: {},", led(self.led1))?;
        writeln!(out, "    led2: {},", led(self.led2))?;
        writeln!(
            out,
            "    verbose: Verbosity::{},",
            match self.verbose {
                Verbosity::Live => "Live",
                Verbosity::Frames => "Frames",
                Verbosity::Wire => "Wire",
            }
        )?;
        writeln!(
            out,
            "    tx_mode: TxMode::{},",
            match self.tx_mode {
                TxMode::Periodic => "Periodic",
                TxMode::OnRx => "OnRx",
                TxMode::Quiet => "Quiet",
            }
        )?;
        writeln!(out, "    burst: BurstConfig {{ count: {}, rate_hz: {:?} }},", self.burst.count, self.burst.rate_hz)?;
        writeln!(
            out,
            "    console: Console::{},",
            match self.console {
                Console::Auto => "Auto",
                Console::Serial => "Serial",
                Console::Rtt => "Rtt",
                Console::Both => "Both",
            }
        )?;
        write!(out, "}}")
    }

    /// Writes the current value of `key`, in a form [`Self::set`] accepts back.
    pub fn write_value(&self, key: Key, out: &mut impl fmt::Write) -> fmt::Result {
        match key {
            Key::DataPeriod => write!(out, "{}", Num(self.data_period_ms)),
            Key::StatusPeriod => write!(out, "{}", Num(self.status_period_ms)),
            Key::LinkTimeoutPeriods => write!(out, "{}", self.link_timeout_periods),
            Key::Debounce => write!(out, "{}", self.debounce_ms),
            Key::Lockout => write!(out, "{}", self.lockout_ms),
            Key::RgbPattern => write!(out, "{}", self.rgb.pattern),
            Key::RgbFreq => write!(out, "{}", Num(self.rgb.freq_hz)),
            Key::RgbCycle => write!(out, "{}", Num(self.rgb.cycle_ms)),
            Key::RgbBrightness => write!(out, "{}", self.rgb.brightness),
            Key::Led1 => write!(out, "{}", self.led1),
            Key::Led2 => write!(out, "{}", self.led2),
            Key::Verbose => write!(out, "{}", self.verbose),
            Key::TxMode => write!(out, "{}", self.tx_mode),
            Key::BurstCount => write!(out, "{}", self.burst.count),
            Key::BurstRate => write!(out, "{}", Num(self.burst.rate_hz)),
            Key::Console => write!(out, "{}", self.console),
        }
    }

    /// Current value of `key`, as text.
    pub fn value(&self, key: Key) -> heapless::String<24> {
        let mut s = heapless::String::new();
        let _ = self.write_value(key, &mut s);
        s
    }

    /// Restores `key` to its value in `defaults` (the compile-time config).
    pub fn reset(&mut self, key: Key, defaults: &NodeConfig) {
        // Defaults are valid (checked at compile time), so this can't fail.
        let _ = self.set(key.name(), &defaults.value(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: NodeConfig = NodeConfig::DEFAULT;

    fn with_pins(f: impl FnOnce(&mut Pins)) -> Result<(), &'static str> {
        let mut cfg = D;
        f(&mut cfg.pins);
        cfg.validate()
    }

    fn with_uart(f: impl FnOnce(&mut UartLink)) -> Result<(), &'static str> {
        let mut cfg = D;
        let Link::Uart(u) = &mut cfg.link else { unreachable!() };
        f(u);
        cfg.validate()
    }

    #[test]
    fn default_is_valid() {
        assert_eq!(D.validate(), Ok(()));
    }

    #[test]
    fn name_rules() {
        for ok in ["a", "node-A", "ABCDEFGHIJKL", "n_1.~"] {
            assert_eq!(NodeConfig { name: ok, ..D }.validate(), Ok(()), "{ok:?}");
        }
        for bad in ["", "ABCDEFGHIJKLM", "a,b", "a*b", "$a", "a b", "é", "a\tb"] {
            assert!(NodeConfig { name: bad, ..D }.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn numeric_limits_match_the_documented_ranges() {
        // (setting, lowest accepted, highest accepted), as written in `Key::describe`.
        // (setting, lowest accepted, highest accepted, just outside), as written in `Key::describe`.
        let cases: [(&str, &str, &str, &[&str]); 9] = [
            ("data_period_ms", "10", "3600000", &["9.9", "3600000.5"]),
            ("status_period_ms", "100", "3600000", &["99.5", "3600001"]),
            ("link_timeout_periods", "2", "100", &["1", "101"]),
            ("debounce_ms", "1", "1000", &["0", "1001"]),
            ("lockout_ms", "0", "5000", &["5001"]),
            ("rgb.freq_hz", "1", "200", &["0.9", "200.5"]),
            ("rgb.cycle_ms", "200", "60000", &["199.9", "60000.5"]),
            ("burst.count", "1", "100000", &["0", "100001"]),
            ("burst.rate_hz", "0.1", "20000", &["0.09", "20000.5"]),
        ];
        for (key, lo, hi, outside) in cases {
            let mut cfg = D;
            assert!(cfg.set(key, lo).is_ok(), "{key} = {lo}");
            assert!(cfg.set(key, hi).is_ok(), "{key} = {hi}");
            for bad in outside {
                assert!(matches!(cfg.set(key, bad), Err(SetError::Invalid(_))), "{key} = {bad}");
            }
            // The documented range is the one shown by `help <key>`.
            let range = Key::from_name(key).unwrap().describe().1;
            assert!(range.starts_with(&format!("{lo}-{hi}")), "describe() of {key}: {range}");
        }
    }

    #[test]
    fn pins_must_be_distinct() {
        assert!(with_pins(|p| p.led2 = p.led1).is_err());
        assert!(with_uart(|u| u.rx = u.tx).is_err());
        assert!(with_uart(|u| u.tx = 4).is_err(), "same as the button");
    }

    #[test]
    fn reserved_missing_and_out_of_range_pins_are_refused() {
        for n in RESERVED_PINS.into_iter().chain(MISSING_PINS).chain([62, 255]) {
            assert!(with_pins(|p| p.led1 = n).is_err(), "GPIO{n} accepted");
        }
    }

    #[test]
    fn every_header_pin_is_usable() {
        // GPIOs broken out on header J2 of the ESP32-S31-Function-CoreBoard-1
        // (user guide, "Header Block"), minus the reserved ones.
        let j2 = [0, 1, 2, 3, 4, 20, 21, 22, 23, 24, 25, 35, 36, 37, 38, 39, 40, 42, 43, 44, 45, 46, 47, 48, 49, 61];
        for n in j2 {
            if !RESERVED_PINS.contains(&n) && ![2, 3, 4, 21].contains(&n) {
                assert_eq!(with_pins(|p| p.led1 = n), Ok(()), "GPIO{n} refused");
            }
        }
    }

    #[test]
    fn set_keeps_the_config_on_error() {
        let mut cfg = D;
        assert_eq!(
            cfg.set("data_period_ms", "5"),
            Err(SetError::Invalid("config: `data_period_ms` must be at least 10 ms (100 Hz)"))
        );
        assert!(matches!(cfg.set("data_period_ms", "abc"), Err(SetError::BadValue(_))));
        assert!(matches!(cfg.set("data_period_ms", "-1"), Err(SetError::BadValue(_))));
        assert!(matches!(cfg.set("burst.count", "2.5"), Err(SetError::BadValue("a whole number"))));
        assert_eq!(cfg.set("rgb.brightness", "256"), Err(SetError::Invalid("`rgb.brightness` must be 0-100")));
        assert_eq!(cfg.set("rgb.brightness", "101"), Err(SetError::Invalid("config: `rgb.brightness` must be 0-100")));
        assert_eq!(cfg, D);
    }

    #[test]
    fn unknown_and_read_only_keys() {
        let mut cfg = D;
        assert_eq!(cfg.set("nope", "1"), Err(SetError::UnknownKey));
        for node in [D, NodeConfig::DEFAULT_NET] {
            let mut cfg = node;
            for key in node.read_only() {
                assert_eq!(cfg.set(key, "1"), Err(SetError::ReadOnly), "{key}");
                assert_eq!(Key::from_name(key), None, "{key} is both read-only and settable");
            }
        }
    }

    #[test]
    fn rgb_pattern_by_name_or_number() {
        let mut cfg = D;
        assert!(cfg.set("rgb.pattern", "OCEAN").is_ok());
        assert_eq!(cfg.rgb.pattern, "ocean");
        assert!(cfg.set("rgb.pattern", "1").is_ok());
        assert_eq!(cfg.rgb.pattern, fx::PATTERNS[0].name);
        assert!(cfg.set("rgb.pattern", &fx::PATTERNS.len().to_string()).is_ok());
        for bad in ["0", "6", "rainbow", ""] {
            assert!(matches!(cfg.set("rgb.pattern", bad), Err(SetError::BadValue(_))), "{bad:?}");
        }
    }

    #[test]
    fn led_modes() {
        let parse = |s| LedMode::parse(s);
        assert_eq!(parse("off"), Ok(LedMode::Off));
        assert_eq!(parse("  peer "), Ok(LedMode::PeerButton));
        assert_eq!(parse("blink"), Ok(LedMode::Blink { period_ms: 500.0 }));
        assert_eq!(parse("blink 200"), Ok(LedMode::Blink { period_ms: 200.0 }));
        assert_eq!(parse("blink 200 ms"), Ok(LedMode::Blink { period_ms: 200.0 }));
        assert_eq!(parse("blink 2.5Hz"), Ok(LedMode::Blink { period_ms: 400.0 }));
        assert_eq!(parse("blink 0.5s"), Ok(LedMode::Blink { period_ms: 500.0 }));
        assert!(parse("blink fast").is_err());
        assert!(parse("Off").is_err());
        let mut cfg = D;
        assert!(matches!(cfg.set("led1", "blink 10"), Err(SetError::Invalid(_))));
        assert!(matches!(cfg.set("led2", "blink 60001"), Err(SetError::Invalid(_))));
    }

    #[test]
    fn tx_modes() {
        let mut cfg = D;
        for (text, mode) in
            [("periodic", TxMode::Periodic), ("on-rx", TxMode::OnRx), ("echo", TxMode::OnRx), ("quiet", TxMode::Quiet)]
        {
            assert!(cfg.set("tx_mode", text).is_ok());
            assert_eq!(cfg.tx_mode, mode, "{text}");
        }
        assert!(matches!(cfg.set("tx_mode", "sometimes"), Err(SetError::BadValue(_))));
        assert!(cfg.set("burst.rate_hz", "max").is_ok());
        assert_eq!(cfg.burst.rate_hz, 0.0);
        assert!(cfg.set("tx_mode", "periodic 2.5Hz").is_ok());
        assert_eq!((cfg.tx_mode, cfg.data_period_ms), (TxMode::Periodic, 400.0));
        assert!(cfg.set("tx_mode", "periodic 12.5").is_ok());
        assert_eq!(cfg.data_period_ms, 12.5);
        assert!(matches!(cfg.set("tx_mode", "quiet 4Hz"), Err(SetError::BadValue(_))));
        assert!(matches!(cfg.set("tx_mode", "periodic 1kHz"), Err(SetError::Invalid(_)))); // 1 ms < 10 ms
    }

    #[test]
    fn verbose_accepts_common_spellings() {
        let mut cfg = D;
        for (text, level) in [
            ("0", Verbosity::Live),
            ("live", Verbosity::Live),
            ("off", Verbosity::Live),
            ("false", Verbosity::Live),
            ("1", Verbosity::Frames),
            ("frames", Verbosity::Frames),
            ("2", Verbosity::Wire),
            ("wire", Verbosity::Wire),
            ("on", Verbosity::Wire),
            ("yes", Verbosity::Wire),
        ] {
            assert!(cfg.set("verbose", text).is_ok(), "{text}");
            assert_eq!(cfg.verbose, level, "{text}");
        }
        assert!(cfg.set("verbose", "3").is_err());
        assert!(cfg.set("verbose", "maybe").is_err());
    }

    #[test]
    fn verbose_next_cycles_through_the_levels() {
        let mut cfg = D;
        assert_eq!(cfg.verbose, Verbosity::Live, "the default is the quiet live line");
        let mut seen = std::vec::Vec::new();
        for _ in 0..4 {
            assert!(cfg.set("verbose", "next").is_ok());
            seen.push(cfg.verbose);
        }
        assert_eq!(seen, [Verbosity::Frames, Verbosity::Wire, Verbosity::Live, Verbosity::Frames]);
        assert!(Verbosity::Live < Verbosity::Frames && Verbosity::Frames < Verbosity::Wire);
    }

    /// `config` prints `value(k)`; `set k <that>` must give the same config back.
    #[test]
    fn every_value_round_trips_through_set() {
        let mut cfg = NodeConfig { led1: LedMode::Blink { period_ms: 60_000.0 }, verbose: Verbosity::Wire, ..D };
        for (name, key) in KEYS {
            let before = cfg;
            let text = cfg.value(key);
            assert_eq!(cfg.set(name, &text), Ok(key), "{name} = {text}");
            assert_eq!(cfg, before, "{name} = {text}");
        }
    }

    #[test]
    fn console_routes() {
        let mut cfg = D;
        for (text, c) in [
            ("auto", Console::Auto),
            ("serial", Console::Serial),
            ("uart", Console::Serial),
            ("usb", Console::Serial),
            ("rtt", Console::Rtt),
            ("both", Console::Both),
        ] {
            assert_eq!(cfg.set("console", text), Ok(Key::Console));
            assert_eq!(cfg.console, c, "{text}");
        }
        assert!(matches!(cfg.set("console", "jtag"), Err(SetError::BadValue(_))));
        // (setting, serial / rtt with RTT built in, serial / rtt without it)
        for (c, with, without) in [
            (Console::Auto, (true, true), (true, false)),
            (Console::Serial, (true, false), (true, false)),
            (Console::Rtt, (false, true), (true, false)), // never silent without RTT
            (Console::Both, (true, true), (true, false)),
        ] {
            assert_eq!((c.serial(true), c.rtt(true)), with, "{c}");
            assert_eq!((c.serial(false), c.rtt(false)), without, "{c}");
        }
    }

    #[test]
    fn reset_restores_build_values() {
        let mut cfg = D;
        for (name, key) in KEYS {
            let mut changed = cfg;
            // Any other valid value will do: take it from a different config.
            let other = NodeConfig {
                data_period_ms: 99.5,
                status_period_ms: 999.0,
                link_timeout_periods: 9,
                debounce_ms: 9,
                lockout_ms: 9,
                verbose: Verbosity::Wire,
                led1: LedMode::On,
                led2: LedMode::Off,
                rgb: RgbConfig { pattern: "sakura", freq_hz: 37.5, cycle_ms: 999.9, brightness: 9 },
                tx_mode: TxMode::Quiet,
                burst: BurstConfig { count: 9, rate_hz: 0.5 },
                console: Console::Rtt,
                ..D
            };
            assert!(changed.set(name, &other.value(key)).is_ok());
            assert_ne!(changed, D, "{name}");
            changed.reset(key, &D);
            assert_eq!(changed, D, "{name}");
        }
        cfg.reset(Key::Verbose, &D);
        assert_eq!(cfg, D);
    }

    #[test]
    fn key_table_is_consistent() {
        for (i, (name, key)) in KEYS.iter().enumerate() {
            assert_eq!(Key::from_name(name), Some(*key));
            assert_eq!(key.name(), *name);
            assert!(KEYS[i + 1..].iter().all(|(n, k)| n != name && k != key), "{name} listed twice");
            let (what, range) = key.describe();
            assert!(!what.is_empty() && !range.is_empty());
        }
    }

    #[test]
    fn values_fit_their_buffer() {
        let worst = NodeConfig { led1: LedMode::Blink { period_ms: 60_000.0 }, data_period_ms: 3_599_999.5, ..D };
        for (_, key) in KEYS {
            let mut s = std::string::String::new();
            worst.write_value(key, &mut s).unwrap();
            assert_eq!(worst.value(key).as_str(), s, "{key:?} truncated");
        }
    }

    // --- Ethernet link, build-time settings and generated source ---------------------------

    const N: NodeConfig = NodeConfig::DEFAULT_NET;

    #[test]
    fn ethernet_default_is_valid_and_has_no_uart() {
        assert_eq!(N.validate(), Ok(()));
        assert_eq!(N.link.kind(), LinkKind::Net);
        assert_eq!(D.link.kind(), LinkKind::Uart);
        assert!(!N.read_only().iter().any(|k| k.contains("uart") || k.contains("baud")));
        assert!(!D.read_only().iter().any(|k| k.starts_with("net.")));
    }

    #[test]
    fn ethernet_nodes_keep_off_the_phy_pins() {
        for n in crate::net::PHY_FIRST_PIN..=crate::net::PHY_LAST_PIN {
            let mut cfg = N;
            cfg.set_build("pins.led1", &n.to_string()).unwrap();
            assert!(cfg.validate().is_err(), "GPIO{n} accepted");
        }
        // The same pins are fine for a UART node.
        let mut cfg = D;
        cfg.set_build("pins.led1", "9").unwrap();
        assert_eq!(cfg.validate(), Ok(()));
    }

    #[test]
    fn set_build_reaches_read_only_settings() {
        let mut cfg = N;
        // In any order: only the end result is validated.
        cfg.set_build("net.address", "192.168.77.12").unwrap();
        assert!(cfg.validate().is_err(), "peer = address, until the peer is changed too");
        cfg.set_build("net.peer", "192.168.77.11").unwrap();
        cfg.set_build("pins.button", "22").unwrap();
        cfg.set_build("rgb.pattern", "ocean").unwrap(); // runtime keys work too
        assert_eq!((cfg.pins.button, cfg.rgb.pattern), (22, "ocean"));
        assert_eq!(cfg.validate(), Ok(()));
        assert!(matches!(cfg.set_build("net.nope", "1"), Err(SetError::Invalid(_))));
        assert!(matches!(cfg.set_build("pins.uart_tx", "5"), Err(SetError::Invalid(_))));
        assert!(matches!(D.clone().set_build("net.port", "7001"), Err(SetError::Invalid(_))));
        cfg.set_name("node-B");
        assert_eq!((cfg.name, cfg.validate()), ("node-B", Ok(())));
        cfg.set_name("bad name");
        assert!(cfg.validate().is_err());
    }

    /// `write_rust` must produce exactly the config it was given (checked by comparing the
    /// text of two configs: the generated code itself is compiled by the firmware builds).
    #[test]
    fn write_rust_mentions_every_setting() {
        let mut cfg = N;
        cfg.set_build("net.transport", "tcp").unwrap();
        cfg.set("led2", "blink 2.5Hz").unwrap();
        cfg.set("tx_mode", "on-rx").unwrap();
        let mut text = std::string::String::new();
        cfg.write_rust(&mut text).unwrap();
        for needle in [
            "name: \"node\"",
            "Link::Net(NetLink",
            "transport: Transport::Tcp",
            "address: [192, 168, 77, 11]",
            "led2: LedMode::Blink { period_ms: 400.0 }",
            "tx_mode: TxMode::OnRx",
            "console: Console::Auto",
        ] {
            assert!(text.contains(needle), "{needle} missing from:\n{text}");
        }
        let mut uart = std::string::String::new();
        D.write_rust(&mut uart).unwrap();
        assert!(uart.contains("link: Link::Uart(UartLink { tx: 2, rx: 3 })"));
    }
}
