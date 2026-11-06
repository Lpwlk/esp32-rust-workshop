//! Link speed change, agreed by both nodes (CLI `baud`).
//!
//! Both nodes must use the same baud rate, so a change is a handshake carried by
//! `BAU` frames ([`Payload::Baud`]):
//!
//! 1. the requester sends `BAU REQ <rate>` at the current speed;
//! 2. the peer answers `BAU ACK <rate>`, waits until it has left its UART, and
//!    switches ([`BaudAction::AckThenSwitch`]);
//! 3. the requester switches when it receives the `ACK` ([`BaudAction::Switch`]).
//!
//! After a switch, the first valid frame confirms that the new speed works. If none
//! arrives in time (bad wiring for that speed, a lost `ACK`...), each node goes back
//! to the **boot** speed on its own ([`BaudAction::Revert`]): both know it, so they
//! meet again there.
//!
//! ```
//! use workshop_core::baud::{BaudAction, BaudControl};
//! use workshop_core::protocol::Payload;
//!
//! let (mut a, mut b) = (BaudControl::new(5_000_000), BaudControl::new(5_000_000));
//! a.request(1_000_000, 0).unwrap();                       // A sends BAU REQ
//! let req = Payload::Baud { baud: 1_000_000, ack: false };
//! assert_eq!(b.on_frame(&req, 10), Some(BaudAction::AckThenSwitch(1_000_000)));
//! let ack = Payload::Baud { baud: 1_000_000, ack: true };
//! assert_eq!(a.on_frame(&ack, 20), Some(BaudAction::Switch(1_000_000)));
//! assert_eq!((a.current(), b.current()), (1_000_000, 1_000_000));
//! ```
//!
//! Times are microseconds since boot.

use crate::protocol::Payload;

/// How long the requester waits for the `ACK`.
pub const ACK_TIMEOUT_US: u64 = 500_000;
/// Shortest wait for a first frame after a switch before going back to the boot
/// speed (the caller may give a longer one, e.g. a few peer periods).
pub const MIN_CONFIRM_TIMEOUT_US: u64 = 2_000_000;

/// What the firmware must do after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaudAction {
    /// The peer asked for this speed: send `BAU ACK` at the current speed, let it
    /// leave the UART, then switch.
    AckThenSwitch(u32),
    /// Our request was acknowledged: switch now.
    Switch(u32),
    /// A valid frame arrived at the new speed: the switch is confirmed.
    Confirmed(u32),
    /// The peer never acknowledged our request; nothing changed.
    NoAck(u32),
    /// No valid frame after a switch: go back to the boot speed.
    Revert {
        /// Speed that didn't work.
        from: u32,
        /// Boot speed, switch to it.
        to: u32,
    },
}

/// Why [`BaudControl::request`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaudError {
    /// Already at that speed.
    Same,
    /// A request or a switch is still being confirmed.
    Busy,
}

/// Speed-change state of one node (see the [module docs](self)).
#[derive(Clone, Copy, Debug)]
pub struct BaudControl {
    boot: u32,
    current: u32,
    /// Our request waiting for its `ACK`: `(baud, sent at)`.
    pending: Option<(u32, u64)>,
    /// Switched at this time, not confirmed by a valid frame yet.
    unconfirmed_since: Option<u64>,
}

impl BaudControl {
    /// Starts at the boot speed.
    pub const fn new(boot: u32) -> Self {
        Self { boot, current: boot, pending: None, unconfirmed_since: None }
    }

    /// Speed in use.
    pub fn current(&self) -> u32 {
        self.current
    }

    /// Speed used after a reboot or a failed switch.
    pub fn boot(&self) -> u32 {
        self.boot
    }

    /// A request or a switch is in progress.
    pub fn is_busy(&self) -> bool {
        self.pending.is_some() || self.unconfirmed_since.is_some()
    }

    /// Starts a change to `baud`; on `Ok` the caller sends `BAU REQ baud`.
    pub fn request(&mut self, baud: u32, now_us: u64) -> Result<(), BaudError> {
        if self.is_busy() {
            return Err(BaudError::Busy);
        }
        if baud == self.current {
            return Err(BaudError::Same);
        }
        self.pending = Some((baud, now_us));
        Ok(())
    }

    /// Call for every valid received frame.
    pub fn on_frame(&mut self, payload: &Payload, now_us: u64) -> Option<BaudAction> {
        match *payload {
            Payload::Baud { baud, ack: false } => {
                // The peer's request wins over ours, if both asked at once.
                self.pending = None;
                self.switch(baud, now_us);
                Some(BaudAction::AckThenSwitch(baud))
            }
            Payload::Baud { baud, ack: true } if self.pending.is_some_and(|(b, _)| b == baud) => {
                self.pending = None;
                self.switch(baud, now_us);
                Some(BaudAction::Switch(baud))
            }
            // A stray ACK (not ours, or late): nothing to do, but it proves the link works.
            _ => self.unconfirmed_since.take().map(|_| BaudAction::Confirmed(self.current)),
        }
    }

