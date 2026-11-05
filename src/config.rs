//! Firmware configuration, in two parts:
//!
//! - [`shared`]: settings both nodes must agree on, because they define what goes
//!   on the wire. Change one and reflash **both** boards.
//! - [`NodeConfig`]: settings each node chooses freely (timings, pins, LEDs...).
//!   Each binary (`src/bin/uart_node_*.rs`) starts from [`NodeConfig::DEFAULT`]
//!   and overrides only what it needs.
//!
//! Invalid values are rejected **at compile time** by [`NodeConfig::check`]
//! (called by [`crate::node_firmware!`]), with a message saying what is wrong.

use core::fmt;

use crate::fx;

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
}

/// GPIO numbers used by a node. All must be different and not reserved
/// (see [`RESERVED_PINS`]). The RGB LED is fixed on the board (GPIO60).
#[derive(Clone, Copy)]
pub struct Pins {
    /// UART1 TX, wired to the peer's RX.
    pub uart_tx: u8,
    /// UART1 RX, wired to the peer's TX.
    pub uart_rx: u8,
    /// Jumper "button" to GND (internal pull-up).
    pub button: u8,
    /// Breadboard LEDs, active high: GPIO -> 330 Ω -> LED anode, cathode -> GND.
    pub led1: u8,
    pub led2: u8,
}

/// Pins the firmware must not touch on the ESP32-S31-WROOM-3 / CoreBoard-1:
/// in-package flash/PSRAM (26-28, 30-32), UART0 console (58, 59), RGB LED (60).
pub const RESERVED_PINS: [u8; 9] = [26, 27, 28, 30, 31, 32, 58, 59, 60];
/// Highest GPIO number of the ESP32-S31.
pub const MAX_GPIO: u8 = 61;

/// On-board RGB LED at boot (all of it can be changed at runtime from the CLI).
#[derive(Clone, Copy)]
pub struct RgbConfig {
    /// One of [`fx::PATTERNS`]: viridis, sunset, ocean, aurora, sakura.
    pub pattern: &'static str,
    /// Refresh rate, Hz.
    pub freq_hz: u32,
    /// Duration of one fade loop, ms.
    pub cycle_ms: u32,
    /// 0-100 %.
    pub brightness: u8,
}

/// What a breadboard LED shows (can be changed at runtime with the CLI `led` command).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LedMode {
    Off,
    On,
    /// Blinks with this full period (on + off), ms.
    Blink { period_ms: u32 },
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
                let _ = fmt::Write::write_fmt(&mut s, format_args!("blink {period_ms} ms"));
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

/// Everything a node may set on its own, without breaking the link.
#[derive(Clone, Copy)]
pub struct NodeConfig {
    /// Name sent in every frame. Max 12 chars, no `,` `*` `$` (frame separators).
    pub name: &'static str,
    pub pins: Pins,
    /// Period of the DAT frame, ms. Sent in the frame, so the peer adapts to it.
    pub data_period_ms: u32,
    /// Period of the stats line in the console, ms.
    pub status_period_ms: u32,
    /// The link is DOWN after this many *peer* data periods without a valid frame.
    pub link_timeout_periods: u32,
    /// Button: line must be stable this long before an edge is accepted, ms.
    pub debounce_ms: u32,
    /// Button: changes ignored this long after an accepted edge, ms.
    pub lockout_ms: u32,
    pub rgb: RgbConfig,
    pub led1: LedMode,
    pub led2: LedMode,
    /// Verbose console: raw frames on the wire, byte counters, ignored bounces.
    pub verbose: bool,
}

impl NodeConfig {
    pub const DEFAULT: NodeConfig = NodeConfig {
        name: "node",
        pins: Pins { uart_tx: 2, uart_rx: 3, button: 4, led1: 20, led2: 21 },
        data_period_ms: 2500,
        status_period_ms: 10_000,
        link_timeout_periods: 3,
        debounce_ms: 30,
        lockout_ms: 150,
        rgb: RgbConfig { pattern: "viridis", freq_hz: 50, cycle_ms: 6000, brightness: 40 },
        led1: LedMode::PeerButton,
        led2: LedMode::RxFlash,
        verbose: false,
    };

