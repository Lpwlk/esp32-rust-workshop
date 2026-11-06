//! When a frame was actually on the wire, and the time between consecutive `DAT`
//! frames, sent or received (one shared clock).
//!
//! Both directions use the same reference: **the moment the frame's last byte
//! (`\n`) finished on the wire**.
//!
//! - **Received**: the UART interrupt notes the time it read the `\n`, then
//!   corrects it ([`rx_line_end_us`]): the bytes read after it in the same batch
//!   arrived later (one byte time each), and when the interrupt fired because the
//!   line went idle, the last byte arrived one idle timeout earlier. What remains is
//!   the interrupt latency (a few µs).
//! - **Sent**: the firmware notes when it queued the frame in the TX FIFO, and adds
//!   the frame's duration on the wire ([`tx_line_end_us`]).
//!
//! The link is full-duplex: a received frame can end while one of ours is being
//! sent, so a delta can be slightly negative.
//!
//! ```
//! use workshop_core::wire::{DatClock, Dir, byte_time_ns, tx_line_end_us};
//!
//! assert_eq!(byte_time_ns(5_000_000), 2_000); // 8N1: 10 bits per byte
//! let sent_end = tx_line_end_us(1_000_000, 32, 5_000_000); // 32 bytes: +64 µs
//! assert_eq!(sent_end, 1_000_064);
//!
//! let mut clock = DatClock::new();
//! assert_eq!(clock.mark(Dir::Tx, sent_end), None); // first DAT
//! let d = clock.mark(Dir::Rx, 2_250_064).unwrap();
//! assert_eq!((d.delta_us, d.prev), (1_250_000, Dir::Tx));
//! ```

/// Direction of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Sent by this node.
    Tx,
    /// Received from the peer.
    Rx,
}

/// Time of one byte on the wire, ns (8N1: start bit + 8 data bits + stop bit).
pub fn byte_time_ns(baud: u32) -> u64 {
    10_000_000_000 / u64::from(baud.max(1))
}

/// When a received `\n` finished on the wire, µs.
///
/// - `read_at_us`: when the interrupt read the batch of bytes;
/// - `bytes_after`: bytes of that batch after the `\n` (they arrived later);
/// - `idle_symbols`: `Some(n)` when the interrupt fired because the line was idle
///   for `n` byte times (the last byte arrived that long before `read_at_us`),
///   `None` when it fired because the FIFO filled up.
pub fn rx_line_end_us(read_at_us: u64, bytes_after: usize, idle_symbols: Option<u8>, baud: u32) -> u64 {
    let byte = byte_time_ns(baud);
    let idle_ns = idle_symbols.map_or(0, |n| u64::from(n) * byte);
    let back_ns = idle_ns + bytes_after as u64 * byte;
    read_at_us.saturating_sub((back_ns + 500) / 1000)
}

/// When a frame of `len` bytes queued at `queued_at_us` (in an empty TX FIFO)
/// finishes on the wire, µs.
pub fn tx_line_end_us(queued_at_us: u64, len: usize, baud: u32) -> u64 {
    queued_at_us + (len as u64 * byte_time_ns(baud) + 500) / 1000
}

/// Time since the previous `DAT` frame, whichever its direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatDelta {
    /// µs; negative when this frame ended before the previous one (full duplex).
    pub delta_us: i64,
    /// Direction of the previous `DAT`.
    pub prev: Dir,
}

/// The shared clock of `DAT` frames: one previous event, sent or received.
#[derive(Clone, Copy, Debug, Default)]
pub struct DatClock {
    last: Option<(u64, Dir)>,
}

impl DatClock {
    /// No `DAT` seen yet.
    pub const fn new() -> Self {
        Self { last: None }
    }

    /// A `DAT` frame ended on the wire at `at_us`: returns the time since the
    /// previous one (`None` for the first).
    pub fn mark(&mut self, dir: Dir, at_us: u64) -> Option<DatDelta> {
        let previous = self.last.replace((at_us, dir));
        previous.map(|(t, prev)| DatDelta { delta_us: at_us as i64 - t as i64, prev })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_times() {
        assert_eq!(byte_time_ns(5_000_000), 2_000);
        assert_eq!(byte_time_ns(1_000_000), 10_000);
        assert_eq!(byte_time_ns(115_200), 86_805);
        assert_eq!(byte_time_ns(0), 10_000_000_000); // never divides by zero
    }

    #[test]
    fn rx_corrections() {
        // FIFO-full interrupt, `\n` is the last byte read: no correction.
        assert_eq!(rx_line_end_us(10_000, 0, None, 5_000_000), 10_000);
        // 5 bytes came after it in the batch: 5 × 2 µs earlier.
        assert_eq!(rx_line_end_us(10_000, 5, None, 5_000_000), 9_990);
        // Idle interrupt after 10 byte times: the last byte arrived 20 µs earlier.
        assert_eq!(rx_line_end_us(10_000, 0, Some(10), 5_000_000), 9_980);
        // Both, at 115200 baud: (10 + 2) × 86.8 µs.
        assert_eq!(rx_line_end_us(100_000, 2, Some(10), 115_200), 98_958);
        // Never before boot.
        assert_eq!(rx_line_end_us(5, 100, Some(10), 1200), 0);
    }

    #[test]
    fn tx_end() {
        assert_eq!(tx_line_end_us(0, 32, 5_000_000), 64);
        assert_eq!(tx_line_end_us(0, 32, 115_200), 2_778);
    }

    #[test]
    fn shared_clock_across_directions() {
        let mut c = DatClock::new();
        assert_eq!(c.mark(Dir::Tx, 1_000), None);
        assert_eq!(c.mark(Dir::Rx, 1_500), Some(DatDelta { delta_us: 500, prev: Dir::Tx }));
        assert_eq!(c.mark(Dir::Rx, 2_000), Some(DatDelta { delta_us: 500, prev: Dir::Rx }));
        // Full duplex: our frame ended before the received one we just processed.
        assert_eq!(c.mark(Dir::Tx, 1_990), Some(DatDelta { delta_us: -10, prev: Dir::Rx }));
    }
}