    /// Call regularly. `confirm_timeout_us`: how long to wait for a first frame
    /// after a switch (at least [`MIN_CONFIRM_TIMEOUT_US`] is used).
    pub fn tick(&mut self, now_us: u64, confirm_timeout_us: u64) -> Option<BaudAction> {
        if let Some((baud, since)) = self.pending
            && now_us.saturating_sub(since) > ACK_TIMEOUT_US
        {
            self.pending = None;
            return Some(BaudAction::NoAck(baud));
        }
        if let Some(since) = self.unconfirmed_since
            && now_us.saturating_sub(since) > confirm_timeout_us.max(MIN_CONFIRM_TIMEOUT_US)
        {
            self.unconfirmed_since = None;
            if self.current != self.boot {
                let from = self.current;
                self.current = self.boot;
                return Some(BaudAction::Revert { from, to: self.boot });
            }
        }
        None
    }

    fn switch(&mut self, baud: u32, now_us: u64) {
        self.current = baud;
        self.unconfirmed_since = Some(now_us);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT: u32 = 5_000_000;
    const REQ: Payload = Payload::Baud { baud: 1_000_000, ack: false };
    const ACK: Payload = Payload::Baud { baud: 1_000_000, ack: true };
    const DAT: Payload = Payload::Data { pressed: false, events: 0, period_us: 2_500_000 };

    #[test]
    fn handshake_then_confirmation() {
        let (mut a, mut b) = (BaudControl::new(BOOT), BaudControl::new(BOOT));
        a.request(1_000_000, 0).unwrap();
        assert_eq!(b.on_frame(&REQ, 100), Some(BaudAction::AckThenSwitch(1_000_000)));
        assert_eq!(a.on_frame(&ACK, 200), Some(BaudAction::Switch(1_000_000)));
        assert!(a.is_busy() && b.is_busy());
        assert_eq!(a.on_frame(&DAT, 300), Some(BaudAction::Confirmed(1_000_000)));
        assert_eq!(b.on_frame(&DAT, 300), Some(BaudAction::Confirmed(1_000_000)));
        assert!(!a.is_busy() && !b.is_busy());
        // Confirmed: no revert later.
        assert_eq!(a.tick(60_000_000, 0), None);
        assert_eq!(a.current(), 1_000_000);
    }

    #[test]
    fn no_ack_keeps_the_speed() {
        let mut a = BaudControl::new(BOOT);
        a.request(1_000_000, 0).unwrap();
        assert_eq!(a.tick(ACK_TIMEOUT_US, 0), None);
        assert_eq!(a.tick(ACK_TIMEOUT_US + 1, 0), Some(BaudAction::NoAck(1_000_000)));
        assert_eq!(a.current(), BOOT);
        assert!(!a.is_busy());
    }

    #[test]
    fn silent_link_after_switch_reverts_to_boot_speed() {
        let mut b = BaudControl::new(BOOT);
        b.on_frame(&REQ, 0);
        assert_eq!(b.tick(MIN_CONFIRM_TIMEOUT_US, 0), None);
        assert_eq!(b.tick(MIN_CONFIRM_TIMEOUT_US + 1, 0), Some(BaudAction::Revert { from: 1_000_000, to: BOOT }));
        assert_eq!(b.current(), BOOT);
        // A longer timeout from the caller is honoured.
        let mut b = BaudControl::new(BOOT);
        b.on_frame(&REQ, 0);
        assert_eq!(b.tick(3_000_000, 5_000_000), None);
    }

    #[test]
    fn requests_are_refused_when_pointless_or_busy() {
        let mut a = BaudControl::new(BOOT);
        assert_eq!(a.request(BOOT, 0), Err(BaudError::Same));
        a.request(1_000_000, 0).unwrap();
        assert_eq!(a.request(2_000_000, 1), Err(BaudError::Busy));
    }

    #[test]
    fn foreign_ack_is_ignored() {
        let mut a = BaudControl::new(BOOT);
        a.request(2_000_000, 0).unwrap();
        assert_eq!(a.on_frame(&ACK, 10), None); // ACK for 1 Mbaud, we asked 2 Mbaud
        assert_eq!(a.current(), BOOT);
    }

    #[test]
    fn crossed_requests_end_on_the_peer_speed() {
        // Both asked at once: each takes the other's REQ, so they end up swapped;
        // the confirmation timeout then brings both back to the boot speed.
        let mut a = BaudControl::new(BOOT);
        a.request(2_000_000, 0).unwrap();
        assert_eq!(a.on_frame(&REQ, 10), Some(BaudAction::AckThenSwitch(1_000_000)));
        assert!(a.is_busy());
    }
}
