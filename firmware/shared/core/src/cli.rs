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
pub const MAX_INPUT: usize = 96;

/// A parsed command line.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// `help`, or `help <command|key>`.
    Help(Option<String<24>>),
    /// `gen ...`: the traffic generator (Ethernet).
    Gen(crate::pktgen::GenCmd),
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
    /// `browse [path] [depth]`: lists what the peer has under `path` (default `/`), `depth` levels deep
    /// (default 1), see [`crate::space`].
    Browse {
        /// The branch, `/` when none was given.
        path: Option<Path>,
        /// How many levels to list, 1 to [`crate::space::MAX_BROWSE_DEPTH`].
        depth: u8,
    },
    /// `read <path>`: reads one of the peer's variables.
    Read(Path),
    /// `write <path> <value>`: writes one of the peer's variables.
    Write {
        /// The variable, e.g. `/leds/1`.
        path: Path,
        /// The new value, as text.
        value: String<24>,
    },
    /// `watch <path> [interval]`: the peer reports a variable whenever it changes.
    Watch {
        /// The variable.
        path: Path,
        /// How often the peer checks it, ms.
        interval_ms: u32,
    },
    /// `unwatch [path]`: stops watching one variable, or all of them.
    Unwatch(Option<Path>),
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
    /// `gen`: what is wrong with the command.
    Gen(&'static str),
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
            CliError::Gen(why) => write!(f, "gen: {why}"),
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

/// Longest address space path a command takes (the longest `/config/<key>` is 24 characters: room to grow).
pub const PATH_LEN: usize = 64;
/// A path of the peer's address space, as typed.
pub type Path = String<PATH_LEN>;

/// Default sampling interval of `watch`, ms.
pub const DEFAULT_WATCH_MS: u32 = 250;

fn path_arg(path: &str) -> Result<Path, CliError<'_>> {
    String::try_from(path).map_err(|_| CliError::TooLong)
}

