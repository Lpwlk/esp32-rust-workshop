//! The network trace (`trace` in the CLI): a tap on the Ethernet driver, and the queue that carries
//! what it saw to the main loop.
//!
//! ```text
//!   EMAC ──▶ driver (Embassy: SpeedTap, Ferrite: EthDevice) ──▶ smoltcp
//!                    │ tap(): first 256 bytes, direction, time
//!                    ▼
//!                 QUEUE (12 frames, never blocks, counts what it drops)
//!                    │
//!                    ▼  main loop: poll_trace() decodes (workshop_core::dissect) and prints (term)
//! ```
//!
//! The tap also counts every frame into per-slice totals ([`snapshot`]) for the dashboard's bar graphs.
//!
//! The tap runs in the network task, in the path of every packet, so it does as little as it can:
//! with `trace off` it is one atomic load. It decodes nothing for printing: the main loop owns the console,
//! and formatting a frame takes longer than the frame does on the wire. With `trace events` it does decode
//! enough to keep only the control frames, so the queue isn't filled with the data it would throw away.

use core::{
    cell::{Cell, RefCell},
    sync::atomic::{AtomicU8, AtomicU32, Ordering},
};

use critical_section::Mutex;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use workshop_core::{
    config::{TRACE_BYTES_MAX, Trace},
    dissect,
    traffic::{MAX_COLUMNS, Mix, Slice, Totals, Traffic, classify},
};

/// A frame the tap kept.
pub struct Captured {
    /// Sent (`true`) or received.
    pub tx: bool,
    /// When it crossed the driver, µs since boot.
    pub at_us: u64,
    /// Its size on the wire, bytes.
    pub wire_len: u16,
    /// Its first bytes (up to [`TRACE_BYTES_MAX`]).
    pub data: heapless::Vec<u8, { TRACE_BYTES_MAX as usize }>,
}

/// Frames waiting for the main loop. The loop prints a couple per pass and only while the console has room,
/// so a burst fills this and the rest is counted in [`take_dropped`].
const DEPTH: usize = 12;
static QUEUE: Channel<CriticalSectionRawMutex, Captured, DEPTH> = Channel::new();
static LEVEL: AtomicU8 = AtomicU8::new(0);
static DROPPED: AtomicU32 = AtomicU32::new(0);
/// Frames dropped since the graphs started, for the dashboard.
static DROPPED_TOTAL: AtomicU32 = AtomicU32::new(0);

/// What crossed the port, per slice of time, for the dashboard's graphs. Counted for every frame while the
/// trace is on, whatever the level keeps (a graph of only the control frames would say nothing).
static TRAFFIC: Mutex<RefCell<Traffic>> = Mutex::new(RefCell::new(Traffic::new()));
/// The window and columns of [`TRAFFIC`], to restart it only when they change.
static WINDOW_S: AtomicU32 = AtomicU32::new(0);
static COLUMNS: AtomicU32 = AtomicU32::new(0);
/// Whether anything is counted for a dashboard (columns > 0).
static DASH_ON: AtomicU8 = AtomicU8::new(0);
/// When the dashboard was last redrawn, µs since boot.
static LAST_DASH_US: Mutex<Cell<u64>> = Mutex::new(Cell::new(0));
/// How often the dashboard is looked at, µs (it is only redrawn if a row changed).
const DASH_EVERY_US: u64 = 500_000;

/// Sets what the tap keeps (the `trace` setting). Switching off empties the queue; switching on starts the
/// graphs over.
pub fn set_level(level: Trace) {
    let before = LEVEL.swap(level as u8, Ordering::Relaxed);
    if level == Trace::Off {
        while QUEUE.try_receive().is_ok() {}
    } else if before == Trace::Off as u8 {
        restart_traffic();
    }
}

/// Sets the graphs: `columns` of them (0: no dashboard) covering `window_s` seconds. Starts them over only
/// if that changed.
pub fn set_dash(window_s: u16, columns: usize) {
    DASH_ON.store(u8::from(columns > 0), Ordering::Relaxed);
    let (w, c) = (u32::from(window_s), columns as u32);
    if WINDOW_S.swap(w, Ordering::Relaxed) != w || COLUMNS.swap(c, Ordering::Relaxed) != c {
        restart_traffic();
    }
    // Draw (or clear) at the next pass.
    critical_section::with(|cs| LAST_DASH_US.borrow(cs).set(0));
}

