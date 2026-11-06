//! Debouncing of the jumper "button", independent of the GPIO.
//!
//! The firmware's GPIO interrupt only records *that* the line moved, and when. The
//! main loop then drives a [`Debouncer`] in three steps:
//!
//! 1. [`Debouncer::ready`]: nothing is evaluated while a lockout runs;
//! 2. [`Debouncer::settled`]: the line must have been quiet for the debounce time
//!    since the last raw edge;
//! 3. [`Debouncer::settle`]: the level read then is compared with the last
//!    reported one: a real change is an [`ButtonUpdate::Edge`] (and starts a
//!    lockout), a bounce that came back to the same level is
//!    [`ButtonUpdate::Ignored`].
//!
//! A jumper sliding into a breadboard can lose contact for longer than the
//! debounce time; the lockout filters that, and since the level is read again when
//! it ends, a real change is never missed.
//!
//! Times are microseconds since boot, as `u64`.

use crate::protocol::Edge;

/// A debounced button event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonEvent {
    /// Direction of the change (`Falling` = pressed).
    pub edge: Edge,
    /// Raw interrupts counted for this event (1 = clean contact, more = bounces).
    pub raw_edges: u32,
}

/// What [`Debouncer::settle`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonUpdate {
    /// A real, debounced level change.
    Edge(ButtonEvent),
    /// Edges that ended on the level already reported (bounce or lockout): ignored.
    Ignored {
        /// Raw interrupts seen.
        raw_edges: u32,
    },
}

/// Debounce + lockout state machine (see the [module docs](self)).
#[derive(Clone, Copy, Debug)]
pub struct Debouncer {
    pressed: bool,
    debounce_us: u64,
    lockout_us: u64,
    lockout_until: Option<u64>,
}

impl Debouncer {
    /// `pressed` is the level read at start-up (line low = pressed).
    pub const fn new(pressed: bool, debounce_us: u64, lockout_us: u64) -> Self {
        Self { pressed, debounce_us, lockout_us, lockout_until: None }
    }

    /// Changes the filter timings; takes effect on the next edge.
    pub fn set_timing(&mut self, debounce_us: u64, lockout_us: u64) {
        self.debounce_us = debounce_us;
        self.lockout_us = lockout_us;
    }

    /// Last reported state.
    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    /// Step 1: `false` while the lockout after an accepted edge is running.
    pub fn ready(&mut self, now_us: u64) -> bool {
        match self.lockout_until {
            Some(until) if now_us < until => false,
            _ => {
                self.lockout_until = None;
                true
            }
        }
    }

    /// Step 2: whether the line has been quiet long enough since its last raw edge.
    pub fn settled(&self, last_edge_us: u64, now_us: u64) -> bool {
        now_us.saturating_sub(last_edge_us) >= self.debounce_us
    }

    /// Step 3: the line has settled on `level_low` after `raw_edges` interrupts.
    pub fn settle(&mut self, level_low: bool, raw_edges: u32, now_us: u64) -> ButtonUpdate {
        if level_low == self.pressed {
            return ButtonUpdate::Ignored { raw_edges };
        }
        self.pressed = level_low;
        self.lockout_until = Some(now_us + self.lockout_us);
        ButtonUpdate::Edge(ButtonEvent { edge: if level_low { Edge::Falling } else { Edge::Rising }, raw_edges })
    }
}

/// A button read by polling instead of an interrupt (the board's BOOT button):
/// the main loop passes the level it reads, the same [`Debouncer`] decides.
///
/// ```
/// use workshop_core::button::{ButtonUpdate, PolledButton};
/// use workshop_core::protocol::Edge;
///
/// let mut boot = PolledButton::new(false, 20_000, 100_000); // 20 ms debounce
/// assert_eq!(boot.update(true, 0), None); // pressed: wait for it to settle
/// assert_eq!(boot.update(true, 10_000), None);
/// let Some(ButtonUpdate::Edge(ev)) = boot.update(true, 20_000) else { panic!() };
/// assert_eq!(ev.edge, Edge::Falling);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct PolledButton {
    debouncer: Debouncer,
    raw_low: bool,
    last_edge_us: Option<u64>,
    raw_edges: u32,
}

impl PolledButton {
    /// `pressed`: the level read at start-up (line low = pressed).
    pub const fn new(pressed: bool, debounce_us: u64, lockout_us: u64) -> Self {
        Self {
            debouncer: Debouncer::new(pressed, debounce_us, lockout_us),
            raw_low: pressed,
            last_edge_us: None,
            raw_edges: 0,
        }
    }

