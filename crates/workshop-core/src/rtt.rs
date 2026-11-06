//! Round-trip time, from the `ECH` replies of a peer in `on-rx` TX mode.
//!
//! The node remembers when it sent its last frames; when an `ECH` comes back with
//! the `seq` of one of them, the difference is the round-trip time: both UART
//! transfers plus the peer's reaction time.
//!
//! ```
//! use workshop_core::rtt::RttTracker;
//!
//! let mut rtt = RttTracker::new();
//! rtt.sent(41, 1_000);
//! rtt.sent(42, 2_000);
//! assert_eq!(rtt.echo(42, 2_350), Some(350));
//! assert_eq!(rtt.echo(7, 9_000), None); // not one of ours (or too old)
//! let stats = rtt.stats().unwrap();
//! assert_eq!((stats.min_us, stats.avg_us, stats.max_us, stats.count), (350, 350, 350, 1));
//! ```

/// How many sent frames are remembered.
pub const WINDOW: usize = 16;

/// Min / average / max round-trip time since boot, µs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RttStats {
    /// Shortest.
    pub min_us: u64,
    /// Average.
    pub avg_us: u64,
    /// Longest.
    pub max_us: u64,
    /// Number of measurements.
    pub count: u32,
}

/// Send times of the last [`WINDOW`] frames, and the measured round trips.
#[derive(Clone, Copy, Debug)]
pub struct RttTracker {
    sent: [Option<(u32, u64)>; WINDOW],
    next: usize,
    min: u64,
    max: u64,
    sum: u64,
    count: u32,
}

impl Default for RttTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RttTracker {
    /// No frame sent, no measurement.
    pub const fn new() -> Self {
        Self { sent: [None; WINDOW], next: 0, min: u64::MAX, max: 0, sum: 0, count: 0 }
    }

    /// Records that frame `seq` was sent at `now_us`.
    pub fn sent(&mut self, seq: u32, now_us: u64) {
        self.sent[self.next] = Some((seq, now_us));
        self.next = (self.next + 1) % WINDOW;
    }

    /// An `ECH` for `echo_seq` arrived: returns the round-trip time if that frame
    /// is one of the remembered ones (each frame is measured once).
    pub fn echo(&mut self, echo_seq: u32, now_us: u64) -> Option<u64> {
        let slot = self.sent.iter_mut().find(|s| s.is_some_and(|(seq, _)| seq == echo_seq))?;
        let (_, at) = slot.take()?;
        let rtt = now_us.saturating_sub(at);
        self.min = self.min.min(rtt);
        self.max = self.max.max(rtt);
        self.sum += rtt;
        self.count += 1;
        Some(rtt)
    }

    /// `None` until a first measurement.
    pub fn stats(&self) -> Option<RttStats> {
        (self.count > 0).then(|| RttStats {
            min_us: self.min,
            avg_us: self.sum / u64::from(self.count),
            max_us: self.max,
            count: self.count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_over_several_echoes() {
        let mut r = RttTracker::new();
        assert_eq!(r.stats(), None);
        for (seq, rtt) in [(1, 300), (2, 500), (3, 400)] {
            r.sent(seq, 1000 * u64::from(seq));
            assert_eq!(r.echo(seq, 1000 * u64::from(seq) + rtt), Some(rtt));
        }
        assert_eq!(r.stats(), Some(RttStats { min_us: 300, avg_us: 400, max_us: 500, count: 3 }));
    }

    #[test]
    fn each_frame_is_measured_once_and_old_ones_are_forgotten() {
        let mut r = RttTracker::new();
        r.sent(1, 0);
        assert!(r.echo(1, 10).is_some());
        assert_eq!(r.echo(1, 20), None);
        for seq in 2..2 + WINDOW as u32 {
            r.sent(seq, 0);
        }
        r.sent(100, 0); // pushes out seq 2
        assert_eq!(r.echo(2, 10), None);
        assert!(r.echo(3, 10).is_some());
    }
}
