//! CLI on the USB console: line editing, command parsing and the help table.
//!
//! The firmware feeds the bytes it reads from USB Serial/JTAG to a [`LineEditor`],
//! then [`parse`]s each submitted line into a [`Command`]:
//!
//! ```
//! use workshop_core::cli::{parse, Command, EditorEvent, LineEditor};
//!
//! let mut editor = LineEditor::new();
//! let mut submitted = None;
//! for &byte in b"led 1 blinx\x7fk 200\r" { // \x7f = backspace
//!     if let Some(EditorEvent::Submitted(line)) = editor.feed(byte) {
//!         submitted = Some(line);
//!     }
//! }
//! let line = submitted.unwrap();
//! assert_eq!(line, "led 1 blink 200");
//! assert!(matches!(parse(&line), Ok(Command::Set { key, value }) if key == "led1" && value == "blink 200"));
//! ```

use core::fmt;

use heapless::String;

use crate::config::{BURST_COUNT_RANGE, BURST_RATE_RANGE, BurstConfig, LinkKind, shared};
use crate::units::{Num, parse_freq_hz};

/// Longest command line, in characters.
pub const MAX_INPUT: usize = 48;

/// A parsed command line.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// `help`, or `help <command|key>`.
    Help(Option<String<24>>),
    /// `list`: RGB patterns.
    List,
    /// `status`: LED state.
    Status,
    /// Show every setting, its current value and whether it differs from the build.
    Config,
    /// `set <key> <value>`; the shortcuts (`pattern`, `freq`, `led 1`...) map to it.
    Set {
        /// Setting name (see [`crate::config::KEYS`]).
        key: String<24>,
        /// Value as typed, checked by [`crate::config::NodeConfig::set`].
        value: String<MAX_INPUT>,
    },
    /// Restore one key, or everything, to the compiled-in config.
    Reset(Option<String<24>>),
    /// `next`: next RGB pattern.
    NextPattern,
    /// `info`: print the boot card again (with the current settings).
    Info,
    /// `save`: store the settings changed with `set` in flash, applied at boot.
    Save,
    /// `forget`: erase the saved settings (the current ones are kept until reboot).
    Forget,
    /// `reboot`: software reset.
    Reboot,
    /// `baud`: show the link speed; `baud <rate>`: switch both nodes to `rate`. UART nodes.
    Baud(Option<u32>),
    /// `link`: the Ethernet link: PHY state, speed, addresses, transport, counters. Ethernet nodes.
    Link,
    /// `burst [count [rate]]`. Missing values: ask them ([`BurstWizard`]).
    Burst {
        /// Frames in the burst.
        count: Option<u32>,
        /// Frames per second (decimals allowed), 0 = back to back.
        rate_hz: Option<f32>,
    },
}

/// Why a command line was refused. `Display` gives the message shown to the user.
#[derive(Debug, PartialEq, Eq)]
pub enum CliError<'a> {
    /// First word is not a command.
    UnknownCommand(&'a str),
    /// `set <key>` (or a shortcut) without a value.
    MissingValue(&'a str),
    /// `set` without a key.
    SetUsage,
    /// `led` without a valid LED number.
    LedUsage,
    /// A key or value longer than the command buffers.
    TooLong,
    /// A number argument that doesn't parse or is out of range.
    BadNumber {
        /// What was expected, e.g. `baud rate 1200-5000000`.
        expected: &'static str,
        /// What was typed.
        got: &'a str,
    },
}

impl fmt::Display for CliError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::UnknownCommand(cmd) => write!(f, "unknown command '{cmd}' (try `help`)"),
            CliError::MissingValue(key) => write!(f, "missing value for `{key}`"),
            CliError::SetUsage => f.write_str("usage: set <key> <value>  (see `config` for the keys)"),
            CliError::LedUsage => f.write_str("usage: led <1|2> <off|on|blink [ms]|button|peer|link|rx|tx>"),
            CliError::TooLong => f.write_str("argument too long"),
            CliError::BadNumber { expected, got } => write!(f, "'{got}': expected {expected}"),
        }
    }
}

pub use crate::editor::{EditorEvent, LineEditor};

/// Splits `"word rest of line"` into `("word", "rest of line")`.
fn split_first(text: &str) -> (&str, &str) {
    let text = text.trim();
    match text.find(char::is_whitespace) {
        Some(i) => (&text[..i], text[i..].trim()),
        None => (text, ""),
    }
}