    /// Compile-time validation. Use it as `const _: () = CONFIG.check();`
    /// ([`crate::node_firmware!`] does it for you).
    pub const fn check(&self) {
        if let Err(msg) = self.validate() {
            panic!("{}", msg);
        }
    }

    /// Checks every setting. Used at compile time by [`Self::check`] and at runtime
    /// by [`Self::set`], so the CLI enforces exactly the same rules as the build.
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

        ensure!(self.data_period_ms >= 10, "config: `data_period_ms` must be at least 10 ms");
        ensure!(self.status_period_ms >= 100, "config: `status_period_ms` must be at least 100 ms");
        ensure!(self.link_timeout_periods >= 2, "config: `link_timeout_periods` must be at least 2");
        ensure!(self.debounce_ms >= 1 && self.debounce_ms <= 1000, "config: `debounce_ms` must be 1-1000");
        ensure!(self.lockout_ms <= 5000, "config: `lockout_ms` must be at most 5000");

        ensure!(fx::find_const(self.rgb.pattern).is_some(), "config: unknown `rgb.pattern` (see fx::PATTERNS)");
        ensure!(
            self.rgb.freq_hz >= fx::FREQ_RANGE.0 && self.rgb.freq_hz <= fx::FREQ_RANGE.1,
            "config: `rgb.freq_hz` out of range (1-200)"
        );
        ensure!(
            self.rgb.cycle_ms >= fx::CYCLE_RANGE.0 && self.rgb.cycle_ms <= fx::CYCLE_RANGE.1,
            "config: `rgb.cycle_ms` out of range (200-60000)"
        );
        ensure!(self.rgb.brightness <= 100, "config: `rgb.brightness` must be 0-100");
        ensure!(self.data_period_ms <= 3_600_000, "config: `data_period_ms` must be at most 3600000 (1 h)");
        ensure!(self.status_period_ms <= 3_600_000, "config: `status_period_ms` must be at most 3600000 (1 h)");
        ensure!(self.link_timeout_periods <= 100, "config: `link_timeout_periods` must be at most 100");
        if let Err(e) = check_led(self.led1) {
            return Err(e);
        }
        if let Err(e) = check_led(self.led2) {
            return Err(e);
        }

        let p = self.pins;
        let pins = [p.uart_tx, p.uart_rx, p.button, p.led1, p.led2];
        let mut a = 0;
        while a < pins.len() {
            ensure!(pins[a] <= MAX_GPIO, "config: GPIO number too high (ESP32-S31 has GPIO0-61)");
            let mut r = 0;
            while r < RESERVED_PINS.len() {
                ensure!(pins[a] != RESERVED_PINS[r], "config: a pin is reserved (flash/PSRAM, console or RGB LED)");
                r += 1;
            }
            let mut b = a + 1;
            while b < pins.len() {
                ensure!(pins[a] != pins[b], "config: the same GPIO is used twice in `pins`");
                b += 1;
            }
            a += 1;
        }
        Ok(())
    }
}

const fn check_led(mode: LedMode) -> Result<(), &'static str> {
    if let LedMode::Blink { period_ms } = mode {
        ensure!(period_ms >= 20 && period_ms <= 60_000, "config: LED blink period must be 20-60000 ms");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Runtime access (CLI `set` / `config` / `reset`)
// ---------------------------------------------------------------------------

/// Settings that can be changed at runtime. Their names match the fields of
/// [`NodeConfig`] (`rgb.*` for [`RgbConfig`]).
#[derive(Clone, Copy, PartialEq, Eq)]
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
}

pub const KEYS: [(&str, Key); 12] = [
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
];

/// Settings that exist but are fixed at build time: pins are already wired to
/// peripherals, the name identifies the node, the baud rate must match the peer.
pub const READ_ONLY: [&str; 7] =
    ["name", "pins.uart_tx", "pins.uart_rx", "pins.button", "pins.led1", "pins.led2", "shared.baud_rate"];

