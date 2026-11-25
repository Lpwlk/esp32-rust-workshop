//! Network traffic over time, for the trace dashboard (`trace_dash`): how many frames and bytes crossed
//! the port in each slice of a sliding window, and the bar graphs drawn from them.
//!
//! The window (`trace_window_s`, say 30 s) is cut into as many equal slices as the graph has columns
//! (40: 750 ms each), kept in a ring. [`Traffic::record`] adds a frame to the slice it arrived in, and
//! the slices that went by without a frame are zeroed as time moves on. Only **complete** slices are
//! shown: the one being filled would make the right edge of the graph flicker.
//!
//! ```
//! use workshop_core::traffic::{Class, Traffic, spark};
//!
//! let mut t = Traffic::new();
//! t.configure(4, 4); // 4 s in 4 columns: one second each
//! t.record(100_000, false, 1500, Class::Udp); //          second 0: one frame received
//! t.record(1_500_000, true, 60, Class::Udp); //           second 1: one sent
//! t.record(1_600_000, true, 60, Class::Udp);
//! t.advance(4_100_000); //                    now it is second 4: seconds 0 to 3 are complete
//! let rx: Vec<u32> = (0..4).map(|i| t.column(i).rx.bytes).collect();
//! assert_eq!(rx, [1500, 0, 0, 0]);
//! let mut bars = String::new();
//! spark((0..4).map(|i| t.column(i).tx.bytes), 120, &mut bars).unwrap();
//! assert_eq!(bars, "·█··");
//! ```

use core::fmt;

/// What a frame carries, by a peek at its headers (no decoding: the tap runs for every frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// ARP.
    Arp,
    /// IPv4 + ICMP.
    Icmp,
    /// IPv4 + UDP.
    Udp,
    /// IPv4 + TCP.
    Tcp,
    /// IPv4, another protocol.
    OtherIp,
    /// Anything else (IPv6, LLDP, a cut frame...).
    Other,
}

/// Number of [`Class`]es.
pub const CLASSES: usize = 6;

impl Class {
    /// Position in [`Mix::classes`].
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Short name for the dashboard.
    pub const fn name(self) -> &'static str {
        match self {
            Class::Arp => "ARP",
            Class::Icmp => "ICMP",
            Class::Udp => "UDP",
            Class::Tcp => "TCP",
            Class::OtherIp => "IP",
            Class::Other => "other",
        }
    }

    /// Every class, in [`Self::index`] order.
    pub const ALL: [Class; CLASSES] = [Class::Udp, Class::Tcp, Class::Arp, Class::Icmp, Class::OtherIp, Class::Other];
}

/// Which [`Class`] a frame is, from its Ethernet and IPv4 headers (an 802.1Q tag is skipped).
pub fn classify(frame: &[u8]) -> Class {
    let ethertype = |at: usize| frame.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let (kind, ip) = match ethertype(12) {
        Some(0x8100) => (ethertype(16), 18),
        other => (other, 14),
    };
    match kind {
        Some(0x0806) => Class::Arp,
        Some(0x0800) => match frame.get(ip + 9) {
            Some(1) => Class::Icmp,
            Some(6) => Class::Tcp,
            Some(17) => Class::Udp,
            Some(_) => Class::OtherIp,
            None => Class::Other,
        },
        _ => Class::Other,
    }
}

/// Number of frame size bins (the usual RMON ones).
pub const SIZE_BINS: usize = 6;
/// Names of the bins, bytes on the wire.
pub const SIZE_BIN_NAMES: [&str; SIZE_BINS] = ["≤64", "65-127", "128-255", "256-511", "512-1023", "≥1024"];

/// The bin of a frame of `len` bytes.
pub const fn size_bin(len: usize) -> usize {
    match len {
        0..=64 => 0,
        65..=127 => 1,
        128..=255 => 2,
        256..=511 => 3,
        512..=1023 => 4,
        _ => 5,
    }
}

