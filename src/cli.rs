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

use crate::{fx, term};

pub const MAX_INPUT: usize = 48;

pub enum Command {
    Help,
    List,
    Status,
    Pattern(usize),
    NextPattern,
    Freq(u32),
    Cycle(u32),
    Bright(u8),
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

fn parse(line: &str) -> Option<Command> {
    let mut words = line.split_whitespace();
    let cmd = words.next()?;
    let arg = words.next();
    let number = |range: (u32, u32), unit: &str| -> Option<u32> {
        match arg.and_then(|a| a.parse::<u32>().ok()) {
            Some(v) if (range.0..=range.1).contains(&v) => Some(v),
            _ => {
                term::cli_error(format_args!("expected a number {}-{} {unit}", range.0, range.1));
                None
            }
        }
    };

    Some(match cmd {
        "help" | "h" | "?" => Command::Help,
        "list" | "ls" => Command::List,
        "status" | "st" => Command::Status,
        "next" | "n" => Command::NextPattern,
        "pattern" | "p" => {
            let Some(name) = arg else {
                term::cli_error(format_args!("usage: pattern <name|1-{}>", fx::PATTERNS.len()));
                return None;
            };
            let index = match name.parse::<usize>() {
                Ok(n) if (1..=fx::PATTERNS.len()).contains(&n) => Some(n - 1),
                _ => fx::find(name),
            };
            match index {
                Some(i) => Command::Pattern(i),
                None => {
                    term::cli_error(format_args!("unknown pattern '{name}' (try `list`)"));
                    return None;
                }
            }
        }
        "freq" | "f" => Command::Freq(number(fx::FREQ_RANGE, "Hz")?),
        "cycle" | "c" => Command::Cycle(number(fx::CYCLE_RANGE, "ms")?),
        "bright" | "b" => Command::Bright(number((0, 100), "%")? as u8),
        _ => {
            term::cli_error(format_args!("unknown command '{cmd}' (try `help`)"));
            return None;
        }
    })
}