fn restart_traffic() {
    let (window, columns) = (WINDOW_S.load(Ordering::Relaxed), COLUMNS.load(Ordering::Relaxed) as usize);
    critical_section::with(|cs| TRAFFIC.borrow_ref_mut(cs).configure(window, columns));
    DROPPED_TOTAL.store(0, Ordering::Relaxed);
}

/// Whether it is time to redraw the dashboard (and the timer is rearmed if so).
pub fn dash_due() -> bool {
    let now = crate::link::now_us();
    critical_section::with(|cs| {
        let last = LAST_DASH_US.borrow(cs);
        if now.wrapping_sub(last.get()) < DASH_EVERY_US {
            return false;
        }
        last.set(now);
        true
    })
}

/// A copy of what the dashboard draws from, taken at one instant (so nothing is formatted with interrupts
/// masked).
pub struct Snapshot {
    /// The graph, oldest column first.
    pub columns: heapless::Vec<Slice, MAX_COLUMNS>,
    /// Duration of one column, µs.
    pub slice_us: u64,
    /// The busiest column, per direction.
    pub peak: Slice,
    /// The last second, and how many columns that is.
    pub recent: (Slice, usize),
    /// Everything counted since the graphs started.
    pub totals: Totals,
    /// What the frames were (by protocol and size), received then sent.
    pub mix: [Mix; 2],
    /// Frames the trace had no room to show since then.
    pub dropped: u32,
}

impl Snapshot {
    /// Per second: `count` seen over [`Self::recent`]'s columns.
    pub fn per_second(&self, count: u32) -> u64 {
        u64::from(count) * 1_000_000 / (self.recent.1.max(1) as u64 * self.slice_us)
    }

    /// Per second: `count` seen in one column.
    pub fn per_second_of_slice(&self, count: u32) -> u64 {
        u64::from(count) * 1_000_000 / self.slice_us
    }
}

/// Moves the graphs to now and copies them.
pub fn snapshot() -> Snapshot {
    let now = crate::link::now_us();
    critical_section::with(|cs| {
        let mut t = TRAFFIC.borrow_ref_mut(cs);
        t.advance(now);
        Snapshot {
            columns: (0..t.columns()).map(|i| t.column(i)).collect(),
            slice_us: t.slice_us(),
            peak: t.peak(),
            recent: t.recent(1_000_000),
            totals: t.totals(),
            mix: [t.mix(false), t.mix(true)],
            dropped: DROPPED_TOTAL.load(Ordering::Relaxed),
        }
    })
}

fn level() -> Trace {
    match LEVEL.load(Ordering::Relaxed) {
        0 => Trace::Off,
        1 => Trace::Events,
        _ => Trace::Frames,
    }
}

/// Called by the driver for every frame that crosses it (`tx`: leaving), with the frame as the MAC
/// sees it. Keeps a copy of the start of it when the trace wants it.
#[inline]
pub fn tap(tx: bool, frame: &[u8]) {
    let level = level();
    if level == Trace::Off {
        return;
    }
    let now = crate::link::now_us();
    if DASH_ON.load(Ordering::Relaxed) != 0 {
        let class = classify(frame);
        critical_section::with(|cs| TRAFFIC.borrow_ref_mut(cs).record(now, tx, frame.len(), class));
    }
    let kept = &frame[..frame.len().min(usize::from(TRACE_BYTES_MAX))];
    if level == Trace::Events && !dissect::dissect(kept, frame.len()).is_ok_and(|p| p.is_control() || p.is_damaged()) {
        return;
    }
    let captured = Captured {
        tx,
        at_us: now,
        wire_len: frame.len().min(usize::from(u16::MAX)) as u16,
        data: heapless::Vec::from_slice(kept).unwrap_or_default(),
    };
    if QUEUE.try_send(captured).is_err() {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// The oldest captured frame, if any.
pub fn take() -> Option<Captured> {
    QUEUE.try_receive().ok()
}

/// Frames the tap had to drop since the last call (the queue was full).
pub fn take_dropped() -> u32 {
    let n = DROPPED.swap(0, Ordering::Relaxed);
    DROPPED_TOTAL.fetch_add(n, Ordering::Relaxed);
    n
}