fn set<'a>(key: &'a str, value: &'a str) -> Result<Command, CliError<'a>> {
    if value.is_empty() {
        return Err(CliError::MissingValue(key));
    }
    Ok(Command::Set {
        key: String::try_from(key).map_err(|_| CliError::TooLong)?,
        value: String::try_from(value).map_err(|_| CliError::TooLong)?,
    })
}

/// Parses `5000000`, `5M`, `921600`, `500k`, `115.2k`: a rate with an optional
/// `k` (×1000) or `M` (×1 000 000) suffix.
pub fn parse_rate(text: &str) -> Option<u32> {
    let (digits, scale) = match text.as_bytes().last()? {
        b'k' | b'K' => (&text[..text.len() - 1], 1000u64),
        b'M' => (&text[..text.len() - 1], 1_000_000u64),
        _ => (text, 1u64),
    };
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if int.is_empty() || frac.len() > 6 || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut value = int.parse::<u64>().ok()?.checked_mul(scale)?;
    let mut place = scale;
    for b in frac.bytes() {
        place /= 10;
        value = value.checked_add(u64::from(b - b'0') * place)?;
    }
    if place == 0 && !frac.is_empty() {
        return None; // more decimals than the suffix allows
    }
    u32::try_from(value).ok()
}

fn in_range<'a>(text: &'a str, range: (u32, u32), expected: &'static str) -> Result<u32, CliError<'a>> {
    match parse_rate(text) {
        Some(v) if (range.0..=range.1).contains(&v) => Ok(v),
        _ => Err(CliError::BadNumber { expected, got: text }),
    }
}

/// Parses a burst rate: frames/s (`50`, `37.5`, `2kHz`), a period between frames
/// (`20ms`), or `max` / `0` (back to back).
fn burst_rate(text: &str) -> Result<f32, CliError<'_>> {
    if matches!(text, "max" | "0") {
        return Ok(0.0);
    }
    match parse_freq_hz(text) {
        Some(hz) if (BURST_RATE_RANGE.0..=BURST_RATE_RANGE.1).contains(&hz) => Ok(hz),
        _ => Err(CliError::BadNumber { expected: "frames/s 0.1-20000 (37.5, 2kHz, 20ms...), or max", got: text }),
    }
}

/// Parses a command line. Shortcuts become the equivalent [`Command::Set`].
pub fn parse(line: &str) -> Result<Command, CliError<'_>> {
    let (cmd, rest) = split_first(line);
    let topic =
        || if rest.is_empty() { Ok(None) } else { String::try_from(rest).map(Some).map_err(|_| CliError::TooLong) };
    match cmd {
        // A topic too long to be a key or command: show the general help.
        "help" | "h" | "?" => Ok(Command::Help(topic().unwrap_or(None))),
        "list" | "ls" => Ok(Command::List),
        "status" | "st" => Ok(Command::Status),
        "config" | "cfg" | "get" => Ok(Command::Config),
        "next" | "n" => Ok(Command::NextPattern),
        "info" | "about" | "boot" => Ok(Command::Info),
        "save" | "w" => Ok(Command::Save),
        "forget" => Ok(Command::Forget),
        "reboot" | "restart" => Ok(Command::Reboot),
        "set" | "s" => match split_first(rest) {
            ("", _) => Err(CliError::SetUsage),
            (key, value) => set(key, value),
        },
        "reset" => Ok(Command::Reset(topic()?)),
        // Shortcuts for the most used keys.
        "pattern" | "p" => set("rgb.pattern", rest),
        "freq" | "f" => set("rgb.freq_hz", rest),
        "cycle" | "c" => set("rgb.cycle_ms", rest),
        "bright" | "b" => set("rgb.brightness", rest),
        "verbose" | "v" => set("verbose", if rest.is_empty() { "next" } else { rest }),
        "tx" => set("tx_mode", rest),
        "period" => set("data_period_ms", rest),
        "link" | "net" => Ok(Command::Link),
        "baud" => match rest {
            "" => Ok(Command::Baud(None)),
            rate => Ok(Command::Baud(Some(in_range(
                rate,
                shared::BAUD_RANGE,
                "baud rate 1200-5000000 (or 921600, 2M, 500k...)",
            )?))),
        },
        "burst" => {
            let (count, rate) = split_first(rest);
            Ok(Command::Burst {
                count: if count.is_empty() {
                    None
                } else {
                    Some(in_range(count, BURST_COUNT_RANGE, "frame count 1-100000")?)
                },
                rate_hz: if rate.is_empty() { None } else { Some(burst_rate(rate)?) },
            })
        }
        "led" | "l" => match split_first(rest) {
            ("1", mode) => set("led1", mode),
            ("2", mode) => set("led2", mode),
            _ => Err(CliError::LedUsage),
        },
        _ => Err(CliError::UnknownCommand(cmd)),
    }
}