/// What the frames of one direction were, since the graphs started: by class and by size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mix {
    /// Frames per [`Class`] (indexed by [`Class::index`]).
    pub classes: [u32; CLASSES],
    /// Frames per size bin.
    pub sizes: [u32; SIZE_BINS],
}

impl Mix {
    /// Frames counted.
    pub fn total(&self) -> u32 {
        self.classes.iter().fold(0u32, |a, b| a.saturating_add(*b))
    }
}

/// Most columns a graph can have.
pub const MAX_COLUMNS: usize = 64;
/// Slices kept: the columns, and the one being filled.
const SLOTS: usize = MAX_COLUMNS + 1;

/// Frames and bytes, one direction, one slice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Frames.
    pub frames: u32,
    /// Bytes on the wire.
    pub bytes: u32,
}

/// What crossed the port in one slice of time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slice {
    /// Received.
    pub rx: Counts,
    /// Sent.
    pub tx: Counts,
}

/// Everything counted since the graph was configured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Frames received.
    pub rx_frames: u64,
    /// Bytes received.
    pub rx_bytes: u64,
    /// Frames sent.
    pub tx_frames: u64,
    /// Bytes sent.
    pub tx_bytes: u64,
}

/// A sliding window of [`Slice`]s.
#[derive(Clone, Debug)]
pub struct Traffic {
    slots: [Slice; SLOTS],
    columns: usize,
    slice_us: u64,
    /// Number of the slice being filled (time since boot / `slice_us`), once something has been seen.
    newest: u64,
    started: bool,
    totals: Totals,
    /// Received, sent.
    mix: [Mix; 2],
}

impl Default for Traffic {
    fn default() -> Self {
        Self::new()
    }
}

impl Traffic {
    /// An empty window of 30 columns of 1 s (call [`Self::configure`] to choose).
    pub const fn new() -> Traffic {
        Traffic {
            slots: [Slice { rx: Counts { frames: 0, bytes: 0 }, tx: Counts { frames: 0, bytes: 0 } }; SLOTS],
            columns: 30,
            slice_us: 1_000_000,
            newest: 0,
            started: false,
            totals: Totals { rx_frames: 0, rx_bytes: 0, tx_frames: 0, tx_bytes: 0 },
            mix: [Mix { classes: [0; CLASSES], sizes: [0; SIZE_BINS] }; 2],
        }
    }

    /// Covers `window_s` seconds with `columns` columns (1 to [`MAX_COLUMNS`]), and starts over: the
    /// counts and the totals are zeroed.
    pub fn configure(&mut self, window_s: u32, columns: usize) {
        let columns = columns.clamp(1, MAX_COLUMNS);
        *self = Traffic::new();
        self.columns = columns;
        self.slice_us = (u64::from(window_s.max(1)) * 1_000_000 / columns as u64).max(1);
    }

    /// Number of columns.
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Duration of one column, µs.
    pub fn slice_us(&self) -> u64 {
        self.slice_us
    }

    /// Moves the window to `at_us`: the slices that went by are empty. Time that goes backwards is ignored.
    pub fn advance(&mut self, at_us: u64) {
        let id = at_us / self.slice_us;
        if !self.started {
            self.newest = id;
            self.started = true;
            return;
        }
        if id <= self.newest {
            return;
        }
        if id - self.newest >= SLOTS as u64 {
            self.slots = [Slice::default(); SLOTS];
        } else {
            for k in self.newest + 1..=id {
                self.slots[(k % SLOTS as u64) as usize] = Slice::default();
            }
        }
        self.newest = id;
    }

