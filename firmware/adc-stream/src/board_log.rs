//! The log of the ADC firmware: every line is printed in colour on the USB console, kept in a short
//! history, and sent to the computer as a datagram so that `tools/adc-fft/adc_fft.py` can show it in its page.
//!
//! ```text
//!    12.345  ✓ client 192.168.77.1:50612 · 83333.3 Hz · 256 samples/datagram
//!   ▲ warning   ✗ error   ✓ good news   · information   ▪ statistics (dim)
//! ```
//!
//! A log datagram is `LOG1`, then `seq` (`u32`), the board's uptime in ms (`u32`), the [`Level`] (`u8`) and
//! the text (UTF-8, no colour codes), all little-endian. When a computer starts a stream, the board first
//! replays its history (the boot banner included), so the page shows what happened before it connected.
//!
//! The answer to a command typed in the page's terminal (`info`, `verbose`…) is an `RPL1` datagram, the
//! same layout with `seq` 0: it goes only to the computer that asked, and is not kept in the history.
//!
//! **Verbose mode** ([`set_verbose`]) turns on the [`Level::Debug`] lines. They are cheap by design: the
//! caller only counts (a few integer operations per datagram), and the firmware writes one summary line
//! per second, so the console and the network see one more short line a second, never one per sample.

use core::{
    cell::RefCell,
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering},
};

use critical_section::Mutex;
use esp_println::println;
use heapless::{Deque, String};

/// How important a line is. The number is what the datagram carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Level {
    Info = 0,
    Ok = 1,
    Warn = 2,
    Error = 3,
    /// Periodic statistics: shown dim, and replaced by the next one in the page.
    Stat = 4,
    /// The answer to a command (see [`reply`]).
    Reply = 5,
    /// Detail, only in verbose mode.
    Debug = 6,
}

/// Longest text of a line (the rest is cut).
pub const TEXT: usize = 120;
/// Lines kept for the computer that connects late.
const HISTORY: usize = 48;

#[derive(Clone)]
pub struct Line {
    pub seq: u32,
    pub ms: u32,
    pub level: Level,
    pub text: String<TEXT>,
}

struct Ring {
    lines: Deque<Line, HISTORY>,
    next_seq: u32,
}

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Turns the [`Level::Debug`] lines on or off.
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

/// Whether the [`Level::Debug`] lines are on. Callers that measure only for them check this first.
pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

static RING: Mutex<RefCell<Ring>> = Mutex::new(RefCell::new(Ring { lines: Deque::new(), next_seq: 0 }));

/// Prints one line on the console: uptime, a coloured glyph, the text.
fn print(level: Level, ms: u32, text: &str) {
    // glyph colour, glyph, text colour (ANSI)
    let (glyph_colour, glyph, text_colour) = match level {
        Level::Info => ("37", "·", "0"),
        Level::Ok => ("32", "✓", "0"),
        Level::Warn => ("33", "▲", "33"),
        Level::Error => ("31", "✗", "31"),
        Level::Stat => ("34", "▪", "90"),
        Level::Reply => ("36", "›", "0"),
        Level::Debug => ("35", "◦", "90"),
    };
    println!(
        "\x1b[90m{:>6}.{:03}\x1b[0m  \x1b[{glyph_colour}m{glyph}\x1b[0m \x1b[{text_colour}m{text}\x1b[0m",
        ms / 1000,
        ms % 1000
    );
}

/// Logs one line: prints it and keeps it. Use the [`blog!`](crate::blog) macro.
pub fn log(level: Level, args: core::fmt::Arguments<'_>) {
    if level == Level::Debug && !verbose() {
        return;
    }
    let ms = embassy_time::Instant::now().as_millis() as u32;
    let mut text = String::<TEXT>::new();
    let _ = text.write_fmt(args); // too long: cut
    print(level, ms, &text);

    critical_section::with(|cs| {
        let mut ring = RING.borrow_ref_mut(cs);
        if ring.lines.is_full() {
            ring.lines.pop_front();
        }
        let seq = ring.next_seq;
        ring.next_seq = seq.wrapping_add(1);
        let _ = ring.lines.push_back(Line { seq, ms, level, text });
    });
}

/// The answer to a command: printed on the console, and encoded as an `RPL1` datagram in `out` (returns
/// its length) for whoever asked. It is not kept in the history.
pub fn reply(level: Level, args: core::fmt::Arguments<'_>, out: &mut [u8]) -> usize {
    let ms = embassy_time::Instant::now().as_millis() as u32;
    let mut text = String::<TEXT>::new();
    let _ = text.write_fmt(args);
    print(level, ms, &text);
    encode(&Line { seq: 0, ms, level, text }, b"RPL1", out)
}

/// `blog!(Level::Ok, "link up at {} Mbit/s", speed)`
#[macro_export]
macro_rules! blog {
    ($level:expr, $($arg:tt)*) => {
        $crate::board_log::log($level, format_args!($($arg)*))
    };
}

/// The first line whose number is at least `seq` (a late reader skips what has been pushed out).
pub fn line_from(seq: u32) -> Option<Line> {
    critical_section::with(|cs| {
        let ring = RING.borrow_ref(cs);
        ring.lines.iter().find(|l| l.seq.wrapping_sub(seq) < u32::MAX / 2).cloned()
    })
}

/// The number of the oldest line kept (where a new reader starts to replay the history).
pub fn oldest_seq() -> u32 {
    critical_section::with(|cs| {
        let ring = RING.borrow_ref(cs);
        ring.lines.front().map_or(ring.next_seq, |l| l.seq)
    })
}

/// Lines in the history.
pub fn len() -> usize {
    critical_section::with(|cs| RING.borrow_ref(cs).lines.len())
}

/// Encodes a line as a datagram (`LOG1` for the log, `RPL1` for an answer); returns its length.
pub fn encode(line: &Line, magic: &[u8; 4], out: &mut [u8]) -> usize {
    out[0..4].copy_from_slice(magic);
    out[4..8].copy_from_slice(&line.seq.to_le_bytes());
    out[8..12].copy_from_slice(&line.ms.to_le_bytes());
    out[12] = line.level as u8;
    let text = line.text.as_bytes();
    out[13..13 + text.len()].copy_from_slice(text);
    13 + text.len()
}