// ---------------------------------------------------------------------------
// `burst` questions
// ---------------------------------------------------------------------------

/// Asks the `burst` parameters one by one when the command is typed alone:
/// first the frame count, then the rate. An empty answer takes the default shown
/// in brackets, `q` cancels.
///
/// ```
/// use workshop_core::cli::{BurstWizard, Command, WizardStep};
/// use workshop_core::config::BurstConfig;
///
/// let mut w = BurstWizard::new(BurstConfig { count: 1000, rate_hz: 0.0 });
/// assert_eq!(w.question(), "burst length, frames [1000]");
/// assert_eq!(w.answer("500"), WizardStep::Ask);
/// assert_eq!(w.question(), "burst speed, frames/s or max [max]");
/// assert_eq!(w.answer(""), WizardStep::Done(Command::Burst { count: Some(500), rate_hz: Some(0.0) }));
/// ```
#[derive(Debug)]
pub struct BurstWizard {
    defaults: BurstConfig,
    count: Option<u32>,
}

/// What an answer to the [`BurstWizard`] led to.
#[derive(Debug, PartialEq)]
pub enum WizardStep {
    /// Ask the next question ([`BurstWizard::question`]).
    Ask,
    /// Invalid answer: show this, then ask the same question again.
    Retry(&'static str),
    /// All answers given: run this command.
    Done(Command),
    /// The user typed `q`.
    Cancelled,
}

impl BurstWizard {
    /// Starts with the count question; `defaults` are the `burst.*` settings.
    pub fn new(defaults: BurstConfig) -> Self {
        Self { defaults, count: None }
    }

    /// The question to show now, with its default in brackets.
    pub fn question(&self) -> String<48> {
        let mut q = String::new();
        let _ = match self.count {
            None => fmt::Write::write_fmt(&mut q, format_args!("burst length, frames [{}]", self.defaults.count)),
            Some(_) if self.defaults.rate_hz == 0.0 => {
                fmt::Write::write_str(&mut q, "burst speed, frames/s or max [max]")
            }
            Some(_) => fmt::Write::write_fmt(
                &mut q,
                format_args!("burst speed, frames/s or max [{}]", Num(self.defaults.rate_hz)),
            ),
        };
        q
    }