    /// Counts a frame of `len` bytes of the given class that crossed the port at `at_us`, sent (`tx`) or received.
    pub fn record(&mut self, at_us: u64, tx: bool, len: usize, class: Class) {
        let mix = &mut self.mix[usize::from(tx)];
        mix.classes[class.index()] = mix.classes[class.index()].saturating_add(1);
        mix.sizes[size_bin(len)] = mix.sizes[size_bin(len)].saturating_add(1);
        self.advance(at_us);
        let slot = &mut self.slots[(self.newest % SLOTS as u64) as usize];
        let len = u32::try_from(len).unwrap_or(u32::MAX);
        let (counts, frames, bytes) = if tx {
            (&mut slot.tx, &mut self.totals.tx_frames, &mut self.totals.tx_bytes)
        } else {
            (&mut slot.rx, &mut self.totals.rx_frames, &mut self.totals.rx_bytes)
        };
        counts.frames = counts.frames.saturating_add(1);
        counts.bytes = counts.bytes.saturating_add(len);
        *frames += 1;
        *bytes += u64::from(len);
    }

    /// Column `i` of the graph, `0` the oldest and `columns() - 1` the newest **complete** slice. Call
    /// [`Self::advance`] first so that "now" is the time you draw at.
    pub fn column(&self, i: usize) -> Slice {
        let back = (self.columns - i.min(self.columns - 1)) as u64;
        match self.newest.checked_sub(back) {
            Some(id) if self.started => self.slots[(id % SLOTS as u64) as usize],
            _ => Slice::default(),
        }
    }

    /// The busiest column of the window, per direction and per unit.
    pub fn peak(&self) -> Slice {
        let mut peak = Slice::default();
        for i in 0..self.columns {
            let c = self.column(i);
            peak.rx.bytes = peak.rx.bytes.max(c.rx.bytes);
            peak.rx.frames = peak.rx.frames.max(c.rx.frames);
            peak.tx.bytes = peak.tx.bytes.max(c.tx.bytes);
            peak.tx.frames = peak.tx.frames.max(c.tx.frames);
        }
        peak
    }

    /// The sum of the last `span_us` of complete slices (at least one, at most the window), and how many
    /// slices that is: divide by `slices * slice_us()` for a rate.
    pub fn recent(&self, span_us: u64) -> (Slice, usize) {
        let n = usize::try_from(span_us / self.slice_us).unwrap_or(usize::MAX).clamp(1, self.columns);
        let mut sum = Slice::default();
        for i in self.columns - n..self.columns {
            let c = self.column(i);
            sum.rx.frames = sum.rx.frames.saturating_add(c.rx.frames);
            sum.rx.bytes = sum.rx.bytes.saturating_add(c.rx.bytes);
            sum.tx.frames = sum.tx.frames.saturating_add(c.tx.frames);
            sum.tx.bytes = sum.tx.bytes.saturating_add(c.tx.bytes);
        }
        (sum, n)
    }

    /// Per second: `count` observed over `slices` slices.
    pub fn per_second(&self, count: u32, slices: usize) -> u64 {
        u64::from(count) * 1_000_000 / (slices.max(1) as u64 * self.slice_us)
    }

    /// Everything counted since [`Self::configure`].
    pub fn totals(&self) -> Totals {
        self.totals
    }

    /// What the frames of one direction were, since [`Self::configure`].
    pub fn mix(&self, tx: bool) -> Mix {
        self.mix[usize::from(tx)]
    }
}

/// One cell of a plot `rows` characters tall: the part of a column of height `value` (of `max`) that falls in
/// row `row`, counted from the bottom. A full row is `█`, the top of a column is a bar of the right height
/// (`▁`..`▇`), above it is a space. Any `value` above zero shows at least the lowest bar of the bottom row; zero is
/// blank.
pub fn plot_cell(value: u32, max: u32, rows: usize, row: usize) -> char {
    if value == 0 || max == 0 {
        return ' ';
    }
    let levels = (rows.max(1) * 8) as u64;
    let fill = (u64::from(value) * levels).div_ceil(u64::from(max)).clamp(1, levels) as usize;
    match fill.saturating_sub(row * 8).min(8) {
        0 => ' ',
        n => BARS[n - 1],
    }
}

