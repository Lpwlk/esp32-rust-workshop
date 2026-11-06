//! Bursts of `BST` frames (CLI `burst`): scheduling on the sender, statistics on
//! the receiver.
//!
//! ```
//! use workshop_core::burst::{BurstRx, BurstTx};
//!
//! // Sender: 3 frames back to back (rate 0).
//! let mut tx = BurstTx::new(7, 3, 0.0, 0);
//! let mut rx = BurstRx::new("node-A", 7, 3, 0);
//! let mut t = 0;
//! while let Some(index) = tx.next(t) {
//!     tx.sent(40); // bytes of the encoded frame
//!     rx.frame(index, 40, t + 100);
//!     t += 80;
//! }
//! assert!(tx.is_done() && rx.is_complete());
//! let s = rx.summary();
//! assert_eq!((s.received, s.lost, s.bytes), (3, 0, 120));
//! ```
//!
//! Times are microseconds since boot.

use heapless::String;

/// The receiver closes an incomplete burst after this much silence.
pub const IDLE_TIMEOUT_US: u64 = 300_000;

/// Bytes and the time they took: gives the throughput.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Throughput {
    /// Bytes on the wire, `\n` included.
    pub bytes: u64,
    /// From the first to the last frame, µs.
    pub duration_us: u64,
}

impl Throughput {
    /// Bytes per second (0 if the duration is 0).
    pub fn bytes_per_s(&self) -> u64 {
        (self.bytes * 1_000_000).checked_div(self.duration_us).unwrap_or(0)
    }

    /// Share of the line capacity used, in tenths of a percent (8N1: 10 bits per byte).
    pub fn line_permille(&self, baud: u32) -> u64 {
        let capacity = u64::from(baud) / 10;
        (self.bytes_per_s() * 1000).checked_div(capacity).unwrap_or(0)
    }
}

/// Sender side: which frame to send next, and when.
#[derive(Clone, Copy, Debug)]
pub struct BurstTx {
    /// Burst number (goes in the frame's `seq` field).
    pub id: u32,
    /// Frames in the burst.
    pub count: u32,
    /// Frames per second (decimals allowed), 0 = back to back.
    pub rate_hz: f32,
    /// Time between frames, ns (0 = back to back): integer math for the schedule.
    interval_ns: u64,
    /// Frames sent so far.
    pub sent: u32,
    bytes: u64,
    start_us: u64,
    last_us: u64,
}

impl BurstTx {
    /// A burst starting at `now_us`.
    pub fn new(id: u32, count: u32, rate_hz: f32, now_us: u64) -> Self {
        let interval_ns = if rate_hz > 0.0 { (1e9 / f64::from(rate_hz)) as u64 } else { 0 };
        Self { id, count, rate_hz, interval_ns, sent: 0, bytes: 0, start_us: now_us, last_us: now_us }
    }

    /// Index of the frame to send now, or `None` (done, or not due yet). Frame `i`
    /// is due at `start + i / rate`, so a late loop catches up instead of drifting.
    pub fn next(&mut self, now_us: u64) -> Option<u32> {
        if self.is_done() {
            return None;
        }
        if self.interval_ns != 0 {
            let due = self.start_us + u64::from(self.sent) * self.interval_ns / 1000;
            if now_us < due {
                return None;
            }
        }
        self.last_us = now_us;
        Some(self.sent)
    }

    /// The frame returned by [`Self::next`] was sent, `bytes` long.
    pub fn sent(&mut self, bytes: usize) {
        self.sent += 1;
        self.bytes += bytes as u64;
    }

    /// All frames sent.
    pub fn is_done(&self) -> bool {
        self.sent >= self.count
    }

    /// What was sent, from the first to the last frame.
    pub fn throughput(&self) -> Throughput {
        Throughput { bytes: self.bytes, duration_us: self.last_us - self.start_us }
    }
}

/// Receiver side: counts the frames of one burst.
#[derive(Clone, Debug)]
pub struct BurstRx {
    /// Sender's name.
    pub node: String<12>,
    /// Burst number.
    pub id: u32,
    /// Announced number of frames.
    pub count: u32,
    received: u32,
    /// Corrupted lines received while the burst was running.
    pub bad: u32,
    last_index: Option<u32>,
    bytes: u64,
    first_us: u64,
    last_us: u64,
}

/// Result of a received burst.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BurstSummary {
    /// Announced frames.
    pub count: u32,
    /// Valid frames received.
    pub received: u32,
    /// `count - received`.
    pub lost: u32,
    /// Corrupted lines seen during the burst.
    pub bad: u32,
    /// Bytes received.
    pub bytes: u64,
    /// From the first to the last frame received.
    pub throughput: Throughput,
}

