//! Console output that never blocks the node.
//!
//! Printing a line to the USB Serial/JTAG port is slow: the chip hands the host 64 bytes per USB
//! frame, one frame per millisecond, so a frame line takes about 3 ms, and much longer when no
//! terminal reads it. In the UART firmware that only delays the loop; here the main loop shares
//! an executor with the network tasks, so a blocking print would hold back the very packets it
//! is describing (and their timestamps). So:
//!
//! ```text
//!   term::out!(...) ──▶ PIPE (16 KB, never blocks, drops when full) ──▶ console_task ──▶ USB
//! ```
//!
//! The pipe takes bytes from anywhere ([`print`]), the task writes them to the USB FIFO a byte at
//! a time without blocking and yields while the host drains it. Panics still print directly (esp-backtrace).

use core::{
    fmt::{self, Write as _},
    sync::atomic::{AtomicU32, Ordering},
};

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, pipe::Pipe};
use embassy_time::{Duration, Timer};
use esp_hal::{Blocking, usb::usb_serial_jtag::UsbSerialJtagTx};

/// Room for about 60 frame lines, or the whole boot card. The pipe never blocks and drops what doesn't fit, in the
/// middle of whatever was being written: so the answer to a command needs room for all of it (see [`REPLY_ROOM`]),
/// and this is twice what the longest one (a `help` page, a dashboard redrawn after each of its lines) takes.
const PIPE_SIZE: usize = 16384;

/// Room the console must have before a long answer (`help`, `config`, `link`...) starts, and that everything else
/// (the trace, the dashboard) leaves free for it: the longest answer is about 4 KB, and the prompt block drawn
/// after it (live line, a dashboard of up to 14 rows) up to 4 KB more.
pub const REPLY_ROOM: usize = 8192;
static PIPE: Pipe<CriticalSectionRawMutex, PIPE_SIZE> = Pipe::new();
/// Bytes that didn't fit (the terminal isn't keeping up, or nobody is reading).
static DROPPED: AtomicU32 = AtomicU32::new(0);

/// Lines (newlines) written to the console since boot.
static LINES: AtomicU32 = AtomicU32::new(0);

/// How many lines have been printed since boot: the console uses it to tell that the terminal's screen is full.
pub fn lines_printed() -> u32 {
    LINES.load(Ordering::Relaxed)
}

/// Whether the console has room for the whole of a long answer (see [`REPLY_ROOM`]).
pub fn has_room_for_reply() -> bool {
    free() >= REPLY_ROOM
}

/// Bytes the console pipe has room for right now (the network trace prints only while there is plenty).
pub fn free() -> usize {
    PIPE.free_capacity()
}

/// Queues `text` for the console. Never blocks; what doesn't fit is dropped and counted.
pub fn write_str(text: &str) {
    LINES.fetch_add(text.bytes().filter(|b| *b == b'\n').count() as u32, Ordering::Relaxed);
    let mut rest = text.as_bytes();
    while !rest.is_empty() {
        match PIPE.try_write(rest) {
            Ok(n) => rest = &rest[n..],
            Err(_) => {
                DROPPED.fetch_add(rest.len() as u32, Ordering::Relaxed);
                return;
            }
        }
    }
}

struct Sink;

impl fmt::Write for Sink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_str(s);
        Ok(())
    }
}

/// `print!` to the console, formatted straight into the pipe (no size limit, no copy).
pub fn print(args: fmt::Arguments<'_>) {
    let _ = Sink.write_fmt(args);
}

/// The Ferrite runtime's console task: moves what is in the pipe to the USB FIFO. Returns whether it moved
/// anything (the task yields the processor when it didn't).
#[cfg(feature = "ferrite")]
pub(crate) fn pump(tx: &mut UsbSerialJtagTx<'static, Blocking>) -> bool {
    let mut chunk = [0u8; 64];
    let Ok(n) = PIPE.try_read(&mut chunk) else { return false };
    for &byte in &chunk[..n] {
        // The FIFO holds 64 bytes: when it is full, hand it to the host and let the others run.
        while tx.write_byte_nb(byte).is_err() {
            let _ = tx.flush_tx_nb();
            ferrite_esp32s31::yield_now();
        }
    }
    let _ = tx.flush_tx_nb();

    let lost = DROPPED.swap(0, Ordering::Relaxed);
    if lost > 0 {
        let mut note: heapless::String<64> = heapless::String::new();
        let _ = write!(note, "\r\n[console: {lost} bytes dropped, the terminal can't keep up]\r\n");
        write_str(&note);
    }
    true
}

/// Writes the pipe to the USB Serial/JTAG FIFO: spawn once, with the TX half of the port.
#[embassy_executor::task]
pub async fn console_task(mut tx: UsbSerialJtagTx<'static, Blocking>) -> ! {
    let mut chunk = [0u8; 64];
    loop {
        let n = PIPE.read(&mut chunk).await;
        for &byte in &chunk[..n] {
            // The FIFO holds 64 bytes: when it is full, hand it to the host and wait for room.
            while tx.write_byte_nb(byte).is_err() {
                let _ = tx.flush_tx_nb();
                Timer::after(Duration::from_micros(200)).await;
            }
        }
        let _ = tx.flush_tx_nb();

        // Say so, once there is room, if output was lost.
        let lost = DROPPED.swap(0, Ordering::Relaxed);
        if lost > 0 {
            let mut note: heapless::String<64> = heapless::String::new();
            let _ = write!(note, "\r\n[console: {lost} bytes dropped, the terminal can't keep up]\r\n");
            write_str(&note);
        }
    }
}