    /// Feeds the level read now; returns an update once the line has settled.
    pub fn update(&mut self, level_low: bool, now_us: u64) -> Option<ButtonUpdate> {
        if level_low != self.raw_low {
            self.raw_low = level_low;
            self.raw_edges += 1;
            self.last_edge_us = Some(now_us);
        }
        if !self.debouncer.ready(now_us) {
            return None;
        }
        let last = self.last_edge_us?;
        if !self.debouncer.settled(last, now_us) {
            return None;
        }
        self.last_edge_us = None;
        let raw_edges = core::mem::take(&mut self.raw_edges);
        Some(self.debouncer.settle(level_low, raw_edges, now_us))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;

    /// The firmware's GPIO interrupt + main loop, simulated: the line is driven by
    /// a list of `(time, level_low)` changes and polled every millisecond.
    struct Sim {
        deb: Debouncer,
        level_low: bool,
        last_edge: Option<u64>,
        raw_edges: u32,
    }

    impl Sim {
        fn new() -> Self {
            Self { deb: Debouncer::new(false, 30 * MS, 150 * MS), level_low: false, last_edge: None, raw_edges: 0 }
        }

        /// Runs until `end_ms`, applying `changes`; returns `(time_ms, update)` for each update.
        fn run(&mut self, changes: &[(u64, bool)], end_ms: u64) -> Vec<(u64, ButtonUpdate)> {
            let mut out = Vec::new();
            for t_ms in 0..=end_ms {
                let now = t_ms * MS;
                for &(_, low) in changes.iter().filter(|(at, _)| *at == t_ms) {
                    if low != self.level_low {
                        self.level_low = low;
                        self.raw_edges += 1;
                        self.last_edge = Some(now);
                    }
                }
                if !self.deb.ready(now) {
                    continue;
                }
                if self.last_edge.is_some_and(|t| self.deb.settled(t, now)) {
                    self.last_edge = None;
                    let raw = core::mem::take(&mut self.raw_edges);
                    out.push((t_ms, self.deb.settle(self.level_low, raw, now)));
                }
            }
            out
        }
    }

    fn edge(edge: Edge, raw_edges: u32) -> ButtonUpdate {
        ButtonUpdate::Edge(ButtonEvent { edge, raw_edges })
    }

    #[test]
    fn clean_press_and_release() {
        let mut sim = Sim::new();
        let updates = sim.run(&[(10, true), (500, false)], 1000);
        assert_eq!(updates, [(40, edge(Edge::Falling, 1)), (530, edge(Edge::Rising, 1))]);
    }

    #[test]
    fn bounces_give_a_single_edge() {
        let mut sim = Sim::new();
        // Contact chatters for 8 ms, then holds.
        let bouncy = [(10, true), (11, false), (13, true), (14, false), (16, true), (18, false), (18, true)];
        let updates = sim.run(&bouncy, 300);
        assert_eq!(updates, [(48, edge(Edge::Falling, 7))]);
        assert!(sim.deb.is_pressed());
    }

    #[test]
    fn glitch_back_to_the_same_level_is_ignored() {
        let mut sim = Sim::new();
        let updates = sim.run(&[(10, true), (12, false)], 200);
        assert_eq!(updates, [(42, ButtonUpdate::Ignored { raw_edges: 2 })]);
        assert!(!sim.deb.is_pressed());
    }

    #[test]
    fn contact_lost_during_lockout_is_filtered() {
        let mut sim = Sim::new();
        // Pressed at 10; loses contact 60 ms later for 50 ms (> debounce, < lockout).
        let updates = sim.run(&[(10, true), (100, false), (150, true)], 400);
        assert_eq!(updates[0], (40, edge(Edge::Falling, 1)));
        // Evaluated when the lockout ends (40 + 150): back to "pressed" -> ignored.
        assert_eq!(updates[1..], [(190, ButtonUpdate::Ignored { raw_edges: 2 })]);
    }

    #[test]
    fn release_during_lockout_is_reported_when_it_ends() {
        let mut sim = Sim::new();
        let updates = sim.run(&[(10, true), (60, false)], 400);
        assert_eq!(updates, [(40, edge(Edge::Falling, 1)), (190, edge(Edge::Rising, 1))]);
    }

    #[test]
    fn new_timings_apply_to_the_next_edge() {
        let mut sim = Sim::new();
        sim.deb.set_timing(5 * MS, 0);
        let updates = sim.run(&[(10, true), (20, false)], 100);
        assert_eq!(updates, [(15, edge(Edge::Falling, 1)), (25, edge(Edge::Rising, 1))]);
    }

    #[test]
    fn polled_button_press_bounce_release() {
        let mut b = PolledButton::new(false, 20 * MS, 100 * MS);
        let mut updates = Vec::new();
        // Pressed at 5 ms with two bounces, released at 400 ms, a glitch at 700 ms.
        let level = |t: u64| matches!(t, 5..=7 | 9..=399) || t == 700;
        for t in 0..=1000 {
            if let Some(u) = b.update(level(t), t * MS) {
                updates.push((t, u));
            }
        }
        let edges: Vec<_> = updates
            .iter()
            .filter_map(|(t, u)| match u {
                ButtonUpdate::Edge(e) => Some((*t, e.edge, e.raw_edges)),
                ButtonUpdate::Ignored { .. } => None,
            })
            .collect();
        assert_eq!(edges, [(29, Edge::Falling, 3), (420, Edge::Rising, 1)]);
        assert!(matches!(updates.last(), Some((721, ButtonUpdate::Ignored { raw_edges: 2 }))), "{updates:?}");
    }
}