impl BurstRx {
    /// First frame of burst `id` from `node`, received at `now_us`.
    pub fn new(node: &str, id: u32, count: u32, now_us: u64) -> Self {
        let mut name = String::new();
        let _ = name.push_str(&node[..node.len().min(12)]);
        Self {
            node: name,
            id,
            count,
            received: 0,
            bad: 0,
            last_index: None,
            bytes: 0,
            first_us: now_us,
            last_us: now_us,
        }
    }

    /// Whether a `BST` frame belongs to this burst.
    pub fn matches(&self, node: &str, id: u32) -> bool {
        self.id == id && self.node == node
    }

    /// A valid frame of this burst.
    pub fn frame(&mut self, index: u32, bytes: usize, now_us: u64) {
        if self.last_index.is_some_and(|last| index <= last) {
            return; // duplicate (can't happen on a UART, but don't count it twice)
        }
        self.received += 1;
        self.bytes += bytes as u64;
        self.last_index = Some(index);
        self.last_us = now_us;
    }

    /// The last frame of the burst arrived.
    pub fn is_complete(&self) -> bool {
        self.last_index == Some(self.count - 1)
    }

    /// Nothing received for [`IDLE_TIMEOUT_US`]: the end was lost.
    pub fn is_stale(&self, now_us: u64) -> bool {
        now_us.saturating_sub(self.last_us) > IDLE_TIMEOUT_US
    }

    /// Counts so far.
    pub fn summary(&self) -> BurstSummary {
        BurstSummary {
            count: self.count,
            received: self.received,
            lost: self.count - self.received,
            bad: self.bad,
            bytes: self.bytes,
            throughput: Throughput { bytes: self.bytes, duration_us: self.last_us - self.first_us },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paced_burst_follows_its_schedule() {
        let mut tx = BurstTx::new(1, 4, 100.0, 1_000); // one frame every 10 ms
        assert_eq!(tx.next(1_000), Some(0));
        tx.sent(50);
        assert_eq!(tx.next(5_000), None); // not due yet
        assert_eq!(tx.next(11_000), Some(1));
        tx.sent(50);
        // Late loop: frames 2 and 3 are both due, sent right away (no drift).
        assert_eq!(tx.next(40_000), Some(2));
        tx.sent(50);
        assert_eq!(tx.next(40_000), Some(3));
        tx.sent(50);
        assert!(tx.is_done());
        assert_eq!(tx.next(50_000), None);
        assert_eq!(tx.throughput(), Throughput { bytes: 200, duration_us: 39_000 });
    }

    #[test]
    fn decimal_rate_schedule() {
        let mut tx = BurstTx::new(1, 3, 2.5, 0); // one frame every 400 ms
        assert_eq!(tx.next(0), Some(0));
        tx.sent(10);
        assert_eq!(tx.next(399_999), None);
        assert_eq!(tx.next(400_000), Some(1));
    }

    #[test]
    fn losses_are_counted() {
        let mut rx = BurstRx::new("node-B", 3, 10, 0);
        for i in [0, 1, 2, 5, 6, 9] {
            rx.frame(i, 60, u64::from(i) * 100);
        }
        rx.frame(6, 60, 1000); // duplicate, ignored
        assert!(rx.is_complete());
        let s = rx.summary();
        assert_eq!((s.received, s.lost, s.bytes), (6, 4, 360));
        assert_eq!(s.throughput.duration_us, 900);
    }

    #[test]
    fn incomplete_burst_goes_stale() {
        let mut rx = BurstRx::new("node-B", 3, 10, 0);
        rx.frame(0, 60, 0);
        assert!(!rx.is_complete() && !rx.is_stale(IDLE_TIMEOUT_US));
        assert!(rx.is_stale(IDLE_TIMEOUT_US + 1));
        assert!(rx.matches("node-B", 3) && !rx.matches("node-A", 3) && !rx.matches("node-B", 4));
    }

    #[test]
    fn throughput_and_line_share() {
        // 5 Mbaud 8N1 = 500 000 bytes/s.
        let t = Throughput { bytes: 250_000, duration_us: 1_000_000 };
        assert_eq!(t.bytes_per_s(), 250_000);
        assert_eq!(t.line_permille(5_000_000), 500);
        assert_eq!(Throughput { bytes: 10, duration_us: 0 }.bytes_per_s(), 0);
    }
}