/// `browse [path] [depth]`, the depth as `2`, `-d 2` or `--depth 2`, in any order with the path.
fn browse(args: &str) -> Result<Command, CliError<'_>> {
    const DEPTH: &str = "a depth, 1 to 4";
    let depth_of = |text: &str| match text.parse::<u8>() {
        Ok(d) if (1..=crate::space::MAX_BROWSE_DEPTH).contains(&d) => Ok(d),
        _ => Err(CliError::BadNumber { expected: DEPTH, got: "" }),
    };
    let (mut path, mut depth) = (None, None);
    let mut words = args.split_whitespace();
    while let Some(word) = words.next() {
        if matches!(word, "-d" | "--depth") {
            let value = words.next().ok_or(CliError::MissingValue("depth"))?;
            depth = Some(depth_of(value).map_err(|_| CliError::BadNumber { expected: DEPTH, got: value })?);
        } else if word.bytes().all(|b| b.is_ascii_digit()) {
            depth = Some(depth_of(word).map_err(|_| CliError::BadNumber { expected: DEPTH, got: word })?);
        } else if path.is_none() {
            path = Some(path_arg(word)?);
        } else {
            return Err(CliError::BadNumber { expected: "one path and a depth", got: word });
        }
    }
    Ok(Command::Browse { path, depth: depth.unwrap_or(1) })
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
        "info" | "about" | "boot" | "i" => Ok(Command::Info),
        "save" | "w" => Ok(Command::Save),
        "forget" | "fg" => Ok(Command::Forget),
        "reboot" | "restart" | "rb" => Ok(Command::Reboot),
        "set" | "s" => match split_first(rest) {
            ("", _) => Err(CliError::SetUsage),
            (key, value) => set(key, value),
        },
        "reset" | "r" => Ok(Command::Reset(topic()?)),
        // Shortcuts for the most used keys.
        "pattern" | "p" => set("rgb.pattern", rest),
        "freq" | "f" => set("rgb.freq_hz", rest),
        "cycle" | "c" => set("rgb.cycle_ms", rest),
        "bright" | "b" => set("rgb.brightness", rest),
        "verbose" | "v" => set("verbose", if rest.is_empty() { "next" } else { rest }),
        "tx" => set("tx_mode", rest),
        "period" | "pd" => set("data_period_ms", rest),
        // `trace` alone: next level. `trace compact`, `trace layers`, `trace inside-out`: the view. Else the level.
        "trace" | "t" | "tr" => match rest {
            "" => set("trace", "next"),
            view if crate::config::TraceView::parse(view).is_ok() => set("trace_view", view),
            level => set("trace", level),
        },
        "dump" | "d" => set("trace_bytes", rest),
        "gen" | "g" => crate::pktgen::parse_gen(rest).map(Command::Gen).map_err(CliError::Gen),
        // `dash` alone: the next view. `dash chart`, `dash mix`, `dash bars`: a view. Else a size.
        "dash" | "ds" => match rest {
            "" => set("trace_dash_view", "next"),
            view if crate::config::DashView::parse(view).is_ok() => set("trace_dash_view", view),
            size => set("trace_dash", size),
        },
        "window" | "win" => set("trace_window_s", rest),
        "link" | "net" | "lk" => Ok(Command::Link),
        "browse" | "br" | "tree" => browse(rest),
        "read" | "rd" => match rest {
            "" => Err(CliError::MissingValue("path")),
            path => Ok(Command::Read(path_arg(path)?)),
        },
        "write" | "wr" => match split_first(rest) {
            ("", _) => Err(CliError::MissingValue("path")),
            (_, "") => Err(CliError::MissingValue("value")),
            (path, value) => Ok(Command::Write {
                path: path_arg(path)?,
                value: String::try_from(value).map_err(|_| CliError::TooLong)?,
            }),
        },
        "watch" | "wa" => match split_first(rest) {
            ("", _) => Err(CliError::MissingValue("path")),
            (path, interval) => Ok(Command::Watch {
                path: path_arg(path)?,
                interval_ms: if interval.is_empty() {
                    DEFAULT_WATCH_MS
                } else {
                    // 1 ms to 1 h, as a period (250ms, 2s) or a frequency (4Hz).
                    match crate::units::parse_period_ms(interval) {
                        Some(ms) if (1.0..=3_600_000.0).contains(&ms) => ms as u32,
                        _ => {
                            return Err(CliError::BadNumber {
                                expected: "a period (250ms, 2s) or a frequency (4Hz)",
                                got: interval,
                            });
                        }
                    }
                },
            }),
        },
        "unwatch" | "uw" => Ok(Command::Unwatch(if rest.is_empty() { None } else { Some(path_arg(rest)?) })),
        "baud" | "bd" => match rest {
            "" => Ok(Command::Baud(None)),
            rate => Ok(Command::Baud(Some(in_range(
                rate,
                shared::BAUD_RANGE,
                "baud rate 1200-5000000 (or 921600, 2M, 500k...)",
            )?))),
        },
        "burst" | "bs" => {
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
    /// The peer's address space: browse, read, write, watch.
    Space,
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
    /// Width, in characters, of `name (alias, alias)`: how `help` lists the command.
    pub fn label_width(&self) -> usize {
        if self.aliases.is_empty() {
            self.name.len()
        } else {
            self.name.len() + 3 + self.aliases.iter().map(|a| a.len()).sum::<usize>() + 2 * (self.aliases.len() - 1)
        }
    }

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
        aliases: &["r"],
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
        aliases: &["fg"],
        usage: "forget",
        summary: "erase the saved settings (next boot: build config)",
        details: &[
            "The current settings stay as they are until the next reboot;",
            "`reset` then `reboot` to go back now.",
        ],
        examples: &["forget"],
        group: Group::System,
        only: None,
    },
    CmdHelp {
        name: "reboot",
        aliases: &["restart", "rb"],
        usage: "reboot",
        summary: "restart the chip (software reset)",
        details: &["Settings not saved with `save` are lost.", "The boot card then shows a software reset (`CoreSw`)."],
        examples: &["reboot"],
        group: Group::System,
        only: None,
    },
    CmdHelp {
        name: "verbose",
        aliases: &["v"],
        usage: "verbose [0|1|2|3|silent|live|frames|wire]",
        summary: "how much the console prints (= set verbose)",
        details: &[
            "0 silent  only what you ask for (replies, burst results) and what is wrong",
            "          (bad frames, frames not sent): no live line, no frames, no link events",
            "1 live    one line, updated in place, with the last frame sent or received and",
            "          the counters. Lost frames, link events, bursts and errors still scroll.",
            "2 frames  every frame on its own line, and the periodic stats line",
            "3 wire    frames plus what is behind each: the time since the peer's previous frame,",
            "          ignored button bounces and counters (UART: also the raw line, checksum",
            "          and size; on Ethernet the raw bytes are `trace`'s)",
            "`verbose` alone goes to the next level: live, frames, wire, silent, and round again.",
        ],
        examples: &["verbose", "verbose frames", "verbose silent", "verbose 3"],
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
        aliases: &["bd"],
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
        aliases: &["net", "lk"],
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
        name: "trace",
        aliases: &["t", "tr"],
        usage: "trace [0|1|2|off|events|frames|compact|layers|inside-out]",
        summary: "decode the network frames on the port (= set trace, or trace_view)",
        details: &[
            "0 off     nothing (the driver copies no frame; `dash` still counts them)",
            "1 events  one line per control frame: ARP, ICMP, TCP SYN/FIN/RST, the undecodable",
            "2 frames  every frame decoded, checksums checked, and `dump` bytes as a hex dump",
            "`trace` alone goes to the next level (frames goes back to off).",
            "Independent of `verbose`. How a frame is laid out (= set trace_view):",
            "compact     one line per frame: who to whom, flags, the node's frame",
            "layers      a header line, then Ethernet, IPv4, UDP/TCP, the node's frame, raw bytes",
            "inside-out  the node's frame first, then the layers back out to the cable",
        ],
        examples: &["trace", "trace events", "trace frames", "trace compact", "trace inside-out", "t 0"],
        group: Group::Link,
        only: Some(LinkKind::Net),
    },
    CmdHelp {
        name: "dash",
        aliases: &["ds"],
        usage: "dash [bars|chart|mix | off|mini|normal|full]",
        summary: "the traffic dashboard above the prompt (= set trace_dash[_view])",
        details: &[
            "Graphs of what is received and sent, at the bottom of the console above the",
            "prompt, redrawn in place. It counts the frames itself, with `trace` on or off.",
            "Off at boot: `dash normal` shows it. A view, and a size:",
            "bars   vertical bars per direction (▁ to █, a column per slice of time), with a",
            "       scale, a time axis and its rate (default)",
            "chart  the classic network graph: rx and tx as two lines on the same axes, bit rate",
            "       over the window, with a time axis, packets per second, peak, link use",
            "mix    the chart, and under it the protocols that went by as horizontal bars",
            "off | mini | normal | full   how many lines (0 off, 1 mini, 2 normal, 3 full)",
            "`dash` alone goes to the next view; `dash next` to the next size.",
            "`window` sets the time the graphs cover.",
        ],
        examples: &["dash", "dash chart", "dash mix", "dash full", "dash off"],
        group: Group::Link,
        only: Some(LinkKind::Net),
    },
    CmdHelp {
        name: "gen",
        aliases: &["g"],
        usage: "gen [start|stop|pause|resume|rate|size|shape|for|count] ...",
        summary: "traffic generator: UDP datagrams to the peer, finely controlled",
        details: &[
            "gen start [rate] [size] [shape] [time]   in any order; the rest stays as it was",
            "gen rate R | size S | shape P | for T | count N | stop | pause | resume | status",
            "R rate: 5000 (datagrams/s), 5kpps, or on the wire 2Mbit, 500kbit, 1Gbit.",
            "  With a shape it is the FLOOR.",
            "S size of the payload: 512B, 64-1400 (random), imix (7 small, 4 medium, 1 large)",
            "T time: 30s, 2min, forever, auto. Default auto: as long as the shape (one cycle),",
            "  10 s at least.",
            "P shape (it has a colon): the second value is the PEAK, the last is one CYCLE,",
            "  not the length of the stream:",
            "  sine:3Mbit:20s  cosine:...  triangle:...  saw:...  square:...",
            "  ramp:3Mbit:30s  steps:3Mbit:5:30s  pulse:200ms:800ms  random",
            "A wave repeats for as long as the stream runs: add `forever` (or a time).",
        ],
        examples: &[
            "gen start 2Mbit 512B 30s",
            "gen start 200kbit triangle:2Mbit:10s forever",
            "gen start 200kbit sine:2Mbit:20s forever",
            "gen start 5000 imix",
            "gen rate 50Mbit",
            "gen stop",
        ],
        group: Group::Link,
        only: Some(LinkKind::Net),
    },
    CmdHelp {
        name: "window",
        aliases: &["win"],
        usage: "window <2-3600 seconds>",
        summary: "the time the dashboard's graphs cover (= set trace_window_s)",
        details: &[
            "Each column of a graph is a slice of it: 30 s over 40 columns is 750 ms a column.",
            "Seconds, or with a unit: 30, 30s, 2min. Changing it starts the graphs over.",
        ],
        examples: &["window 30", "window 2min", "win 10s"],
        group: Group::Link,
        only: Some(LinkKind::Net),
    },
    CmdHelp {
        name: "dump",
        aliases: &["d"],
        usage: "dump <0-256>",
        summary: "raw bytes of each traced frame as a hex dump (= set trace_bytes)",
        details: &["0 shows no dump. The headers of any frame fit in 134 bytes;", "a frame is cut after 256."],
        examples: &["dump 64", "d 0", "dump 256"],
        group: Group::Link,
        only: Some(LinkKind::Net),
    },
    CmdHelp {
        name: "browse",
        aliases: &["br", "tree"],
        usage: "browse [path] [depth]",
        summary: "list what the peer has, as a tree: its address space",
        details: &[
            "Asks the peer to describe itself, one node per frame: its LEDs, buttons,",
            "statistics and settings as a tree. `browse` alone lists the root `/`;",
            "`browse /leds` lists one branch. `depth` (1 to 4, default 1) is how many",
            "levels to go down: `browse / 2` shows the whole tree (`-d 2` and `--depth 2`",
            "work too). Each line shows the node, and for a variable its type and whether",
            "it can be written (`rw`) or only read (`ro`).",
        ],
        examples: &["browse", "browse /leds", "browse / 2", "browse /config -d 1"],
        group: Group::Space,
        only: None,
    },
    CmdHelp {
        name: "read",
        aliases: &["rd"],
        usage: "read <path>",
        summary: "read one of the peer's variables",
        details: &[
            "The answer is the value, and a status: Good, or why not (BadNodeIdUnknown,",
            "BadTypeMismatch for an object...). Paths come from `browse`.",
        ],
        examples: &["read /buttons/jumper", "read /stats/lost", "read /node/uptime"],
        group: Group::Space,
        only: None,
    },
    CmdHelp {
        name: "write",
        aliases: &["wr"],
        usage: "write <path> <value>",
        summary: "write one of the peer's variables",
        details: &[
            "The peer checks the value like its own `set` does, and answers with the value now",
            "in force: what you wrote, or what stayed if it was refused (BadOutOfRange,",
            "BadTypeMismatch, BadNotWritable for a read-only variable).",
        ],
        examples: &[
            "write /leds/1 on",
            "write /leds/2 blink 200",
            "write /rgb/brightness 80",
            "write /config/verbose frames",
        ],
        group: Group::Space,
        only: None,
    },
    CmdHelp {
        name: "watch",
        aliases: &["wa"],
        usage: "watch <path> [interval]",
        summary: "the peer reports a variable whenever it changes",
        details: &[
            "The peer checks the variable every `interval` (default 250ms; 4Hz, 1s work too) and",
            "sends it only when it differs from the last one reported. A few variables can be",
            "watched at once; `unwatch` stops one, or all.",
        ],
        examples: &["watch /buttons/jumper", "watch /stats/rx 1s", "watch /node/uptime 500ms"],
        group: Group::Space,
        only: None,
    },
    CmdHelp {
        name: "unwatch",
        aliases: &["uw"],
        usage: "unwatch [path]",
        summary: "stop watching one variable, or all of them",
        details: &[],
        examples: &["unwatch /buttons/jumper", "unwatch"],
        group: Group::Space,
        only: None,
    },
    CmdHelp {
        name: "burst",
        aliases: &["bs"],
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
        aliases: &["pd"],
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
        aliases: &["about", "boot", "i"],
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

/// Pause between the sections of `help`, in ms: the full list is long, and arriving in one block it
/// scrolls past before it can be read.
pub const HELP_PAUSE_MS: u32 = 0;

/// One piece of the `help` overview, to print when [`HelpPager::poll`] returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpStep {
    /// The `n`th section (settings, shortcuts, ...), in the order of the app's list of sections.
    Section(usize),
    /// The closing hints.
    Footer,
}

