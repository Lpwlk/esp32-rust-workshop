//! Lost-frame and peer-restart detection from sequence numbers.
//!
//! Each node numbers its frames (`DAT` and `EVT` share the counter), wrapping
//! after `u32::MAX`. Comparing a frame's number with the previous one tells:
//!
//! ```
//! use workshop_core::seq::{check, SeqCheck};
//!
//! assert_eq!(check(None, 41), SeqCheck::First);
//! assert_eq!(check(Some(41), 42), SeqCheck::InOrder);
//! assert_eq!(check(Some(42), 45), SeqCheck::Lost(2)); // 43 and 44 missing
//! assert_eq!(check(Some(45), 0), SeqCheck::Restarted); // the peer rebooted
//! assert_eq!(check(Some(u32::MAX), 0), SeqCheck::InOrder); // wrap-around
//! ```

/// Result of [`check`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeqCheck {
    /// First frame seen from this peer.
    First,
    /// The expected next number.
    InOrder,
    /// This many frames were lost in between.
    Lost(u32),
    /// The number went back (or repeated): the peer restarted.
    Restarted,
}

/// Larger forward jumps are taken as a restart rather than billions of lost frames.
const MAX_GAP: u32 = u32::MAX / 2;

/// Compares `seq` with the previous number received from the same peer.
pub fn check(prev: Option<u32>, seq: u32) -> SeqCheck {
    let Some(prev) = prev else { return SeqCheck::First };
    match seq.wrapping_sub(prev) {
        1 => SeqCheck::InOrder,
        gap @ 2..=MAX_GAP => SeqCheck::Lost(gap - 1),
        _ => SeqCheck::Restarted,
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn duplicate_is_a_restart() {
        assert_eq!(check(Some(7), 7), SeqCheck::Restarted);
    }

    #[test]
    fn losses_across_the_wrap() {
        assert_eq!(check(Some(u32::MAX - 1), 1), SeqCheck::Lost(2));
        assert_eq!(check(Some(u32::MAX), 5), SeqCheck::Lost(5));
    }

    proptest! {
        #[test]
        fn counts_exactly_the_skipped_frames(prev in any::<u32>(), lost in 0u32..1_000_000) {
            let seq = prev.wrapping_add(lost).wrapping_add(1);
            let expected = if lost == 0 { SeqCheck::InOrder } else { SeqCheck::Lost(lost) };
            prop_assert_eq!(check(Some(prev), seq), expected);
        }

        #[test]
        fn restart_from_zero_is_detected(prev in 1u32..MAX_GAP) {
            prop_assert_eq!(check(Some(prev), 0), SeqCheck::Restarted);
        }
    }
}
