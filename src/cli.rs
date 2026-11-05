//! Mini command line on the USB Serial/JTAG port (the one `espflash monitor` uses).
//!
//! `espflash monitor` puts the host terminal in raw mode and forwards each key
//! press, so the firmware echoes characters itself and handles backspace.
//! The prompt stays on the last line; logs are printed above it (see `term`).

use esp_hal::{
    Blocking,
    peripherals::USB_DEVICE,
    usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx},
};
use heapless::String;

use crate::term;

pub const MAX_INPUT: usize = 48;

pub enum Command {
    /// `help`, or `help <command|key>`.
    Help(Option<String<24>>),
    List,
    Status,
    /// Show every setting, its current value and whether it differs from the build.
    Config,
    /// `set <key> <value>`; the shortcuts (`pattern`, `freq`, `led 1`...) map to it.
    Set { key: String<24>, value: String<MAX_INPUT> },
    /// Restore one key, or everything, to the compiled-in config.
    Reset(Option<String<24>>),
    NextPattern,
}

pub struct Cli {
    rx: UsbSerialJtagRx<'static, Blocking>,
    // Output goes through esp-println; the TX half is only kept so the driver stays alive.
    _tx: UsbSerialJtagTx<'static, Blocking>,
    line: String<MAX_INPUT>,
    /// Inside an ANSI escape sequence (arrow keys...), which we ignore.
    escape: u8,
}

impl Cli {
    pub fn new(usb: USB_DEVICE<'static>) -> Self {
        let (rx, tx) = UsbSerialJtag::new(usb).split();
        term::prompt_enable();
        Self { rx, _tx: tx, line: String::new(), escape: 0 }
    }

    /// Processes pending key presses. Returns a command when Enter completes a valid line.
    pub fn poll(&mut self) -> Option<Command> {
        while let Ok(byte) = self.rx.read_byte() {
            match (self.escape, byte) {
                (0, 0x1b) => self.escape = 1,
                (1, b'[') => self.escape = 2,
                (1, _) => self.escape = 0,
                (2, 0x40..=0x7e) => self.escape = 0, // end of CSI sequence
                (2, _) => {}
                (_, b'\r' | b'\n') => {
                    if self.line.is_empty() {
                        continue;
                    }
                    let line = core::mem::take(&mut self.line);
                    term::prompt_set("");
                    term::cli_echo(&line);
                    if let Some(cmd) = parse(&line) {
                        return Some(cmd);
                    }
                }
                (_, 0x08 | 0x7f) => {
                    self.line.pop();
                    term::prompt_set(&self.line);
                }
                (_, 0x15) => {
                    // Ctrl+U: clear the line.
                    self.line.clear();
                    term::prompt_set("");
                }
                (_, 0x20..=0x7e) if self.line.push(byte as char).is_ok() => term::prompt_set(&self.line),
                _ => {}
            }
        }
        None
    }
}

/// Splits `"word rest of line"` into `("word", "rest of line")`.
fn split_first(text: &str) -> (&str, &str) {
    let text = text.trim();
    match text.find(char::is_whitespace) {
        Some(i) => (&text[..i], text[i..].trim()),
        None => (text, ""),
    }
}

fn set(key: &str, value: &str) -> Option<Command> {
    if value.is_empty() {
        term::cli_error(format_args!("missing value for `{key}`"));
        return None;
    }
    Some(Command::Set { key: String::try_from(key).ok()?, value: String::try_from(value).ok()? })
}

fn parse(line: &str) -> Option<Command> {
    let (cmd, rest) = split_first(line);
    match cmd {
        "help" | "h" | "?" => Some(Command::Help(if rest.is_empty() { None } else { String::try_from(rest).ok() })),
        "list" | "ls" => Some(Command::List),
        "status" | "st" => Some(Command::Status),
        "config" | "cfg" | "get" => Some(Command::Config),
        "next" | "n" => Some(Command::NextPattern),
        "set" | "s" => {
            let (key, value) = split_first(rest);
            if key.is_empty() {
                term::cli_error(format_args!("usage: set <key> <value>  (see `config` for the keys)"));
                return None;
            }
            set(key, value)
        }
        "reset" => {
            if rest.is_empty() {
                Some(Command::Reset(None))
            } else {
                Some(Command::Reset(Some(String::try_from(rest).ok()?)))
            }
        }
        // Shortcuts for the most used keys.
        "pattern" | "p" => set("rgb.pattern", rest),
        "freq" | "f" => set("rgb.freq_hz", rest),
        "cycle" | "c" => set("rgb.cycle_ms", rest),
        "bright" | "b" => set("rgb.brightness", rest),
        "verbose" | "v" => set("verbose", if rest.is_empty() { "on" } else { rest }),
        "led" | "l" => {
            let (index, mode) = split_first(rest);
            match index {
                "1" => set("led1", mode),
                "2" => set("led2", mode),
                _ => {
                    term::cli_error(format_args!("usage: led <1|2> <off|on|blink [ms]|button|peer|link|rx|tx>"));
                    None
                }
            }
        }
        _ => {
            term::cli_error(format_args!("unknown command '{cmd}' (try `help`)"));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Help: one table feeds both `help` (summaries) and `help <cmd>` (details).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Settings,
    Shortcuts,
    Info,
}

pub struct CmdHelp {
    pub name: &'static str,
    /// Other names accepted for the command.
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub summary: &'static str,
    pub details: &'static [&'static str],
    pub examples: &'static [&'static str],
    pub group: Group,
}