    /// Takes the answer to the current question.
    pub fn answer(&mut self, line: &str) -> WizardStep {
        let line = line.trim();
        if matches!(line, "q" | "quit" | "cancel") {
            return WizardStep::Cancelled;
        }
        match self.count {
            None => {
                let count =
                    if line.is_empty() { Ok(self.defaults.count) } else { in_range(line, BURST_COUNT_RANGE, "") };
                match count {
                    Ok(count) => {
                        self.count = Some(count);
                        WizardStep::Ask
                    }
                    Err(_) => WizardStep::Retry("a number of frames, 1-100000 (q to cancel)"),
                }
            }
            Some(count) => {
                let rate = if line.is_empty() { Ok(self.defaults.rate_hz) } else { burst_rate(line) };
                match rate {
                    Ok(rate_hz) => WizardStep::Done(Command::Burst { count: Some(count), rate_hz: Some(rate_hz) }),
                    Err(_) => WizardStep::Retry("frames per second: 37.5, 2kHz, 20ms... or max (q to cancel)"),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Help: one table feeds both `help` (summaries) and `help <cmd>` (details).
// ---------------------------------------------------------------------------

/// Section of `help` a command is listed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Commands that change or show settings.
    Settings,
    /// Short forms of `set <key>`.
    Shortcuts,
    /// Link speed and traffic tests.
    Link,
    /// Read-only information.
    Info,
    /// Saved settings and restart.
    System,
}

/// Help entry of one command.
pub struct CmdHelp {
    /// Main name.
    pub name: &'static str,
    /// Other names accepted for the command.
    pub aliases: &'static [&'static str],
    /// Syntax, e.g. `set <key> <value>`.
    pub usage: &'static str,
    /// One line for `help`.
    pub summary: &'static str,
    /// Lines shown by `help <command>`.
    pub details: &'static [&'static str],
    /// Example lines; each one is checked by a unit test.
    pub examples: &'static [&'static str],
    /// Where `help` lists it.
    pub group: Group,
    /// The link this command exists on (`None` = every node): `baud` is UART only, `link` is
    /// Ethernet only.
    pub only: Option<LinkKind>,
}

impl CmdHelp {
    /// Whether this command is available on a node with this kind of link.
    pub fn on(&self, kind: LinkKind) -> bool {
        self.only.is_none_or(|only| only == kind)
    }
}

/// Every command, in `help` order. Single source for `help` and the docs.
pub static COMMANDS: &[CmdHelp] = &[
    CmdHelp {
        name: "config",
        aliases: &["cfg", "get"],
        usage: "config",
        summary: "all settings: current value, build value, read-only ones",
        details: &[
            "Lists every setting of this node. Values changed at runtime are shown",
            "in yellow next to their build value (from the node's preset).",
            "Read-only settings (name, pins, link) need a rebuild to change.",
        ],
        examples: &["config"],
        group: Group::Settings,
        only: None,
    },
    CmdHelp {
        name: "set",
        aliases: &["s"],
        usage: "set <key> <value>",
        summary: "change a setting, applied immediately",
        details: &[
            "Keys are the field names of NodeConfig (see `config` for the list,",
            "`help <key>` for one key). The value goes through the same checks as",
            "the build config: if it's refused, nothing changes.",
            "Changes are lost on reboot (back to the build values).",
        ],
        examples: &[
            "set data_period_ms 1000",
            "set data_period_ms 2.5Hz",
            "set rgb.pattern ocean",
            "set led1 blink 200",
        ],
        group: Group::Settings,
        only: None,
    },
    CmdHelp {
        name: "reset",
        aliases: &[],
        usage: "reset [key]",
        summary: "back to the build value (one key, or all)",
        details: &["Without a key, restores every setting changed with `set`."],
        examples: &["reset rgb.brightness", "reset"],
        group: Group::Settings,
        only: None,
    },
    CmdHelp {
        name: "save",
        aliases: &["w"],
        usage: "save",
        summary: "keep the current settings across reboots (in flash)",
        details: &[
            "Stores the settings that differ from the build config in flash; they are",
            "applied at every boot, on top of the build config. Reflashing the firmware",
            "keeps them (espflash doesn't touch that area); `forget` erases them.",
            "Needs the `persist` Cargo feature (on by default).",
        ],
        examples: &["save"],
        group: Group::System,
        only: None,
    },
    CmdHelp {
        name: "forget",
        aliases: &[],
        usage: "forget",
        summary: "erase the saved settings (next boot: build config)",
        details: &[
            "The current settings stay as they are until the next reboot; `reset` then `reboot` to go back now.",
        ],
        examples: &["forget"],
        group: Group::System,
        only: None,
    },
    CmdHelp {
        name: "reboot",
        aliases: &["restart"],
        usage: "reboot",
        summary: "restart the chip (software reset)",
        details: &["Settings not saved with `save` are lost. The boot card then shows a software reset (`CoreSw`)."],
        examples: &["reboot"],
        group: Group::System,
        only: None,
    },
    CmdHelp {
        name: "verbose",
        aliases: &["v"],
        usage: "verbose [0|1|2|live|frames|wire]",
        summary: "how much the console prints (= set verbose)",
        details: &[
            "0 live    one line, updated in place, with the last frame sent or received and",
            "          the counters. Lost frames, link events, bursts and errors still scroll.",
            "1 frames  every frame on its own line, and the periodic stats line",
            "2 wire    frames plus, for each, the raw line, its checksum and size, the time",
            "          since the peer's previous frame, ignored button bounces and counters",
            "`verbose` alone goes to the next level (wire goes back to live).",
        ],
        examples: &["verbose", "verbose frames", "verbose 0", "verbose wire"],
        group: Group::Shortcuts,
        only: None,
    },
    CmdHelp {
        name: "pattern",
        aliases: &["p"],
        usage: "pattern <name|1-5>",
        summary: "RGB LED pattern (= set rgb.pattern)",
        details: &["Name or number from `list`, which shows a color preview of each."],
        examples: &["pattern viridis", "p 3"],
        group: Group::Shortcuts,
        only: None,
    },
    CmdHelp {
        name: "next",
        aliases: &["n"],
        usage: "next",
        summary: "next RGB LED pattern",
        details: &["Cycles through the patterns in `list` order."],
        examples: &["next"],
        group: Group::Shortcuts,
        only: None,
    },
    CmdHelp {
        name: "freq",
        aliases: &["f"],
        usage: "freq <Hz|period>",
        summary: "RGB LED refresh rate (= set rgb.freq_hz)",
        details: &[
            "How often the LED color is recomputed: smoothness, not speed.",
            "1-200 Hz, decimals allowed (37.5), or a period (20ms = 50 Hz).",
            "To make the fade faster, lower `cycle` instead.",
        ],
        examples: &["freq 100", "freq 37.5", "freq 20ms"],
        group: Group::Shortcuts,
        only: None,
    },
    CmdHelp {
        name: "cycle",
        aliases: &["c"],
        usage: "cycle <ms|freq>",
        summary: "duration of one fade loop (= set rgb.cycle_ms)",
        details: &[
            "Lower = faster fade. 200-60000 ms, decimals allowed (2500.5),",
            "with a unit (2.5s), or as a frequency (0.5Hz = one loop every 2 s).",
        ],
        examples: &["cycle 2000", "cycle 2.5s", "cycle 0.5Hz"],
        group: Group::Shortcuts,
        only: None,
    },
    CmdHelp {
        name: "bright",
        aliases: &["b"],
        usage: "bright <0-100>",
        summary: "RGB LED brightness, % (= set rgb.brightness)",
        details: &["Gamma-corrected: 50 % looks about half as bright as 100 %."],
        examples: &["bright 70"],
        group: Group::Shortcuts,
        only: None,
    },
    CmdHelp {
        name: "led",
        aliases: &["l"],
        usage: "led <1|2> <mode>",
        summary: "breadboard LED mode (= set led1 / set led2)",
        details: &[
            "off, on        always off / on",
            "blink [p|f]    blinks: full period (200, 0.5s) or frequency (2Hz), default 500 ms",
            "button         on while this node's button is pressed",
            "peer           on while the peer's button is pressed",
            "link           on while the link is up",
            "rx, tx         short flash on each received / sent frame",
        ],
        examples: &["led 1 peer", "led 2 blink 200", "led 2 blink 2.5Hz"],
        group: Group::Shortcuts,
        only: None,
    },
    CmdHelp {
        name: "baud",
        aliases: &[],
        usage: "baud [rate]",
        summary: "show the link speed, or switch both nodes to a new one",
        details: &[
            "Both nodes must use the same speed, so the switch is a handshake: this",
            "node sends BAU REQ at the current speed, the peer answers BAU ACK and",
            "switches, then this node switches. If no valid frame arrives at the new",
            "speed for a few periods, each node goes back to the boot speed on its own.",
            "Accepts 115200, 921600, 2M, 500k... (1200 to 5000000).",
        ],
        examples: &["baud", "baud 1M", "baud 115200", "baud 5M"],
        group: Group::Link,
        only: Some(LinkKind::Uart),
    },
    CmdHelp {
        name: "link",
        aliases: &["net"],
        usage: "link",
        summary: "the Ethernet link: cable, speed, addresses, transport, counters",
        details: &[
            "Shows what the PHY negotiated (speed, duplex), this node's address and the",
            "peer's, UDP or TCP and the port, and the frame counters. The speed and the",
            "addresses are fixed at build time (presets/*.toml); `baud` is the UART equivalent.",
        ],
        examples: &["link"],
        group: Group::Link,
        only: Some(LinkKind::Net),
    },
    CmdHelp {
        name: "burst",
        aliases: &[],
        usage: "burst [count [rate|max]]",
        summary: "send a burst of frames and measure the link",
        details: &[
            "Without arguments, asks the length then the speed (Enter = the default",
            "in brackets, from `burst.count` / `burst.rate_hz`; q cancels).",
            "rate = frames per second, decimals allowed (37.5), or a period between",
            "frames (20ms); max (or 0) = back to back at line speed.",
            "Frames are counted, not printed. At the end this node shows what it sent,",
            "the peer what it received: lost, corrupted, throughput, % of the line.",
        ],
        examples: &["burst", "burst 1000 max", "burst 200 50", "burst 100 2.5Hz"],
        group: Group::Link,
        only: None,
    },
    CmdHelp {
        name: "tx",
        aliases: &[],
        usage: "tx <periodic [period]|on-rx|quiet>",
        summary: "when this node transmits (= set tx_mode)",
        details: &[
            "periodic   a DAT frame every data_period_ms (default); `tx periodic 2.5Hz`",
            "           or `tx periodic 400ms` also sets the period",
            "on-rx      no DAT: an ECH reply to each frame from the peer; the peer then",
            "           shows the round-trip time (rtt) of its frames",
            "quiet      nothing on its own (button events and link handshakes only)",
        ],
        examples: &["tx on-rx", "tx periodic", "tx periodic 2.5Hz", "tx periodic 12.5ms"],
        group: Group::Link,
        only: None,
    },
    CmdHelp {
        name: "period",
        aliases: &[],
        usage: "period <period|freq>",
        summary: "DAT period (= set data_period_ms)",
        details: &[
            "A period (12.5 = 12.5 ms, 250ms, 2.5s) or a frequency (4Hz = 250 ms).",
            "Sent in each DAT frame, so the peer adapts its link timeout.",
        ],
        examples: &["period 1s", "period 12.5", "period 4Hz"],
        group: Group::Link,
        only: None,
    },
    CmdHelp {
        name: "info",
        aliases: &["about", "boot"],
        usage: "info",
        summary: "the boot card again: link, node, build, chip, LEDs",
        details: &["Shows the current settings (those changed with `set` included)."],
        examples: &["info"],
        group: Group::Info,
        only: None,
    },
    CmdHelp {
        name: "list",
        aliases: &["ls"],
        usage: "list",
        summary: "RGB patterns with a color preview",
        details: &["The current pattern is marked with ▶."],
        examples: &["list"],
        group: Group::Info,
        only: None,
    },
    CmdHelp {
        name: "status",
        aliases: &["st"],
        usage: "status",
        summary: "current state of the RGB LED and breadboard LEDs",
        details: &[],
        examples: &["status"],
        group: Group::Info,
        only: None,
    },
    CmdHelp {
        name: "help",
        aliases: &["h", "?"],
        usage: "help [command|key]",
        summary: "this list, or details on one command or setting",
        details: &[],
        examples: &["help set", "help data_period_ms"],
        group: Group::Info,
        only: None,
    },
];

/// Finds a command by name or alias.
pub fn find_help(name: &str) -> Option<&'static CmdHelp> {
    COMMANDS.iter().find(|c| c.name == name || c.aliases.contains(&name))
}

#[cfg(test)]
mod link_command_tests {
    use super::*;