pub enum SetError {
    UnknownKey,
    ReadOnly,
    /// The value couldn't be parsed; the message says what was expected.
    BadValue(&'static str),
    /// Parsed, but rejected by [`NodeConfig::validate`].
    Invalid(&'static str),
}

impl Key {
    pub fn from_name(name: &str) -> Option<Key> {
        KEYS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
    }

    pub fn name(self) -> &'static str {
        KEYS.iter().find(|(_, k)| *k == self).map_or("?", |(n, _)| n)
    }

    /// One-line description and accepted values, for `help <key>`.
    pub fn describe(self) -> (&'static str, &'static str) {
        match self {
            Key::DataPeriod => ("period of the DAT frame sent to the peer, ms", "10-3600000"),
            Key::StatusPeriod => ("period of the stats line in the console, ms", "100-3600000"),
            Key::LinkTimeoutPeriods => ("link DOWN after this many peer periods without a frame", "2-100"),
            Key::Debounce => ("button: line must be stable this long before an edge counts, ms", "1-1000"),
            Key::Lockout => ("button: changes ignored this long after an accepted edge, ms", "0-5000"),
            Key::RgbPattern => ("RGB LED fade pattern", "name or number, see `list`"),
            Key::RgbFreq => ("RGB LED refresh rate, Hz", "1-200"),
            Key::RgbCycle => ("duration of one fade loop, ms (lower = faster)", "200-60000"),
            Key::RgbBrightness => ("RGB LED brightness, %", "0-100"),
            Key::Led1 | Key::Led2 => ("breadboard LED mode", "off on blink [ms] button peer link rx tx"),
            Key::Verbose => ("verbose console output", "on off"),
        }
    }
}

impl LedMode {
    /// Parses `off`, `on`, `blink [ms]` (also `blink 500 ms`), `button`, `peer`,
    /// `link`, `rx`, `tx`.
    pub fn parse(text: &str) -> Result<LedMode, SetError> {
        let mut words = text.split_whitespace();
        Ok(match words.next() {
            Some("off") => LedMode::Off,
            Some("on") => LedMode::On,
            Some("blink") => match words.next() {
                None => LedMode::Blink { period_ms: 500 },
                Some(ms) => LedMode::Blink {
                    period_ms: ms.parse().map_err(|_| SetError::BadValue("blink period: a number of ms"))?,
                },
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
        if READ_ONLY.contains(&key) {
            return Err(SetError::ReadOnly);
        }
        let key = Key::from_name(key).ok_or(SetError::UnknownKey)?;
        let value = value.trim();
        let number = || value.parse::<u32>().map_err(|_| SetError::BadValue("a whole number"));

        let mut new = *self;
        match key {
            Key::DataPeriod => new.data_period_ms = number()?,
            Key::StatusPeriod => new.status_period_ms = number()?,
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
            Key::RgbFreq => new.rgb.freq_hz = number()?,
            Key::RgbCycle => new.rgb.cycle_ms = number()?,
            Key::RgbBrightness => {
                new.rgb.brightness = u8::try_from(number()?).map_err(|_| SetError::Invalid("`rgb.brightness` must be 0-100"))?;
            }
            Key::Led1 => new.led1 = LedMode::parse(value)?,
            Key::Led2 => new.led2 = LedMode::parse(value)?,
            Key::Verbose => {
                new.verbose = match value {
                    "on" | "1" | "true" | "yes" => true,
                    "off" | "0" | "false" | "no" => false,
                    _ => return Err(SetError::BadValue("on or off")),
                }
            }
        }
        new.validate().map_err(SetError::Invalid)?;
        *self = new;
        Ok(key)
    }

    /// Writes the current value of `key`, in a form [`Self::set`] accepts back.
    pub fn write_value(&self, key: Key, out: &mut impl fmt::Write) -> fmt::Result {
        match key {
            Key::DataPeriod => write!(out, "{}", self.data_period_ms),
            Key::StatusPeriod => write!(out, "{}", self.status_period_ms),
            Key::LinkTimeoutPeriods => write!(out, "{}", self.link_timeout_periods),
            Key::Debounce => write!(out, "{}", self.debounce_ms),
            Key::Lockout => write!(out, "{}", self.lockout_ms),
            Key::RgbPattern => write!(out, "{}", self.rgb.pattern),
            Key::RgbFreq => write!(out, "{}", self.rgb.freq_hz),
            Key::RgbCycle => write!(out, "{}", self.rgb.cycle_ms),
            Key::RgbBrightness => write!(out, "{}", self.rgb.brightness),
            Key::Led1 => write!(out, "{}", self.led1),
            Key::Led2 => write!(out, "{}", self.led2),
            Key::Verbose => write!(out, "{}", if self.verbose { "on" } else { "off" }),
        }
    }

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