pub const COMMANDS: &[CmdHelp] = &[
    CmdHelp {
        name: "config",
        aliases: &["cfg", "get"],
        usage: "config",
        summary: "all settings: current value, build value, read-only ones",
        details: &[
            "Lists every setting of this node. Values changed at runtime are shown",
            "in yellow next to their build value (from src/bin/uart_node_*.rs).",
            "Read-only settings (name, pins, baud rate) need a rebuild to change.",
        ],
        examples: &["config"],
        group: Group::Settings,
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
        examples: &["set data_period_ms 1000", "set rgb.pattern ocean", "set led1 blink 200"],
        group: Group::Settings,
    },
    CmdHelp {
        name: "reset",
        aliases: &[],
        usage: "reset [key]",
        summary: "back to the build value (one key, or all)",
        details: &["Without a key, restores every setting changed with `set`."],
        examples: &["reset rgb.brightness", "reset"],
        group: Group::Settings,
    },
    CmdHelp {
        name: "verbose",
        aliases: &["v"],
        usage: "verbose [on|off]",
        summary: "verbose console (= set verbose)",
        details: &[
            "Adds, for each frame: the raw line on the wire and its size; for each",
            "received frame: time since the previous one from the peer. Also shows",
            "ignored button bounces, and byte counters + RX buffer peak in the",
            "stats line. `verbose` alone turns it on.",
        ],
        examples: &["verbose", "verbose off"],
        group: Group::Shortcuts,
    },
    CmdHelp {
        name: "pattern",
        aliases: &["p"],
        usage: "pattern <name|1-5>",
        summary: "RGB LED pattern (= set rgb.pattern)",
        details: &["Name or number from `list`, which shows a color preview of each."],
        examples: &["pattern viridis", "p 3"],
        group: Group::Shortcuts,
    },
    CmdHelp {
        name: "next",
        aliases: &["n"],
        usage: "next",
        summary: "next RGB LED pattern",
        details: &["Cycles through the patterns in `list` order."],
        examples: &["next"],
        group: Group::Shortcuts,
    },
    CmdHelp {
        name: "freq",
        aliases: &["f"],
        usage: "freq <1-200>",
        summary: "RGB LED refresh rate, Hz (= set rgb.freq_hz)",
        details: &[
            "How often the LED color is recomputed: smoothness, not speed.",
            "To make the fade faster, lower `cycle` instead.",
        ],
        examples: &["freq 100"],
        group: Group::Shortcuts,
    },
    CmdHelp {
        name: "cycle",
        aliases: &["c"],
        usage: "cycle <200-60000>",
        summary: "duration of one fade loop, ms (= set rgb.cycle_ms)",
        details: &["Lower = faster fade."],
        examples: &["cycle 2000"],
        group: Group::Shortcuts,
    },
    CmdHelp {
        name: "bright",
        aliases: &["b"],
        usage: "bright <0-100>",
        summary: "RGB LED brightness, % (= set rgb.brightness)",
        details: &["Gamma-corrected: 50 % looks about half as bright as 100 %."],
        examples: &["bright 70"],
        group: Group::Shortcuts,
    },
    CmdHelp {
        name: "led",
        aliases: &["l"],
        usage: "led <1|2> <mode>",
        summary: "breadboard LED mode (= set led1 / set led2)",
        details: &[
            "off, on        always off / on",
            "blink [ms]     blinks, full period in ms (default 500)",
            "button         on while this node's button is pressed",
            "peer           on while the peer's button is pressed",
            "link           on while the link is up",
            "rx, tx         short flash on each received / sent frame",
        ],
        examples: &["led 1 peer", "led 2 blink 200"],
        group: Group::Shortcuts,
    },
    CmdHelp {
        name: "list",
        aliases: &["ls"],
        usage: "list",
        summary: "RGB patterns with a color preview",
        details: &["The current pattern is marked with ▶."],
        examples: &["list"],
        group: Group::Info,
    },
    CmdHelp {
        name: "status",
        aliases: &["st"],
        usage: "status",
        summary: "current state of the RGB LED and breadboard LEDs",
        details: &[],
        examples: &["status"],
        group: Group::Info,
    },
    CmdHelp {
        name: "help",
        aliases: &["h", "?"],
        usage: "help [command|key]",
        summary: "this list, or details on one command or setting",
        details: &[],
        examples: &["help set", "help data_period_ms"],
        group: Group::Info,
    },
];

/// Finds a command by name or alias.
pub fn find_help(name: &str) -> Option<&'static CmdHelp> {
    COMMANDS.iter().find(|c| c.name == name || c.aliases.contains(&name))
}