    #[test]
    fn rates_with_suffixes() {
        for (text, v) in [
            ("5000000", 5_000_000),
            ("5M", 5_000_000),
            ("2M", 2_000_000),
            ("500k", 500_000),
            ("115.2k", 115_200),
            ("1.5M", 1_500_000),
            ("921600", 921_600),
        ] {
            assert_eq!(parse_rate(text), Some(v), "{text}");
        }
        for text in ["", "k", "M", "fast", "1.2.3k", "-5", "1.0000001M", "99999999999"] {
            assert_eq!(parse_rate(text), None, "{text}");
        }
    }

    #[test]
    fn baud_command() {
        assert_eq!(parse("baud"), Ok(Command::Baud(None)));
        assert_eq!(parse("baud 1M"), Ok(Command::Baud(Some(1_000_000))));
        assert!(matches!(parse("baud 6M"), Err(CliError::BadNumber { .. })));
        assert!(matches!(parse("baud 300"), Err(CliError::BadNumber { .. })));
    }

    #[test]
    fn burst_command() {
        assert_eq!(parse("burst"), Ok(Command::Burst { count: None, rate_hz: None }));
        assert_eq!(parse("burst 200"), Ok(Command::Burst { count: Some(200), rate_hz: None }));
        assert_eq!(parse("burst 200 max"), Ok(Command::Burst { count: Some(200), rate_hz: Some(0.0) }));
        assert_eq!(parse("burst 1k 50"), Ok(Command::Burst { count: Some(1000), rate_hz: Some(50.0) }));
        assert!(matches!(parse("burst 0"), Err(CliError::BadNumber { .. })));
        assert!(matches!(parse("burst 10 lots"), Err(CliError::BadNumber { .. })));
        assert_eq!(parse("burst 10 2.5Hz"), Ok(Command::Burst { count: Some(10), rate_hz: Some(2.5) }));
        assert_eq!(parse("burst 10 20ms"), Ok(Command::Burst { count: Some(10), rate_hz: Some(50.0) }));
    }