/// The height of a rate on a line `h` rows tall, 0 (the bottom row) to `h - 1`. Anything above zero is above the
/// bottom row: a trickle must not look like silence.
pub fn line_level(value: u32, max: u32, h: usize) -> u8 {
    if value == 0 || max == 0 {
        return 0;
    }
    let steps = (h - 1) as u64;
    (u64::from(value) * steps).div_ceil(u64::from(max)).clamp(1, steps) as u8
}

/// What the line of one slice does in the row `row` (from the bottom): it comes in from the left at the height of
/// this slice and goes on to the right at the height of the next.
pub fn line_glyph(from: u8, to: u8, row: u8) -> Option<char> {
    if from == to {
        return (row == from).then_some('─');
    }
    let (lo, hi) = (from.min(to), from.max(to));
    match row {
        r if r == from => Some(if from > to { '╮' } else { '╯' }),
        r if r == to => Some(if from > to { '╰' } else { '╭' }),
        r if r > lo && r < hi => Some('│'),
        _ => None,
    }
}

/// The eight heights of a bar.
const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Draws `values` as bars, scaled so that `max` is the full height. Any value above zero shows at least the
/// lowest bar (a trickle must not look like silence); zero is a dot.
pub fn spark(values: impl Iterator<Item = u32>, max: u32, out: &mut impl fmt::Write) -> fmt::Result {
    for v in values {
        if v == 0 || max == 0 {
            out.write_char('·')?;
        } else {
            let level = (u64::from(v) * 8).div_ceil(u64::from(max)).clamp(1, 8) as usize;
            out.write_char(BARS[level - 1])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{string::String, vec::Vec};

    use super::*;

    fn bytes(t: &Traffic, tx: bool) -> Vec<u32> {
        (0..t.columns()).map(|i| if tx { t.column(i).tx.bytes } else { t.column(i).rx.bytes }).collect()
    }

    #[test]
    fn frames_land_in_their_slice() {
        let mut t = Traffic::new();
        t.configure(4, 4);
        assert_eq!(t.slice_us(), 1_000_000);
        t.record(100, false, 100, Class::Udp);
        t.record(900_000, false, 50, Class::Udp);
        t.record(1_000_000, false, 7, Class::Udp); // exactly the next second
        t.record(2_500_000, true, 60, Class::Udp);
        t.advance(4_000_001);
        assert_eq!(bytes(&t, false), [150, 7, 0, 0]);
        assert_eq!(bytes(&t, true), [0, 0, 60, 0]);
        assert_eq!(t.column(0).rx.frames, 2);
        assert_eq!(
            t.totals(),
            Totals { rx_frames: 3, rx_bytes: 157, tx_frames: 1, tx_bytes: 60 },
            "totals count everything, in or out of the window"
        );
    }

    #[test]
    fn the_slice_being_filled_is_not_shown() {
        let mut t = Traffic::new();
        t.configure(4, 4);
        t.record(10, false, 100, Class::Udp); // second 0, still filling
        assert_eq!(bytes(&t, false), [0, 0, 0, 0]);
        t.advance(1_000_000);
        assert_eq!(bytes(&t, false), [0, 0, 0, 100], "complete now: the newest column");
    }

    #[test]
    fn the_window_slides_and_forgets() {
        let mut t = Traffic::new();
        t.configure(4, 4);
        t.record(0, false, 10, Class::Udp);
        t.advance(4_000_000);
        assert_eq!(bytes(&t, false), [10, 0, 0, 0]);
        t.advance(5_000_000);
        assert_eq!(bytes(&t, false), [0, 0, 0, 0], "one more second: the frame left the window");
        // A silence longer than the whole ring clears everything, without looping over each slice.
        t.record(5_100_000, false, 5, Class::Udp);
        t.advance(1_000_000_000_000);
        assert_eq!(bytes(&t, false), [0, 0, 0, 0]);
        assert_eq!(t.totals().rx_bytes, 15);
    }

    #[test]
    fn a_ring_wrap_does_not_resurrect_old_counts() {
        let mut t = Traffic::new();
        t.configure(2, 2);
        for sec in 0..200u64 {
            t.record(sec * 1_000_000 + 1, false, sec as usize + 1, Class::Udp);
        }
        t.advance(200_000_000);
        // Seconds 198 and 199 are the two complete columns.
        assert_eq!(bytes(&t, false), [199, 200]);
    }

    #[test]
    fn late_stamps_go_in_the_newest_slice() {
        let mut t = Traffic::new();
        t.configure(4, 4);
        t.record(3_000_000, false, 1, Class::Udp);
        t.record(1_000_000, false, 2, Class::Udp); // time went backwards by two seconds
        t.advance(4_000_000);
        assert_eq!(bytes(&t, false), [0, 0, 0, 3]);
    }

    #[test]
    fn configure_starts_over() {
        let mut t = Traffic::new();
        t.configure(4, 4);
        t.record(0, true, 10, Class::Udp);
        t.configure(8, 4);
        assert_eq!(t.slice_us(), 2_000_000);
        assert_eq!(t.totals(), Totals::default());
        assert_eq!(t.peak(), Slice::default());
        t.configure(1000, 500);
        assert_eq!(t.columns(), MAX_COLUMNS, "capped");
        t.configure(0, 0);
        assert_eq!((t.columns(), t.slice_us()), (1, 1_000_000));
    }

    #[test]
    fn rates_and_peak() {
        let mut t = Traffic::new();
        t.configure(10, 10);
        for sec in 0..10u64 {
            t.record(sec * 1_000_000, false, 1000 * (sec as usize + 1), Class::Udp);
        }
        t.record(2_000_000, true, 40, Class::Udp);
        t.advance(10_000_000);
        assert_eq!(t.peak().rx.bytes, 10_000);
        assert_eq!(t.peak().tx.bytes, 40);
        // The last 2 s: 9000 + 10000 bytes in 2 s.
        let (sum, n) = t.recent(2_000_000);
        assert_eq!((sum.rx.bytes, n), (19_000, 2));
        assert_eq!(t.per_second(sum.rx.bytes, n), 9500);
        // A span shorter than a slice still counts one, and longer than the window all of it.
        assert_eq!(t.recent(1).1, 1);
        assert_eq!(t.recent(u64::MAX).1, 10);
    }

    #[test]
    fn frames_are_classified_by_their_headers() {
        let eth = |ethertype: [u8; 2], rest: &[u8]| {
            let mut f = vec![0u8; 12];
            f.extend(ethertype);
            f.extend(rest);
            f
        };
        let ip = |proto: u8| {
            let mut h = vec![0x45, 0, 0, 40, 0, 0, 0, 0, 64, proto];
            h.extend([0; 10]);
            h
        };
        assert_eq!(classify(&eth([8, 6], &[0; 28])), Class::Arp);
        assert_eq!(classify(&eth([8, 0], &ip(17))), Class::Udp);
        assert_eq!(classify(&eth([8, 0], &ip(6))), Class::Tcp);
        assert_eq!(classify(&eth([8, 0], &ip(1))), Class::Icmp);
        assert_eq!(classify(&eth([8, 0], &ip(89))), Class::OtherIp);
        assert_eq!(classify(&eth([0x86, 0xDD], &[0; 40])), Class::Other);
        assert_eq!(classify(&eth([8, 0], &ip(17)[..5])), Class::Other, "an IPv4 header cut before its protocol");
        assert_eq!(classify(&[0; 5]), Class::Other);
        // Behind an 802.1Q tag.
        let mut vlan = vec![0u8; 12];
        vlan.extend([0x81, 0x00, 0, 5, 8, 0]);
        vlan.extend(ip(6));
        assert_eq!(classify(&vlan), Class::Tcp);
        for (i, c) in Class::ALL.iter().enumerate() {
            assert!(c.index() < CLASSES, "{i}");
        }
    }

    #[test]
    fn the_mix_counts_classes_and_sizes_per_direction() {
        let mut t = Traffic::new();
        t.configure(4, 4);
        t.record(0, false, 60, Class::Arp);
        t.record(0, false, 1500, Class::Udp);
        t.record(0, false, 1500, Class::Udp);
        t.record(0, true, 64, Class::Tcp);
        t.record(0, true, 65, Class::Tcp);
        let (rx, tx) = (t.mix(false), t.mix(true));
        assert_eq!((rx.classes[Class::Udp.index()], rx.classes[Class::Arp.index()], rx.total()), (2, 1, 3));
        assert_eq!(rx.sizes, [1, 0, 0, 0, 0, 2]);
        assert_eq!((tx.classes[Class::Tcp.index()], tx.sizes), (2, [1, 1, 0, 0, 0, 0]));
        t.configure(4, 4);
        assert_eq!(t.mix(false), Mix::default(), "starting over clears it");
        assert_eq!(
            [size_bin(64), size_bin(65), size_bin(127), size_bin(128), size_bin(1023), size_bin(1024)],
            [0, 1, 1, 2, 4, 5]
        );
    }

    #[test]
    fn a_plot_is_made_of_stacked_cells() {
        // Two rows tall: sixteen levels. Half of the maximum fills the bottom row and leaves the top one empty.
        assert_eq!((plot_cell(50, 100, 2, 0), plot_cell(50, 100, 2, 1)), ('█', ' '));
        assert_eq!((plot_cell(100, 100, 2, 0), plot_cell(100, 100, 2, 1)), ('█', '█'));
        // 75 %: the bottom row is full, the top one half full.
        assert_eq!((plot_cell(75, 100, 2, 0), plot_cell(75, 100, 2, 1)), ('█', '▄'));
        // A trickle shows the lowest bar; nothing is blank.
        assert_eq!((plot_cell(1, 100, 3, 0), plot_cell(1, 100, 3, 1)), ('▁', ' '));
        assert_eq!((plot_cell(0, 100, 3, 0), plot_cell(0, 100, 3, 1), plot_cell(5, 0, 3, 0)), (' ', ' ', ' '));
        assert_eq!(plot_cell(500, 100, 1, 0), '█', "above the maximum is clamped");
    }

    #[test]
    fn bars_scale_to_the_maximum() {
        let draw = |values: &[u32], max| {
            let mut s = String::new();
            spark(values.iter().copied(), max, &mut s).unwrap();
            s
        };
        assert_eq!(draw(&[0, 1, 50, 100], 100), "·▁▄█");
        assert_eq!(draw(&[1, 13, 26, 38, 51, 63, 76, 88], 100), "▁▂▃▄▅▆▇█", "all eight heights");
        assert_eq!(draw(&[5, 5], 0), "··", "nothing at all: dots, not a division by zero");
        assert_eq!(draw(&[200], 100), "█", "above the maximum is clamped");
        assert_eq!(draw(&[], 10), "");
    }

    #[test]
    fn a_line_steps_between_slices() {
        // Flat: a dash on the row of the level only.
        assert_eq!((line_glyph(2, 2, 2), line_glyph(2, 2, 1)), (Some('─'), None));
        // Up from 1 to 3: enters at the bottom (`╯`), rises through 2 (`│`), leaves at the top (`╭`).
        assert_eq!([1, 2, 3].map(|r| line_glyph(1, 3, r)), [Some('╯'), Some('│'), Some('╭')]);
        assert_eq!((line_glyph(1, 3, 0), line_glyph(1, 3, 4)), (None, None));
        // Down from 3 to 1: the mirror image.
        assert_eq!([3, 2, 1].map(|r| line_glyph(3, 1, r)), [Some('╮'), Some('│'), Some('╰')]);
    }

    #[test]
    fn a_trickle_is_above_silence_on_a_line() {
        assert_eq!(
            (line_level(0, 100, 5), line_level(1, 100, 5), line_level(100, 100, 5), line_level(500, 100, 5)),
            (0, 1, 4, 4)
        );
        assert_eq!(line_level(5, 0, 5), 0);
    }
}