/// Hands out the pieces of the `help` overview one at a time, [`HELP_PAUSE_MS`] apart, without ever
/// blocking: the main loop calls [`poll`](Self::poll) on each pass and prints what it returns. (A
/// delay inside the command would stop the frames and, on Ethernet, the network task.)
///
/// ```
/// use workshop_core::cli::{HELP_PAUSE_MS, HelpPager, HelpStep};
///
/// let mut pager = HelpPager::new(1_000, 2);
/// assert_eq!(pager.poll(1_000), Some(HelpStep::Section(0)));  // at once
/// if HELP_PAUSE_MS > 0 {
///     assert_eq!(pager.poll(1_000), None);                    // then a pause
/// }
/// assert_eq!(pager.poll(1_000 + HELP_PAUSE_MS), Some(HelpStep::Section(1)));
/// assert_eq!(pager.poll(1_000 + 2 * HELP_PAUSE_MS), Some(HelpStep::Footer));
/// assert_eq!(pager.poll(9_999), None);                        // and it's over
/// assert!(pager.is_done());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HelpPager {
    next: usize,
    sections: usize,
    due_ms: u32,
}

impl HelpPager {
    /// A pager for `sections` sections plus the footer; the first piece is due at `now_ms`.
    pub const fn new(now_ms: u32, sections: usize) -> Self {
        Self { next: 0, sections, due_ms: now_ms }
    }