    #[test]
    fn wizard_defaults_retry_and_cancel() {
        let defaults = BurstConfig { count: 1000, rate_hz: 250.0 };
        let mut w = BurstWizard::new(defaults);
        assert_eq!(w.answer("zero"), WizardStep::Retry("a number of frames, 1-100000 (q to cancel)"));
        assert_eq!(w.answer(""), WizardStep::Ask); // default count
        assert_eq!(w.question(), "burst speed, frames/s or max [250]");
        assert!(matches!(w.answer("99999"), WizardStep::Retry(_)));
        assert_eq!(w.answer(""), WizardStep::Done(Command::Burst { count: Some(1000), rate_hz: Some(250.0) }));
        assert_eq!(BurstWizard::new(defaults).answer("q"), WizardStep::Cancelled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Key, NodeConfig, READ_ONLY_NET, READ_ONLY_UART};

    fn set_cmd(key: &str, value: &str) -> Command {
        Command::Set { key: key.try_into().unwrap(), value: value.try_into().unwrap() }
    }

    fn type_in(editor: &mut LineEditor, bytes: &[u8]) -> Vec<EditorEvent> {
        bytes.iter().filter_map(|&b| editor.feed(b)).collect()
    }

    #[test]
    fn commands_and_aliases() {
        assert_eq!(parse("help"), Ok(Command::Help(None)));
        assert_eq!(parse("? set"), Ok(Command::Help(Some("set".try_into().unwrap()))));
        assert_eq!(parse("ls"), Ok(Command::List));
        assert_eq!(parse("st"), Ok(Command::Status));
        assert_eq!(parse("get"), Ok(Command::Config));
        assert_eq!(parse("n"), Ok(Command::NextPattern));
        assert_eq!(parse("reset"), Ok(Command::Reset(None)));
        assert_eq!(parse("reset led1"), Ok(Command::Reset(Some("led1".try_into().unwrap()))));
    }

    #[test]
    fn set_and_shortcuts() {
        assert_eq!(parse("  set   data_period_ms   1000 "), Ok(set_cmd("data_period_ms", "1000")));
        assert_eq!(parse("s led1 blink 200"), Ok(set_cmd("led1", "blink 200")));
        assert_eq!(parse("p ocean"), Ok(set_cmd("rgb.pattern", "ocean")));
        assert_eq!(parse("f 100"), Ok(set_cmd("rgb.freq_hz", "100")));
        assert_eq!(parse("c 2000"), Ok(set_cmd("rgb.cycle_ms", "2000")));
        assert_eq!(parse("b 70"), Ok(set_cmd("rgb.brightness", "70")));
        assert_eq!(parse("verbose"), Ok(set_cmd("verbose", "next")));
        assert_eq!(parse("verbose frames"), Ok(set_cmd("verbose", "frames")));
        assert_eq!(parse("v off"), Ok(set_cmd("verbose", "off")));
        assert_eq!(parse("led 2 rx"), Ok(set_cmd("led2", "rx")));
    }

    #[test]
    fn errors_have_the_expected_messages() {
        let msg = |line| parse(line).unwrap_err().to_string();
        assert_eq!(msg("dance"), "unknown command 'dance' (try `help`)");
        assert_eq!(msg("set"), "usage: set <key> <value>  (see `config` for the keys)");
        assert_eq!(msg("set rgb.freq_hz"), "missing value for `rgb.freq_hz`");
        assert_eq!(msg("bright"), "missing value for `rgb.brightness`");
        assert_eq!(msg("led 3 on"), "usage: led <1|2> <off|on|blink [ms]|button|peer|link|rx|tx>");
        assert_eq!(msg("led 1"), "missing value for `led1`");
        assert_eq!(msg("set abcdefghijklmnopqrstuvwxyz 1"), "argument too long");
        // Help never fails: an over-long topic shows the general help.
        assert_eq!(parse("help abcdefghijklmnopqrstuvwxyz"), Ok(Command::Help(None)));
    }

    /// Every example printed by `help <command>` is a valid command, and every
    /// `set` it produces is accepted by the default config.
    #[test]
    fn help_examples_work() {
        for c in COMMANDS {
            assert!(!c.examples.is_empty(), "{} has no example", c.name);
            for example in c.examples {
                let cmd = parse(example).unwrap_or_else(|e| panic!("`{example}`: {e}"));
                if let Command::Set { ref key, ref value } = cmd {
                    let mut cfg = NodeConfig::DEFAULT;
                    assert!(cfg.set(key, value).is_ok(), "`{example}` refused by config");
                }
                if let Command::Reset(Some(key)) = cmd {
                    assert!(Key::from_name(&key).is_some(), "`{example}`: unknown key");
                }
            }
        }
    }

    #[test]
    fn every_documented_name_is_a_command() {
        for c in COMMANDS {
            for name in core::iter::once(&c.name).chain(c.aliases) {
                assert!(!matches!(parse(name), Err(CliError::UnknownCommand(_))), "`{name}` not parsed");
                assert!(core::ptr::eq(find_help(name).unwrap(), c));
                // `help <topic>` looks commands up first: a name may only shadow a
                // setting it is the shortcut of (`verbose`).
                if Key::from_name(name).is_some() || READ_ONLY_UART.contains(name) || READ_ONLY_NET.contains(name) {
                    let alone = if matches!(*name, "verbose" | "v") { "next" } else { "on" };
                    assert_eq!(parse(name), Ok(set_cmd(name, alone)), "`{name}` shadows a setting");
                }
            }
            assert!(c.usage.starts_with(c.name), "{}: usage doesn't start with its name", c.name);
        }
    }

    #[test]
    fn editor_types_and_submits() {
        let mut e = LineEditor::new();
        assert_eq!(type_in(&mut e, b"ls").len(), 2);
        assert_eq!(e.line(), "ls");
        assert_eq!(e.feed(b'\r'), Some(EditorEvent::Submitted("ls".try_into().unwrap())));
        assert_eq!(e.line(), "");
        // The `\n` of a CRLF, or Enter on an empty line, does nothing.
        assert_eq!(e.feed(b'\n'), None);
    }

    #[test]
    fn editor_backspace_and_ctrl_u() {
        let mut e = LineEditor::new();
        type_in(&mut e, b"helq\x08p");
        assert_eq!(e.line(), "help");
        type_in(&mut e, b"\x7f\x7f\x7f\x7f\x7f"); // more backspaces than characters
        assert_eq!(e.line(), "");
        type_in(&mut e, b"status\x15");
        assert_eq!(e.line(), "");
    }

    #[test]
    fn editor_never_types_escape_sequences_or_control_bytes() {
        let mut e = LineEditor::new();
        // Arrows and F5 (ESC [ 1 5 ~) are keys, not text; a tab and a NUL are ignored.
        type_in(&mut e, b"\x1b[A\x1b[D\x1b[15~\t\0");
        assert_eq!(e.line(), "");
        // → at the end of the line does nothing, typing goes on.
        type_in(&mut e, b"st\x1b[Catus");
        assert_eq!(e.line(), "status");
    }

    #[test]
    fn editor_stops_at_max_input() {
        let mut e = LineEditor::new();
        let long = [b'x'; MAX_INPUT + 10];
        assert_eq!(type_in(&mut e, &long).len(), MAX_INPUT);
        assert_eq!(e.line().len(), MAX_INPUT);
    }

    #[test]
    fn editor_never_panics() {
        let mut e = LineEditor::new();
        for b in (0..=255u8).cycle().take(5000) {
            if let Some(EditorEvent::Submitted(line)) = e.feed(b) {
                let _ = parse(&line);
            }
        }
    }
}