    /// Whether everything was handed out.
    pub const fn is_done(&self) -> bool {
        self.next > self.sections
    }

    /// The next piece, if its time has come (`now_ms` is the node's uptime, wrapping is fine).
    pub fn poll(&mut self, now_ms: u32) -> Option<HelpStep> {
        if self.is_done() || now_ms.wrapping_sub(self.due_ms) >= 1 << 31 {
            return None;
        }
        let step = if self.next < self.sections { HelpStep::Section(self.next) } else { HelpStep::Footer };
        self.next += 1;
        self.due_ms = now_ms.wrapping_add(HELP_PAUSE_MS);
        Some(step)
    }
}

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
    fn address_space_commands() {
        let s = |t: &str| Path::try_from(t).unwrap();
        assert_eq!(parse("browse"), Ok(Command::Browse { path: None, depth: 1 }));
        assert_eq!(parse("br /leds"), Ok(Command::Browse { path: Some(s("/leds")), depth: 1 }));
        // The depth: alone, after or before the path, or as an option.
        assert_eq!(parse("browse 2"), Ok(Command::Browse { path: None, depth: 2 }));
        assert_eq!(parse("browse / 2"), Ok(Command::Browse { path: Some(s("/")), depth: 2 }));
        assert_eq!(parse("tree /config -d 3"), Ok(Command::Browse { path: Some(s("/config")), depth: 3 }));
        assert_eq!(parse("browse --depth 4 /leds"), Ok(Command::Browse { path: Some(s("/leds")), depth: 4 }));
        let msg = |line| parse(line).unwrap_err().to_string();
        assert_eq!(msg("browse / 0"), "'0': expected a depth, 1 to 4");
        assert_eq!(msg("browse / 9"), "'9': expected a depth, 1 to 4");
        assert_eq!(msg("browse -d"), "missing value for `depth`");
        assert_eq!(msg("browse /a /b"), "'/b': expected one path and a depth");
        assert_eq!(parse("read /buttons/jumper"), Ok(Command::Read(s("/buttons/jumper"))));
        // Every `/config/<key>` fits, the longest included.
        for (key, _) in crate::config::KEYS {
            let line = format!("read /config/{key}");
            assert!(parse(&line).is_ok(), "{line}");
        }
        assert_eq!(parse("rd /node/uptime"), Ok(Command::Read(s("/node/uptime"))));
        // The value is everything after the path, spaces included.
        assert_eq!(
            parse("write /leds/2 blink 200"),
            Ok(Command::Write { path: s("/leds/2"), value: String::<24>::try_from("blink 200").unwrap() })
        );
        assert_eq!(
            parse("watch /buttons/jumper"),
            Ok(Command::Watch { path: s("/buttons/jumper"), interval_ms: DEFAULT_WATCH_MS })
        );
        assert_eq!(parse("watch /stats/rx 1s"), Ok(Command::Watch { path: s("/stats/rx"), interval_ms: 1000 }));
        assert_eq!(parse("watch /stats/rx 4Hz"), Ok(Command::Watch { path: s("/stats/rx"), interval_ms: 250 }));
        assert_eq!(parse("unwatch"), Ok(Command::Unwatch(None)));
        assert_eq!(parse("unwatch /stats/rx"), Ok(Command::Unwatch(Some(s("/stats/rx")))));
        assert_eq!(parse("read"), Err(CliError::MissingValue("path")));
        assert_eq!(parse("write /leds/1"), Err(CliError::MissingValue("value")));
        assert_eq!(parse("write"), Err(CliError::MissingValue("path")));
        assert!(matches!(parse("watch /x fast"), Err(CliError::BadNumber { .. })));
        assert!(matches!(parse("watch /x 0"), Err(CliError::BadNumber { .. })));
        assert_eq!(parse(&format!("read /{}", "a/".repeat(PATH_LEN))), Err(CliError::TooLong));
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

    #[test]
    fn help_pager_spaces_the_sections_and_ends() {
        let mut p = HelpPager::new(u32::MAX - 10, 3); // across the wrap of the clock
        let mut now = u32::MAX - 10;
        let mut steps = std::vec::Vec::new();
        for _ in 0..100 {
            if let Some(s) = p.poll(now) {
                steps.push((now, s));
            }
            now = now.wrapping_add(10);
        }
        let kinds: std::vec::Vec<_> = steps.iter().map(|(_, s)| *s).collect();
        assert_eq!(kinds, [HelpStep::Section(0), HelpStep::Section(1), HelpStep::Section(2), HelpStep::Footer]);
        for pair in steps.windows(2) {
            assert!(pair[1].0.wrapping_sub(pair[0].0).checked_sub(HELP_PAUSE_MS).is_some());
        }
        assert!(p.is_done());
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
    fn trace_takes_a_level_or_a_view() {
        let set = |key: &str, value: &str| {
            Ok(Command::Set { key: key.try_into().unwrap(), value: value.try_into().unwrap() })
        };
        assert_eq!(parse("trace"), set("trace", "next"));
        assert_eq!(parse("t events"), set("trace", "events"));
        assert_eq!(parse("tr 2"), set("trace", "2"));
        for view in ["compact", "layers", "inside-out"] {
            assert_eq!(parse(&std::format!("trace {view}")), set("trace_view", view));
        }
        assert_eq!(parse("dump 64"), set("trace_bytes", "64"));
        assert_eq!(parse("d 0"), set("trace_bytes", "0"));
        assert_eq!(parse("dash"), set("trace_dash_view", "next"));
        assert_eq!(parse("ds chart"), set("trace_dash_view", "chart"));
        assert_eq!(parse("dash mix"), set("trace_dash_view", "mix"));
        assert_eq!(parse("dash full"), set("trace_dash", "full"));
        assert_eq!(parse("dash next"), set("trace_dash", "next"));
        assert_eq!(parse("ds off"), set("trace_dash", "off"));
        assert_eq!(parse("window 2min"), set("trace_window_s", "2min"));
        assert_eq!(parse("win 10"), set("trace_window_s", "10"));
    }

    #[test]
    fn short_names_exist_for_the_longer_commands() {
        for (short, long) in [
            ("r", "reset"),
            ("fg", "forget"),
            ("rb", "reboot"),
            ("bd", "baud"),
            ("wa /x", "watch /x"),
            ("uw", "unwatch"),
            ("bs", "burst"),
            ("pd 1s", "period 1s"),
            ("i", "info"),
            ("lk", "link"),
            ("h", "help"),
        ] {
            assert_eq!(parse(short), parse(long), "`{short}` is `{long}`");
        }
    }

    /// The pages of `help` have to fit a terminal of 100 columns: a line that wraps loses its columns (and, with a live
    /// line or a dashboard under it, the prompt block gets redrawn over a wrapped row). `help` lists a command as
    /// `    name (aliases)  │ summary`; `help <command>` as `    label     │ line`.
    #[test]
    fn the_help_pages_fit_a_terminal_of_100_columns() {
        const WIDTH: usize = 100;
        let label_w = COMMANDS.iter().map(CmdHelp::label_width).max().unwrap() + 2;
        for c in COMMANDS {
            let overview = 4 + label_w + 2 + c.summary.chars().count();
            assert!(overview <= WIDTH, "`help` line of {}: {overview} columns: {}", c.name, c.summary);
            let page = |text: &str| 4 + 10 + 2 + text.chars().count();
            assert!(page(c.usage) <= WIDTH, "usage of {}: {}", c.name, c.usage);
            for line in c.details.iter().chain(c.examples) {
                assert!(page(line) <= WIDTH, "`help {}`: {} columns: {line}", c.name, page(line));
            }
        }
        assert!(label_w <= 26, "the name column is {label_w} wide");
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
                    let alone = if matches!(*name, "verbose" | "v" | "trace" | "t" | "tr") { "next" } else { "on" };
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
