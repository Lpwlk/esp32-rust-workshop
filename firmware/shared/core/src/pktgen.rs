//! A traffic generator for the Ethernet firmware (CLI `gen`): a stream of UDP datagrams to the peer, with a rate,
//! frame sizes, a shape over time and a limit that you choose, change while it runs, pause and stop; and, on the
//! receiving node, what arrived: how many, how many were lost, how many came out of order, and the jitter.
//!
//! It is separate from `burst`, which measures the link with the node's own frames: this one fills the network
//! with whatever load you describe. Nothing here touches a socket: [`Generator::poll`] says **what to send now**
//! and the firmware's network task sends it, so the schedule is tested on the host.
//!
//! ```
//! use workshop_core::pktgen::{Generator, Limit, Patch, Rate, GenCmd};
//!
//! let mut generator = Generator::new();
//! let patch = Patch { rate: Some(Rate::Pps(1000)), limit: Some(Limit::Packets(50)), ..Patch::default() };
//! generator.apply(GenCmd::Start(patch), 0, 1).unwrap();
//! let mut sent = 0;
//! for ms in 0..=100u64 {
//!     generator.poll(ms * 1000, &mut |_payload| { sent += 1; true });
//! }
//! assert_eq!(sent, 50 + 1, "the 50 packets, then one more that marks the end of the stream");
//! assert!(generator.status().state.is_done());
//! ```
//!
//! # What a datagram looks like
//!
//! `WGEN`, the stream number, flags, the sequence number and the sender's clock (µs, low 32 bits), 16 bytes in all
//! ([`Header`]), then filler up to the size asked for. The receiver uses the sequence numbers for loss and
//! order, and the clocks for the jitter (RFC 3550: only the *differences* between packets matter, so the two
//! boards' clocks needn't agree).
//!
//! Times are microseconds since boot.

use core::fmt;

use crate::units::parse_period_ms;

/// Bytes of the header at the start of every datagram's payload.
pub const HEADER_LEN: usize = 16;
/// Smallest payload: the header.
pub const MIN_PAYLOAD: usize = HEADER_LEN;
/// Largest payload that fits one Ethernet frame (1500 − 20 IPv4 − 8 UDP).
pub const MAX_PAYLOAD: usize = 1472;
/// Bytes a datagram takes on the wire beyond its payload: Ethernet 14 + IPv4 20 + UDP 8.
pub const WIRE_OVERHEAD: usize = 42;
/// Most datagrams one call to [`Generator::poll`] hands out, so one slow turn can't flood the socket.
pub const MAX_PER_POLL: usize = 128;
/// A generator that was not polled for a while catches up by at most this much time, µs: no flood after a stall.
const MAX_CATCH_UP_US: u64 = 5_000;
/// A stream the receiver has not heard of for this long is over, µs.
pub const RX_IDLE_US: u64 = 1_000_000;
/// The flag of the last datagram of a stream.
const FLAG_END: u16 = 1;
const MAGIC: [u8; 4] = *b"WGEN";

/// Fastest rates accepted.
const MAX_PPS: u32 = 1_000_000;
const MAX_BPS: u64 = 10_000_000_000;

/// How fast: datagrams per second, or bits per second **on the wire** (headers included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rate {
    /// Datagrams per second.
    Pps(u32),
    /// Bits per second, counting each frame as [`WIRE_OVERHEAD`] + its payload bytes.
    Bps(u64),
}

impl Rate {
    fn value(self) -> u64 {
        match self {
            Rate::Pps(n) => u64::from(n),
            Rate::Bps(n) => n,
        }
    }

    /// This rate in the unit of `bits` (bits per second, or datagrams per second), for a stream whose frames
    /// average `mean_wire` bytes.
    fn in_unit(self, bits: bool, mean_wire: u64) -> u64 {
        match (self, bits) {
            (Rate::Pps(n), false) => u64::from(n),
            (Rate::Bps(n), true) => n,
            (Rate::Pps(n), true) => u64::from(n) * mean_wire * 8,
            (Rate::Bps(n), false) => n / (mean_wire * 8).max(1),
        }
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Rate::Pps(n) if n >= 1000 && n % 1000 == 0 => write!(f, "{} kpps", n / 1000),
            Rate::Pps(n) => write!(f, "{n} pps"),
            Rate::Bps(n) if n >= 1_000_000_000 => {
                write!(f, "{}.{:02} Gbit/s", n / 1_000_000_000, n % 1_000_000_000 / 10_000_000)
            }
            Rate::Bps(n) if n >= 1_000_000 => write!(f, "{}.{:02} Mbit/s", n / 1_000_000, n % 1_000_000 / 10_000),
            Rate::Bps(n) if n >= 1_000 => write!(f, "{}.{} kbit/s", n / 1_000, n % 1_000 / 100),
            Rate::Bps(n) => write!(f, "{n} bit/s"),
        }
    }
}

/// How big: the UDP payload of each datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// Always this many bytes.
    Fixed(u16),
    /// Each datagram a random size in this range, both ends included.
    Range(u16, u16),
    /// The classic Internet mix: 7 small (64 B frames), 4 medium (570 B), 1 large (1514 B).
    Imix,
}

/// Payload sizes of the three IMIX frames (64, 570 and 1514 bytes on the wire).
const IMIX: [u16; 12] = [22, 22, 22, 22, 22, 22, 22, 528, 528, 528, 528, 1472];

impl Size {
    /// Average payload, bytes.
    pub fn mean_payload(self) -> u64 {
        match self {
            Size::Fixed(n) => u64::from(n),
            Size::Range(lo, hi) => (u64::from(lo) + u64::from(hi)) / 2,
            Size::Imix => IMIX.iter().map(|n| u64::from(*n)).sum::<u64>() / IMIX.len() as u64,
        }
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Size::Fixed(n) => write!(f, "{n} B"),
            Size::Range(lo, hi) => write!(f, "{lo}-{hi} B"),
            Size::Imix => f.write_str("imix"),
        }
    }
}

/// How the rate moves over the life of the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// The same rate throughout.
    Flat,
    /// From the rate to `to`, in a straight line over `over_ms`, then it stays there.
    Ramp {
        /// The rate it ends at.
        to: Rate,
        /// How long it takes, ms.
        over_ms: u32,
    },
    /// The rate for `on_ms`, then nothing for `off_ms`, and again.
    Pulse {
        /// Time sending, ms.
        on_ms: u32,
        /// Time quiet, ms.
        off_ms: u32,
    },
    /// The same average rate, but every gap between two datagrams random (0 to twice the mean).
    Jitter,
    /// A wave between the rate (its floor) and `to` (its peak), repeating every `period_ms`: made to be
    /// recognised on the dashboard of the node that receives it.
    Wave {
        /// Which wave.
        kind: Wave,
        /// The rate at the top of the wave.
        to: Rate,
        /// One cycle, ms.
        period_ms: u32,
    },
    /// A staircase from the rate to `to` in `steps` equal steps over `over_ms`, then it stays at the top.
    Steps {
        /// The rate of the last step.
        to: Rate,
        /// Number of levels (2 to 64), the first being the rate.
        steps: u8,
        /// How long it takes to reach the last one, ms.
        over_ms: u32,
    },
}

/// The wave of a [`Shape::Wave`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wave {
    /// A raised sine: `floor + (peak − floor) × (1 − cos(2πt/T)) / 2`. Starts at the floor, reaches the peak halfway
    /// through the cycle, never goes below the floor (so it is **strictly positive** when the floor is).
    Sine,
    /// The same raised curve, half a cycle later: starts at the peak, goes down to the floor and back up
    /// (`floor + (peak − floor) × (1 + cos(2πt/T)) / 2`). Also strictly positive when the floor is.
    Cosine,
    /// Straight up for half a cycle, straight down for the other half.
    Triangle,
    /// Straight up for the whole cycle, then back to the floor at once.
    Saw,
    /// Floor for half a cycle, peak for the other half.
    Square,
}

/// `(1 − cos(π x)) / 2` for `x` from 0 to 65536 (a half cycle: 0 to 1), as 0 to 65536: a table of 64 intervals,
/// read with linear interpolation (the error is below 0.02 % of the full height).
fn raised_cosine(x: u64) -> u64 {
    const TABLE: [u32; 65] = [
        0, 39, 158, 355, 630, 982, 1411, 1915, 2494, 3146, 3869, 4662, 5522, 6448, 7438, 8489, 9598, 10762, 11980,
        13248, 14563, 15922, 17321, 18758, 20228, 21729, 23256, 24806, 26375, 27960, 29556, 31160, 32768, 34376, 35980,
        37576, 39161, 40730, 42280, 43807, 45308, 46778, 48215, 49614, 50973, 52288, 53556, 54774, 55938, 57047, 58098,
        59088, 60014, 60874, 61667, 62390, 63042, 63621, 64125, 64554, 64906, 65181, 65378, 65497, 65536,
    ];
    let x = x.min(65536);
    let (i, frac) = ((x >> 10) as usize, x & 1023);
    let (a, b) = (u64::from(TABLE[i]), u64::from(TABLE[(i + 1).min(64)]));
    if b >= a { a + (((b - a) * frac) >> 10) } else { a - (((a - b) * frac) >> 10) }
}

impl Wave {
    fn name(self) -> &'static str {
        match self {
            Wave::Sine => "sine",
            Wave::Cosine => "cosine",
            Wave::Triangle => "triangle",
            Wave::Saw => "saw",
            Wave::Square => "square",
        }
    }

    /// Where the wave is in its cycle, `phase` from 0 to 65535: 0 is the floor, 65536 the peak.
    fn level(self, phase: u64) -> u64 {
        let up_and_down = if phase < 32768 { phase * 2 } else { (65536 - phase) * 2 };
        match self {
            Wave::Sine => raised_cosine(up_and_down),
            Wave::Cosine => 65536 - raised_cosine(up_and_down),
            Wave::Triangle => up_and_down,
            Wave::Saw => phase,
            Wave::Square => {
                if phase < 32768 {
                    0
                } else {
                    65536
                }
            }
        }
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Shape::Flat => f.write_str("flat"),
            Shape::Ramp { to, over_ms } => write!(f, "ramp to {to} over {}", Ms(over_ms)),
            Shape::Pulse { on_ms, off_ms } => write!(f, "pulse {} on, {} off", Ms(on_ms), Ms(off_ms)),
            Shape::Jitter => f.write_str("random gaps"),
            Shape::Wave { kind, to, period_ms } => write!(f, "{} to {to} every {}", kind.name(), Ms(period_ms)),
            Shape::Steps { to, steps, over_ms } => write!(f, "{steps} steps to {to} over {}", Ms(over_ms)),
        }
    }
}

/// The time a stream lasts when nothing says otherwise, at least, ms.
pub const AUTO_MIN_MS: u32 = 10_000;

/// When it stops by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// The default: as long as the shape takes to show itself whole (a ramp's time, a wave's cycle...), and 10 s
    /// at least. So `shape=sine:1Mbit:20s` lasts 20 s without being told, and a plain stream 10 s.
    Auto,
    /// Never: until `gen stop`.
    Forever,
    /// After this many datagrams.
    Packets(u32),
    /// After this long, ms.
    Millis(u32),
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Limit::Auto => f.write_str("the length of the shape"),
            Limit::Forever => f.write_str("until stopped"),
            Limit::Packets(n) => write!(f, "{n} packets"),
            Limit::Millis(ms) => write!(f, "{}", Ms(ms)),
        }
    }
}

/// Milliseconds as `250 ms`, `30 s`, `2 min`.
struct Ms(u32);

impl fmt::Display for Ms {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ms if ms >= 120_000 && ms % 60_000 == 0 => write!(f, "{} min", ms / 60_000),
            ms if ms >= 1000 && ms % 1000 == 0 => write!(f, "{} s", ms / 1000),
            ms if ms >= 1000 => write!(f, "{}.{:03} s", ms / 1000, ms % 1000),
            ms => write!(f, "{ms} ms"),
        }
    }
}

/// Everything that describes a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    /// How fast.
    pub rate: Rate,
    /// How big.
    pub size: Size,
    /// How the rate moves.
    pub shape: Shape,
    /// When it stops.
    pub limit: Limit,
}

impl Default for Spec {
    /// 1000 datagrams a second of 256 bytes, flat, for 10 s (or as long as the shape).
    fn default() -> Self {
        Spec { rate: Rate::Pps(1000), size: Size::Fixed(256), shape: Shape::Flat, limit: Limit::Auto }
    }
}

impl Spec {
    /// Checks the numbers.
    pub fn validate(&self) -> Result<(), &'static str> {
        let rate_ok = |r: Rate| match r {
            Rate::Pps(n) => (1..=MAX_PPS).contains(&n),
            Rate::Bps(n) => (1_000..=MAX_BPS).contains(&n),
        };
        if !rate_ok(self.rate) {
            return Err("rate must be 1-1000000 pps, or 1 kbit/s-10 Gbit/s");
        }
        let size_ok = |n: u16| (MIN_PAYLOAD..=MAX_PAYLOAD).contains(&usize::from(n));
        match self.size {
            Size::Fixed(n) if !size_ok(n) => return Err("size must be 16-1472 bytes of payload"),
            Size::Range(lo, hi) if !size_ok(lo) || !size_ok(hi) || lo > hi => {
                return Err("size range must be 16-1472 bytes, smallest first");
            }
            _ => {}
        }
        match self.shape {
            Shape::Ramp { to, over_ms } if !rate_ok(to) || over_ms == 0 => return Err("ramp: a valid rate and a time"),
            Shape::Pulse { on_ms, off_ms } if on_ms == 0 || off_ms == 0 => return Err("pulse: both times above zero"),
            Shape::Wave { to, period_ms, .. } if !rate_ok(to) || period_ms < 100 => {
                return Err("wave: a valid peak rate and a cycle of at least 100 ms");
            }
            Shape::Steps { to, steps, over_ms } if !rate_ok(to) || !(2..=64).contains(&steps) || over_ms == 0 => {
                return Err("steps: a valid rate, 2-64 steps, and a time");
            }
            _ => {}
        }
        match self.limit {
            Limit::Packets(0) | Limit::Millis(0) => Err("the limit can't be zero"),
            _ => Ok(()),
        }
    }

    /// How long the shape takes to show itself whole, ms: the ramp or the staircase to its top, one cycle of a
    /// wave or a pulse. `None` for a flat or random stream.
    pub fn shape_span_ms(&self) -> Option<u32> {
        match self.shape {
            Shape::Flat | Shape::Jitter => None,
            Shape::Ramp { over_ms, .. } | Shape::Steps { over_ms, .. } => Some(over_ms),
            Shape::Wave { period_ms, .. } => Some(period_ms),
            Shape::Pulse { on_ms, off_ms } => Some(on_ms.saturating_add(off_ms)),
        }
    }

    /// How long the stream lasts, ms, when it is set by time (`None`: by a count, or never).
    pub fn limit_ms(&self) -> Option<u32> {
        match self.limit {
            Limit::Auto => Some(self.shape_span_ms().unwrap_or(0).max(AUTO_MIN_MS)),
            Limit::Millis(ms) => Some(ms),
            Limit::Forever | Limit::Packets(_) => None,
        }
    }

    /// A warning when a limit **that was asked for** ends the stream before the shape has shown itself whole (`for=10s`
    /// with a 30 s ramp would stop a third of the way up). The default limit follows the shape, so it never warns.
    pub fn warning(&self) -> Option<&'static str> {
        match (self.limit, self.shape_span_ms()) {
            (Limit::Millis(limit), Some(span)) if limit < span => {
                Some("the stream ends before the shape does: lengthen it with `gen for <time>`")
            }
            _ => None,
        }
    }

    /// Average bytes of a frame on the wire.
    pub fn mean_wire(&self) -> u64 {
        self.size.mean_payload() + WIRE_OVERHEAD as u64
    }

    /// The rate a shape moves towards (the top of a wave, the end of a ramp or a staircase), if it has one.
    pub fn shape_target(&self) -> Option<Rate> {
        match self.shape {
            Shape::Ramp { to, .. } | Shape::Wave { to, .. } | Shape::Steps { to, .. } => Some(to),
            _ => None,
        }
    }

    /// `rate` in bits per second on the wire, for this spec's frame sizes.
    pub fn bps_of(&self, rate: Rate) -> u64 {
        rate.in_unit(true, self.mean_wire())
    }

    /// `rate` in datagrams per second, for this spec's frame sizes.
    pub fn nominal_pps_of(&self, rate: Rate) -> u64 {
        rate.in_unit(false, self.mean_wire())
    }

    /// Datagrams per second the stream aims at (at the start, for a ramp).
    pub fn nominal_pps(&self) -> u64 {
        self.rate.in_unit(false, self.mean_wire())
    }

    /// Bits per second on the wire the stream aims at (at the start, for a ramp).
    pub fn nominal_bps(&self) -> u64 {
        self.rate.in_unit(true, self.mean_wire())
    }
}

/// Changes to a [`Spec`]: only what is `Some`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    /// New rate.
    pub rate: Option<Rate>,
    /// New size.
    pub size: Option<Size>,
    /// New shape.
    pub shape: Option<Shape>,
    /// New limit.
    pub limit: Option<Limit>,
}

impl Patch {
    fn apply(&self, spec: &mut Spec) {
        spec.rate = self.rate.unwrap_or(spec.rate);
        spec.size = self.size.unwrap_or(spec.size);
        spec.shape = self.shape.unwrap_or(spec.shape);
        spec.limit = self.limit.unwrap_or(spec.limit);
    }
}

/// What the CLI asks of the generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenCmd {
    /// Reports the state.
    Status,
    /// Starts a stream, with these changes to the stored settings.
    Start(Patch),
    /// Changes the stored settings (and the stream, if one is running).
    Set(Patch),
    /// Ends the stream (the receiver is told).
    Stop,
    /// Holds the stream: the clock of its shape stops too.
    Pause,
    /// Goes on.
    Resume,
}

/// Where the generator is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Never started, or stopped.
    Idle,
    /// Sending.
    Running,
    /// Held.
    Paused,
    /// Ended by its limit or by `stop`.
    Done,
}

impl State {
    /// Whether the stream ended.
    pub fn is_done(self) -> bool {
        self == State::Done
    }
}

/// What the generator is doing, for `gen status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// Where it is.
    pub state: State,
    /// The stored settings (those of the running stream, if there is one).
    pub spec: Spec,
    /// Stream number.
    pub stream: u16,
    /// Datagrams sent in this stream.
    pub sent: u32,
    /// Bytes on the wire, headers included.
    pub wire_bytes: u64,
    /// Time spent running (not paused), µs.
    pub run_us: u64,
    /// How many times the socket had no room when a datagram was due.
    pub blocked: u32,
}

/// What a finished stream reports on the sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxSummary {
    /// Stream number.
    pub stream: u16,
    /// Datagrams sent.
    pub sent: u32,
    /// Bytes on the wire.
    pub wire_bytes: u64,
    /// Running time, µs.
    pub run_us: u64,
    /// Times the socket was full when a datagram was due.
    pub blocked: u32,
}

impl TxSummary {
    /// Datagrams per second.
    pub fn pps(&self) -> u64 {
        (u64::from(self.sent) * 1_000_000).checked_div(self.run_us).unwrap_or(0)
    }

    /// Bits per second on the wire.
    pub fn bps(&self) -> u64 {
        (self.wire_bytes * 8 * 1_000_000).checked_div(self.run_us).unwrap_or(0)
    }
}

/// The header of a datagram's payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Stream number.
    pub stream: u16,
    /// Whether this is the last datagram of the stream (it carries no data, `seq` is the number of datagrams).
    pub end: bool,
    /// Sequence number, from 0.
    pub seq: u32,
    /// The sender's clock when it was built, µs (low 32 bits).
    pub sent_us: u32,
}

impl Header {
    /// The 16 bytes.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[..4].copy_from_slice(&MAGIC);
        b[4..6].copy_from_slice(&self.stream.to_be_bytes());
        b[6..8].copy_from_slice(&(if self.end { FLAG_END } else { 0 }).to_be_bytes());
        b[8..12].copy_from_slice(&self.seq.to_be_bytes());
        b[12..16].copy_from_slice(&self.sent_us.to_be_bytes());
        b
    }

    /// Reads a payload that starts with a header; `None` if it isn't one of ours.
    pub fn parse(payload: &[u8]) -> Option<Header> {
        if payload.len() < HEADER_LEN || payload[..4] != MAGIC {
            return None;
        }
        Some(Header {
            stream: u16::from_be_bytes([payload[4], payload[5]]),
            end: u16::from_be_bytes([payload[6], payload[7]]) & FLAG_END != 0,
            seq: u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]),
            sent_us: u32::from_be_bytes([payload[12], payload[13], payload[14], payload[15]]),
        })
    }
}

/// The generator: a schedule, no sockets.
#[derive(Clone, Debug)]
pub struct Generator {
    spec: Spec,
    state: State,
    stream: u16,
    seq: u32,
    sent: u32,
    wire_bytes: u64,
    run_us: u64,
    last_us: u64,
    blocked: u32,
    /// Credit in micro-units (datagrams × 10⁶, or bits × 10⁶): what has accrued and not yet been spent.
    credit: u64,
    /// The payload size of the datagram that is next, and what it costs.
    next_payload: u16,
    next_cost: u64,
    rng: u32,
    /// The end-of-stream datagram is still to be sent.
    end_pending: bool,
    /// A finished stream's report is still to be taken.
    summary: Option<TxSummary>,
    buf: [u8; MAX_PAYLOAD],
}

impl Default for Generator {
    fn default() -> Self {
        Self::new()
    }
}

impl Generator {
    /// A generator with the default settings, not running.
    pub const fn new() -> Generator {
        Generator {
            spec: Spec { rate: Rate::Pps(1000), size: Size::Fixed(256), shape: Shape::Flat, limit: Limit::Auto },
            state: State::Idle,
            stream: 0,
            seq: 0,
            sent: 0,
            wire_bytes: 0,
            run_us: 0,
            last_us: 0,
            blocked: 0,
            credit: 0,
            next_payload: 256,
            next_cost: 0,
            rng: 1,
            end_pending: false,
            summary: None,
            buf: [0; MAX_PAYLOAD],
        }
    }

    /// What it is doing.
    pub fn status(&self) -> Status {
        Status {
            state: self.state,
            spec: self.spec,
            stream: self.stream,
            sent: self.sent,
            wire_bytes: self.wire_bytes,
            run_us: self.run_us,
            blocked: self.blocked,
        }
    }

    /// The report of a stream that just ended, once.
    pub fn take_summary(&mut self) -> Option<TxSummary> {
        self.summary.take()
    }

    /// Whether anything is due to be sent now or soon (the network task can nap when not).
    pub fn is_active(&self) -> bool {
        self.state == State::Running || self.end_pending
    }

    fn next_random(&mut self) -> u32 {
        // xorshift32
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// The payload size of the next datagram.
    fn pick_size(&mut self) -> u16 {
        match self.spec.size {
            Size::Fixed(n) => n,
            Size::Range(lo, hi) => lo + (self.next_random() % (u32::from(hi - lo) + 1)) as u16,
            Size::Imix => IMIX[(self.next_random() as usize) % IMIX.len()],
        }
    }

    /// The units per second the rate is at after `run_us` of running, and whether they are bits.
    fn rate_now(&self, run_us: u64) -> (u64, bool) {
        let bits = matches!(self.spec.rate, Rate::Bps(_));
        let mean = self.spec.mean_wire();
        let base = self.spec.rate.value();
        let units = match self.spec.shape {
            Shape::Flat | Shape::Jitter => base,
            Shape::Ramp { to, over_ms } => {
                let over = u64::from(over_ms) * 1000;
                let (a, b) = (i128::from(base), i128::from(to.in_unit(bits, mean)));
                let p = i128::from(run_us.min(over));
                (a + (b - a) * p / i128::from(over)).max(0) as u64
            }
            Shape::Pulse { on_ms, off_ms } => {
                let period = (u64::from(on_ms) + u64::from(off_ms)) * 1000;
                if run_us % period < u64::from(on_ms) * 1000 { base } else { 0 }
            }
            Shape::Wave { kind, to, period_ms } => {
                let period = u64::from(period_ms) * 1000;
                let phase = (run_us % period) * 65536 / period;
                let (a, b) = (i128::from(base), i128::from(to.in_unit(bits, mean)));
                (a + (b - a) * i128::from(kind.level(phase)) / 65536).max(0) as u64
            }
            Shape::Steps { to, steps, over_ms } => {
                let over = u64::from(over_ms) * 1000;
                let n = u64::from(steps);
                let step = (run_us.min(over) * n / over).min(n - 1);
                let (a, b) = (i128::from(base), i128::from(to.in_unit(bits, mean)));
                (a + (b - a) * i128::from(step) / i128::from(n - 1)).max(0) as u64
            }
        };
        (units, bits)
    }

    /// What one datagram of `payload` bytes costs, in micro-units of the rate's unit.
    fn cost_of(&mut self, payload: u16) -> u64 {
        let bits = matches!(self.spec.rate, Rate::Bps(_));
        let unit = if bits { (u64::from(payload) + WIRE_OVERHEAD as u64) * 8 } else { 1 };
        let cost = unit * 1_000_000;
        match self.spec.shape {
            // A random fraction of the cost, 0 to 2 times: the same average, random gaps.
            Shape::Jitter => cost * (u64::from(self.next_random() % 2000) + 1) / 1000,
            _ => cost,
        }
    }

    fn choose_next(&mut self) {
        self.next_payload = self.pick_size();
        self.next_cost = self.cost_of(self.next_payload);
    }

    fn begin(&mut self, now_us: u64, seed: u32) {
        self.stream = self.stream.wrapping_add(1);
        self.seq = 0;
        self.sent = 0;
        self.wire_bytes = 0;
        self.run_us = 0;
        self.blocked = 0;
        self.last_us = now_us;
        self.rng = seed | 1;
        self.end_pending = false;
        self.summary = None;
        self.choose_next();
        // The first datagram goes out at once.
        self.credit = self.next_cost;
        self.state = State::Running;
    }

    fn finish(&mut self) {
        if matches!(self.state, State::Running | State::Paused) {
            self.state = State::Done;
            self.end_pending = true;
            self.summary = Some(TxSummary {
                stream: self.stream,
                sent: self.sent,
                wire_bytes: self.wire_bytes,
                run_us: self.run_us,
                blocked: self.blocked,
            });
        }
    }

    /// Carries out a command. `seed` seeds the random sizes and gaps of a new stream. An `Err` says what was wrong
    /// with the settings; the generator is then as it was.
    pub fn apply(&mut self, cmd: GenCmd, now_us: u64, seed: u32) -> Result<(), &'static str> {
        match cmd {
            GenCmd::Status => {}
            GenCmd::Start(patch) => {
                let mut spec = self.spec;
                patch.apply(&mut spec);
                spec.validate()?;
                if self.is_active_stream() {
                    self.finish();
                }
                self.spec = spec;
                self.begin(now_us, seed);
            }
            GenCmd::Set(patch) => {
                let mut spec = self.spec;
                patch.apply(&mut spec);
                spec.validate()?;
                self.spec = spec;
                if self.state == State::Running {
                    // The next datagram is chosen again under the new size.
                    self.choose_next();
                }
            }
            GenCmd::Stop => self.finish(),
            GenCmd::Pause => {
                if self.state == State::Running {
                    self.state = State::Paused;
                }
            }
            GenCmd::Resume => {
                if self.state == State::Paused {
                    self.state = State::Running;
                    self.last_us = now_us;
                }
            }
        }
        Ok(())
    }

    fn is_active_stream(&self) -> bool {
        matches!(self.state, State::Running | State::Paused)
    }

    /// Hands out the datagrams that are due at `now_us`, one call to `emit` each with the payload. `emit` returns
    /// whether the socket took it: if not, the datagram stays due and the call ends (it is counted in `blocked`).
    pub fn poll(&mut self, now_us: u64, emit: &mut dyn FnMut(&[u8]) -> bool) {
        let dt = now_us.saturating_sub(self.last_us);
        self.last_us = now_us;

        if self.state == State::Running {
            self.run_us += dt;
            let (rate, _) = self.rate_now(self.run_us);
            // Credit accrues with time, but never more than a few ms' worth (and a datagram): a stall is not repaid.
            let cap = rate.saturating_mul(MAX_CATCH_UP_US).saturating_add(self.next_cost);
            self.credit = self.credit.saturating_add(rate.saturating_mul(dt)).min(cap);
            let mut handed = 0;
            while self.state == State::Running && handed < MAX_PER_POLL {
                let over = match self.spec.limit {
                    Limit::Forever => false,
                    Limit::Packets(n) => self.sent >= n,
                    Limit::Auto | Limit::Millis(_) => {
                        self.run_us >= u64::from(self.spec.limit_ms().unwrap_or(AUTO_MIN_MS)) * 1000
                    }
                };
                if over {
                    self.finish();
                    break;
                }
                if self.credit < self.next_cost {
                    break;
                }
                let len = usize::from(self.next_payload);
                self.fill(len, now_us, false);
                if !emit(&self.buf[..len]) {
                    self.blocked += 1;
                    break;
                }
                self.credit -= self.next_cost;
                self.seq = self.seq.wrapping_add(1);
                self.sent += 1;
                self.wire_bytes += (len + WIRE_OVERHEAD) as u64;
                handed += 1;
                self.choose_next();
            }
            // A `Packets` limit is reached by the last datagram, not by the next poll.
            if self.state == State::Running && matches!(self.spec.limit, Limit::Packets(n) if self.sent >= n) {
                self.finish();
            }
        }

        if self.end_pending {
            self.fill(MIN_PAYLOAD, now_us, true);
            if emit(&self.buf[..MIN_PAYLOAD]) {
                self.end_pending = false;
            }
        }
    }

    /// Builds a payload of `len` bytes in the buffer: the header, then a pattern.
    fn fill(&mut self, len: usize, now_us: u64, end: bool) {
        let header = Header { stream: self.stream, end, seq: self.seq, sent_us: now_us as u32 };
        self.buf[..HEADER_LEN].copy_from_slice(&header.encode());
        for (i, b) in self.buf[HEADER_LEN..len].iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(0x41);
        }
    }
}

/// What the receiver reports about a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxSummary {
    /// Stream number.
    pub stream: u16,
    /// Datagrams received.
    pub received: u32,
    /// Datagrams that never came.
    pub lost: u32,
    /// Datagrams that came after one with a higher number.
    pub reordered: u32,
    /// Bytes on the wire.
    pub wire_bytes: u64,
    /// From the first datagram to the last, µs.
    pub duration_us: u64,
    /// Inter-arrival jitter (RFC 3550), µs.
    pub jitter_us: u32,
    /// Whether the sender's end marker arrived (else the stream was closed after [`RX_IDLE_US`] of silence, and
    /// datagrams lost at its very end are not counted).
    pub ended: bool,
}

impl RxSummary {
    /// Loss, in hundredths of a percent.
    pub fn loss_per_10000(&self) -> u64 {
        let expected = u64::from(self.received) + u64::from(self.lost);
        (u64::from(self.lost) * 10_000).checked_div(expected).unwrap_or(0)
    }

    /// Datagrams per second.
    pub fn pps(&self) -> u64 {
        (u64::from(self.received) * 1_000_000).checked_div(self.duration_us).unwrap_or(0)
    }

    /// Bits per second on the wire.
    pub fn bps(&self) -> u64 {
        (self.wire_bytes * 8 * 1_000_000).checked_div(self.duration_us).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug)]
struct RxStream {
    stream: u16,
    received: u32,
    wire_bytes: u64,
    max_seq: u32,
    reordered: u32,
    end_seq: Option<u32>,
    first_us: u64,
    last_us: u64,
    /// 16 × the jitter, µs.
    jitter16: u32,
    transit: Option<i32>,
}

/// The receiving side: tracks the stream that is arriving.
#[derive(Clone, Debug, Default)]
pub struct GenRx {
    current: Option<RxStream>,
    /// The last stream closed, so late datagrams of it don't open a new one.
    closed: Option<u16>,
}

impl GenRx {
    /// A receiver with nothing in progress.
    pub const fn new() -> GenRx {
        GenRx { current: None, closed: None }
    }

    fn close(&mut self) -> Option<RxSummary> {
        let s = self.current.take()?;
        self.closed = Some(s.stream);
        let expected = s.end_seq.unwrap_or_else(|| s.max_seq.wrapping_add(1));
        Some(RxSummary {
            stream: s.stream,
            received: s.received,
            lost: expected.saturating_sub(s.received),
            reordered: s.reordered,
            wire_bytes: s.wire_bytes,
            duration_us: s.last_us.saturating_sub(s.first_us),
            jitter_us: s.jitter16 >> 4,
            ended: s.end_seq.is_some(),
        })
    }

    /// A datagram arrived at `now_us`: its payload, and its size on the wire. Returns the summary of a stream this
    /// one ended (its end marker, or a new stream starting before the old one was closed).
    pub fn receive(&mut self, payload: &[u8], now_us: u64) -> Option<RxSummary> {
        let h = Header::parse(payload)?;
        let mut done = None;
        if self.current.is_some_and(|c| c.stream != h.stream) {
            done = self.close();
        }
        if self.current.is_none() {
            if self.closed == Some(h.stream) {
                return done;
            }
            self.current = Some(RxStream {
                stream: h.stream,
                received: 0,
                wire_bytes: 0,
                max_seq: 0,
                reordered: 0,
                end_seq: None,
                first_us: now_us,
                last_us: now_us,
                jitter16: 0,
                transit: None,
            });
        }
        let s = self.current.as_mut()?;
        s.last_us = now_us;
        if h.end {
            s.end_seq = Some(h.seq);
            return done.or_else(|| self.close());
        }
        if s.received > 0 && h.seq < s.max_seq {
            s.reordered += 1;
        }
        s.max_seq = s.max_seq.max(h.seq);
        s.received += 1;
        s.wire_bytes += (payload.len() + WIRE_OVERHEAD) as u64;
        // RFC 3550: the change of the transit time between two datagrams. A constant offset between the clocks
        // cancels out.
        let transit = (now_us as u32).wrapping_sub(h.sent_us) as i32;
        if let Some(prev) = s.transit {
            let d = transit.wrapping_sub(prev).unsigned_abs();
            s.jitter16 = s.jitter16 + d - (s.jitter16 >> 4);
        }
        s.transit = Some(transit);
        done
    }

    /// Closes a stream that has gone quiet.
    pub fn poll(&mut self, now_us: u64) -> Option<RxSummary> {
        if self.current.is_some_and(|c| now_us.saturating_sub(c.last_us) > RX_IDLE_US) { self.close() } else { None }
    }

    /// Whether a stream is arriving.
    pub fn is_receiving(&self) -> bool {
        self.current.is_some()
    }
}

// --- Parsing the CLI ------------------------------------------------------------------------------------

/// A number with an optional unit suffix: `(value × 1000 if k, ×10⁶ if M, ×10⁹ if G, rest of the suffix)`.
fn scaled(text: &str) -> Option<(u64, &str)> {
    let i = text.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(text.len());
    let (number, suffix) = text.split_at(i);
    if number.is_empty() {
        return None;
    }
    let (whole, frac) = number.split_once('.').unwrap_or((number, ""));
    let whole: u64 = whole.parse().ok()?;
    let (mult, rest) = match suffix.as_bytes().first() {
        Some(b'k' | b'K') => (1_000u64, &suffix[1..]),
        Some(b'M' | b'm') => (1_000_000, &suffix[1..]),
        Some(b'G') => (1_000_000_000, &suffix[1..]),
        _ => (1, suffix),
    };
    // Up to three decimals.
    let mut f = 0u64;
    let mut scale = 1u64;
    for c in frac.chars().take(3) {
        f = f * 10 + u64::from(c.to_digit(10)?);
        scale *= 10;
    }
    if frac.len() > 3 {
        return None;
    }
    Some((whole.checked_mul(mult)?.checked_add(f * mult / scale)?, rest))
}

/// Parses a rate: `5000`, `5000pps`, `5kpps`, `2Mbit`, `500kbit`, `1.5Mbit/s`, `1Gbps`. A number alone is
/// datagrams per second.
pub fn parse_rate(text: &str) -> Option<Rate> {
    let (value, unit) = scaled(text.trim())?;
    let is = |names: &[&str]| names.iter().any(|n| unit.eq_ignore_ascii_case(n));
    if is(&["", "pps", "p/s"]) {
        Some(Rate::Pps(u32::try_from(value).ok()?))
    } else if is(&["bit", "bits", "bps", "bit/s", "b/s"]) {
        Some(Rate::Bps(value))
    } else {
        None
    }
}

/// Parses a size: `512`, `512B`, `64-1400`, `imix`.
pub fn parse_size(text: &str) -> Option<Size> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("imix") {
        return Some(Size::Imix);
    }
    let bytes = |t: &str| t.trim().trim_end_matches(['B', 'b']).parse::<u16>().ok();
    match text.split_once('-') {
        Some((lo, hi)) => Some(Size::Range(bytes(lo)?, bytes(hi)?)),
        None => Some(Size::Fixed(bytes(text)?)),
    }
}

/// A bare token that is a size and could not be a rate: `512B`, `64-1400` (a plain `512` is a rate).
fn parse_size_bare(token: &str) -> Option<Size> {
    if token.contains('-') || token.ends_with(['B', 'b']) && !token.ends_with("bit") && !token.ends_with("ps") {
        parse_size(token)
    } else {
        None
    }
}

/// Parses a time as milliseconds: `500ms`, `30s`, `2min`, `1.5s`. A bare number is seconds here.
fn parse_millis(text: &str) -> Option<u32> {
    let text = text.trim();
    let ms = if let Some(n) = text.strip_suffix("min").or_else(|| text.strip_suffix('m')) {
        parse_period_ms(&with_seconds(n)?)? * 60.0
    } else if text.ends_with(|c: char| c.is_ascii_digit() || c == '.') {
        parse_period_ms(&with_seconds(text)?)?
    } else {
        parse_period_ms(text)?
    };
    (1.0..=u32::MAX as f32).contains(&ms).then_some(ms as u32)
}

/// `30` → `30s`, for [`parse_period_ms`].
fn with_seconds(n: &str) -> Option<heapless::String<24>> {
    let mut s = heapless::String::new();
    s.push_str(n.trim()).ok()?;
    s.push('s').ok()?;
    Some(s)
}

/// Parses a shape: `flat`, `ramp:<rate>:<time>`, `pulse:<on>:<off>`, `random`, `sine|triangle|saw|square:<peak>:<cycle>`,
/// `steps:<rate>:<n>:<time>`.
pub fn parse_shape(text: &str) -> Option<Shape> {
    let mut parts = text.trim().split(':');
    let name = parts.next()?;
    match name {
        "flat" | "const" | "steady" => Some(Shape::Flat),
        "random" | "jitter" => Some(Shape::Jitter),
        "sine" | "cosine" | "triangle" | "saw" | "square" => Some(Shape::Wave {
            kind: match name {
                "sine" => Wave::Sine,
                "cosine" => Wave::Cosine,
                "triangle" => Wave::Triangle,
                "saw" => Wave::Saw,
                _ => Wave::Square,
            },
            to: parse_rate(parts.next()?)?,
            period_ms: parse_millis(parts.next()?)?,
        }),
        "steps" => Some(Shape::Steps {
            to: parse_rate(parts.next()?)?,
            steps: parts.next()?.trim().parse().ok()?,
            over_ms: parse_millis(parts.next()?)?,
        }),
        "ramp" => Some(Shape::Ramp { to: parse_rate(parts.next()?)?, over_ms: parse_millis(parts.next()?)? }),
        "pulse" => Some(Shape::Pulse { on_ms: parse_millis(parts.next()?)?, off_ms: parse_millis(parts.next()?)? }),
        _ => None,
    }
}

/// Parses a limit: `forever`, a time (`30s`), or `count=` handled by the caller.
fn parse_for(text: &str) -> Option<Limit> {
    if text.eq_ignore_ascii_case("forever") || text == "0" {
        return Some(Limit::Forever);
    }
    if text.eq_ignore_ascii_case("auto") {
        return Some(Limit::Auto);
    }
    Some(Limit::Millis(parse_millis(text)?))
}

/// Refuses a patch whose values are out of range, so the CLI says so at once (the generator checks the whole
/// spec again when it applies it).
fn checked(patch: Patch) -> Result<(), &'static str> {
    let mut probe = Spec::default();
    patch.apply(&mut probe);
    probe.validate()
}

/// Parses what follows `gen`:
///
/// ```text
/// gen                                  status
/// gen start [rate] [size=S] [for=T|count=N] [shape=P]   start (settings not given stay as they were)
/// gen stop | pause | resume
/// gen rate R | size S | shape P | for T | count N        change the settings, a running stream too
/// ```
///
/// `R`: `5000` (pps), `2Mbit`, `500kbit`. `S`: `512`, `64-1400`, `imix`. `P`: `flat`, `ramp:10Mbit:30s`,
/// `pulse:200ms:800ms`, `random`. `T`: `30s`, `2min`, `forever`.
///
/// ```
/// use workshop_core::pktgen::{parse_gen, GenCmd, Rate, Size, Limit};
///
/// let GenCmd::Start(p) = parse_gen("start 2Mbit size=512 for=30s").unwrap() else { panic!() };
/// assert_eq!((p.rate, p.size, p.limit), (Some(Rate::Bps(2_000_000)), Some(Size::Fixed(512)), Some(Limit::Millis(30_000))));
/// assert_eq!(parse_gen("stop"), Ok(GenCmd::Stop));
/// assert!(parse_gen("start size=9000").is_err());
/// ```
pub fn parse_gen(text: &str) -> Result<GenCmd, &'static str> {
    let text = text.trim();
    let (sub, rest) = match text.find(char::is_whitespace) {
        Some(i) => (&text[..i], text[i..].trim()),
        None => (text, ""),
    };
    let mut patch = Patch::default();
    // One `key=value` or bare token into the patch. A bare token says what it is by its look: `5000`, `2Mbit` a
    // rate; `512B`, `64-1400`, `imix` a size; `sine:3Mbit:20s` a shape (it has a colon); `30s`, `2min`, `forever`
    // a time.
    let take = |token: &str, patch: &mut Patch| -> Result<(), &'static str> {
        let (key, value) = match token.split_once('=') {
            Some(pair) => pair,
            None if token.contains(':') => ("shape", token),
            None if token.eq_ignore_ascii_case("imix") || parse_size_bare(token).is_some() => ("size", token),
            None if parse_rate(token).is_some() => ("rate", token),
            None if token.eq_ignore_ascii_case("forever") || token.eq_ignore_ascii_case("auto") => ("for", token),
            None if parse_millis(token).is_some() && token.ends_with(|c: char| c.is_ascii_alphabetic()) => {
                ("for", token)
            }
            None => ("rate", token),
        };
        match key {
            "rate" => patch.rate = Some(parse_rate(value).ok_or("rate: 5000 (pps), 2Mbit, 500kbit")?),
            "size" => patch.size = Some(parse_size(value).ok_or("size: 512, 64-1400 or imix")?),
            "shape" | "profile" => {
                patch.shape = Some(parse_shape(value).ok_or(
                    "shape: flat, ramp:10Mbit:30s, pulse:200ms:800ms, sine:10Mbit:20s, steps:10Mbit:5:30s, random",
                )?);
            }
            "for" | "time" | "duration" => patch.limit = Some(parse_for(value).ok_or("for: 30s, 2min or forever")?),
            "count" | "packets" => {
                patch.limit = Some(Limit::Packets(value.parse().map_err(|_| "count: a number of datagrams")?));
            }
            _ => return Err("unknown setting (rate, size, shape, for, count)"),
        }
        Ok(())
    };
    match sub {
        "" | "status" | "st" => Ok(GenCmd::Status),
        "stop" | "end" => Ok(GenCmd::Stop),
        "pause" | "hold" => Ok(GenCmd::Pause),
        "resume" | "go" | "continue" => Ok(GenCmd::Resume),
        "start" | "run" => {
            for token in rest.split_whitespace() {
                take(token, &mut patch)?;
            }
            checked(patch)?;
            Ok(GenCmd::Start(patch))
        }
        "rate" | "size" | "shape" | "profile" | "for" | "count" => {
            if rest.is_empty() {
                return Err("missing value");
            }
            let mut token: heapless::String<128> = heapless::String::new();
            let _ = core::fmt::Write::write_fmt(&mut token, format_args!("{sub}={rest}"));
            take(&token, &mut patch)?;
            checked(patch)?;
            Ok(GenCmd::Set(patch))
        }
        _ => Err("gen start|stop|pause|resume|rate|size|shape|for|count (see `help gen`)"),
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;

    /// Runs a generator for `seconds`, polling every `step_us`, and returns the payload sizes and send times.
    fn run(generator: &mut Generator, seconds: u64, step_us: u64) -> Vec<(u64, usize)> {
        let mut out = Vec::new();
        let mut t = 0;
        while t <= seconds * 1_000_000 {
            generator.poll(t, &mut |p| {
                if Header::parse(p).is_some_and(|h| !h.end) {
                    out.push((t, p.len()));
                }
                true
            });
            t += step_us;
        }
        out
    }

    fn start(patch: Patch) -> Generator {
        let mut g = Generator::new();
        g.apply(GenCmd::Start(patch), 0, 7).unwrap();
        g
    }

    fn patch(rate: Rate, size: Size) -> Patch {
        Patch { rate: Some(rate), size: Some(size), limit: Some(Limit::Forever), ..Patch::default() }
    }

    #[test]
    fn a_flat_stream_sends_what_the_rate_says() {
        for (rate, step) in
            [(Rate::Pps(1000), 1000), (Rate::Pps(20_000), 500), (Rate::Pps(37), 1000), (Rate::Pps(100_000), 100)]
        {
            let mut g = start(patch(rate, Size::Fixed(100)));
            let sent = run(&mut g, 2, step).len() as i64;
            let Rate::Pps(n) = rate else { unreachable!() };
            let want = i64::from(n) * 2;
            assert!((sent - want).abs() <= want / 100 + 2, "{rate}: sent {sent}, wanted {want}");
        }
    }

    #[test]
    fn a_bit_rate_counts_the_headers() {
        // 8 Mbit/s of 1000-byte payloads: 1042 B on the wire = 8336 bits: about 959.6 datagrams a second.
        let mut g = start(patch(Rate::Bps(8_000_000), Size::Fixed(1000)));
        let sent = run(&mut g, 4, 1000);
        let wire_bits: u64 = sent.iter().map(|(_, len)| ((len + WIRE_OVERHEAD) * 8) as u64).sum();
        let bps = wire_bits / 4;
        assert!((7_950_000..=8_050_000).contains(&bps), "{bps} bit/s");
        assert_eq!(g.status().wire_bytes * 8, wire_bits);
    }

    #[test]
    fn mixed_sizes_still_hit_a_bit_rate() {
        let mut g = start(patch(Rate::Bps(10_000_000), Size::Imix));
        let sent = run(&mut g, 5, 500);
        let bps = sent.iter().map(|(_, len)| ((len + WIRE_OVERHEAD) * 8) as u64).sum::<u64>() / 5;
        assert!((9_800_000..=10_200_000).contains(&bps), "{bps} bit/s with the IMIX");
        let small = sent.iter().filter(|(_, l)| *l == 22).count() as f64 / sent.len() as f64;
        assert!((0.52..0.65).contains(&small), "7 in 12 are small: {small}");
        assert!(sent.iter().all(|(_, l)| [22, 528, 1472].contains(l)));
    }

    #[test]
    fn a_size_range_stays_inside_and_fills_it() {
        let mut g = start(patch(Rate::Pps(5000), Size::Range(100, 200)));
        let sizes: Vec<usize> = run(&mut g, 2, 500).iter().map(|(_, l)| *l).collect();
        assert!(sizes.iter().all(|l| (100..=200).contains(l)));
        assert!(sizes.contains(&100) && sizes.contains(&200), "both ends are used");
    }

    #[test]
    fn a_ramp_goes_from_one_rate_to_the_other() {
        let p = Patch {
            rate: Some(Rate::Pps(100)),
            size: Some(Size::Fixed(64)),
            shape: Some(Shape::Ramp { to: Rate::Pps(2100), over_ms: 10_000 }),
            limit: Some(Limit::Forever),
        };
        let mut g = start(p);
        let sent = run(&mut g, 12, 1000);
        let in_sec = |a: u64, b: u64| sent.iter().filter(|(t, _)| *t >= a * 1_000_000 && *t < b * 1_000_000).count();
        // 100 pps at the start (300 at the end of the first second), 1100 to 1300 over the sixth, 2100 from the tenth.
        assert!((90..=330).contains(&in_sec(0, 1)), "{}", in_sec(0, 1));
        assert!((1100..=1300).contains(&in_sec(5, 6)), "{}", in_sec(5, 6));
        assert!((2000..=2200).contains(&in_sec(10, 11)), "{}", in_sec(10, 11));
        assert!((2000..=2200).contains(&in_sec(11, 12)), "it stays there");
    }

    /// Packets per second in each second of a run of `seconds`.
    fn per_second(shape: Shape, rate: u32, seconds: u64) -> Vec<usize> {
        let p = Patch {
            rate: Some(Rate::Pps(rate)),
            size: Some(Size::Fixed(64)),
            shape: Some(shape),
            limit: Some(Limit::Forever),
        };
        let mut generator = start(p);
        let sent = run(&mut generator, seconds, 500);
        (0..seconds)
            .map(|s| sent.iter().filter(|(t, _)| *t >= s * 1_000_000 && *t < (s + 1) * 1_000_000).count())
            .collect()
    }

    #[test]
    fn waves_go_between_the_floor_and_the_peak() {
        let wave = |kind| Shape::Wave { kind, to: Rate::Pps(1100), period_ms: 8000 };
        // Floor 100 pps, peak 1100 pps, a cycle of 8 s: the floor at 0 and 8, the peak at 4. A second is an average:
        // the two around the peak (3-4 s, 4-5 s) are the highest, 975 pps each, and the two around the floor 225.
        let tri = per_second(wave(Wave::Triangle), 100, 9);
        assert!(tri[0] < 250 && tri[7] < 250 && tri[8] < 250, "{tri:?}");
        assert!(tri[3] > 900 && tri[3] < 1100 && tri[4] > 900 && tri[4] < 1100, "{tri:?}");
        assert!(tri[0] < tri[1] && tri[1] < tri[2] && tri[2] < tri[3], "up: {tri:?}");
        assert!(tri[4] > tri[5] && tri[5] > tri[6] && tri[6] > tri[7], "down: {tri:?}");
        // The sine eases: slower than the triangle near the floor, faster in the middle.
        let sine = per_second(wave(Wave::Sine), 100, 9);
        assert!(sine[0] < tri[0] && sine[1] < tri[1] && sine[2] > tri[2], "{sine:?} vs {tri:?}");
        assert!(sine[3] > 950 && sine[4] > 950 && sine[3] < 1100, "{sine:?}");
        // The saw climbs for the whole cycle and drops.
        let saw = per_second(wave(Wave::Saw), 100, 9);
        assert!(saw[6] > saw[3] && saw[7] > saw[6] && saw[8] < 250, "{saw:?}");
        // The square: half a cycle at each end.
        let square = per_second(wave(Wave::Square), 100, 9);
        assert!(
            square[1] < 150 && square[2] < 150 && square[5] > 1000 && square[6] > 1000 && square[8] < 150,
            "{square:?}"
        );
    }

    #[test]
    fn the_sine_is_a_true_raised_cosine() {
        // Against the real function: `(1 - cos(pi x)) / 2`, to within two hundredths of a percent of the full height.
        let mut worst = 0.0f64;
        for x in (0..=65536u64).step_by(37) {
            let exact = (1.0 - (core::f64::consts::PI * x as f64 / 65536.0).cos()) / 2.0 * 65536.0;
            worst = worst.max((raised_cosine(x) as f64 - exact).abs());
        }
        assert!(worst < 12.0, "{worst} of 65536");
        assert_eq!((raised_cosine(0), raised_cosine(32768), raised_cosine(65536)), (0, 32768, 65536));
        // The whole wave from the floor to the peak and back: monotonic up, then down, the same on both sides.
        let up: Vec<u64> = (0..=64).map(|i| Wave::Sine.level(i * 512)).collect();
        assert!(up.windows(2).all(|w| w[0] <= w[1]), "{up:?}");
        assert_eq!(Wave::Sine.level(0), 0);
        assert_eq!(Wave::Sine.level(32768), 65536);
        assert!(Wave::Sine.level(16384).abs_diff(32768) <= 1, "halfway up at a quarter of the cycle");
        assert_eq!(Wave::Cosine.level(0), 65536, "the cosine starts at the peak");
        assert_eq!(Wave::Cosine.level(32768), 0);
    }

    #[test]
    fn a_continuous_sine_never_goes_to_zero() {
        // 200 pps floor, 1200 pps peak, a 10 s cycle, for ever: 35 s is three and a half cycles.
        let shape = Shape::Wave { kind: Wave::Sine, to: Rate::Pps(1200), period_ms: 10_000 };
        let sent = per_second(shape, 200, 35);
        assert!(sent.iter().all(|n| *n >= 190), "never below the floor: {sent:?}");
        assert!(sent.iter().all(|n| *n <= 1210), "never above the peak: {sent:?}");
        // It really goes between the two, and comes back: the same second of each cycle gives the same count.
        assert!(sent[0] < 400 && sent[4] > 1000, "{sent:?}");
        for k in 0..10 {
            assert!(sent[k].abs_diff(sent[k + 10]) < 25 && sent[k].abs_diff(sent[k + 20]) < 25, "second {k}: {sent:?}");
        }
    }

    #[test]
    fn triangles_repeat_for_as_long_as_the_stream_runs() {
        let shape = Shape::Wave { kind: Wave::Triangle, to: Rate::Pps(1100), period_ms: 6000 };
        let sent = per_second(shape, 100, 20);
        // Floor 100, peak 1100, 6 s a triangle: the peak is at 3, 9, 15 s.
        let peak = |c: usize| sent[c - 1] + sent[c];
        assert!(peak(3).abs_diff(peak(9)) < 30 && peak(3).abs_diff(peak(15)) < 30, "{sent:?}");
        assert!(sent[0].abs_diff(sent[6]) < 30 && sent[0].abs_diff(sent[12]) < 30, "{sent:?}");
        assert!(peak(3) > 1800 && sent[0] < 300, "{sent:?}");
    }

    #[test]
    fn a_staircase_has_distinct_levels_and_stays_at_the_top() {
        let steps = per_second(Shape::Steps { to: Rate::Pps(1000), steps: 5, over_ms: 10_000 }, 200, 13);
        // Five levels of 2 s each: 200, 400, 600, 800, 1000 pps, then it stays.
        let level = |from: usize| steps[from..from + 2].iter().sum::<usize>() / 2;
        for (i, want) in [200usize, 400, 600, 800, 1000].iter().enumerate() {
            let got = level(i * 2);
            assert!(got.abs_diff(*want) < want / 10 + 15, "step {i}: {got} for {want}: {steps:?}");
        }
        assert!(steps[11].abs_diff(1000) < 60 && steps[12].abs_diff(1000) < 60, "it stays: {steps:?}");
    }

    #[test]
    fn the_default_limit_follows_the_shape() {
        // A plain stream: 10 s. A sine whose cycle is 20 s: 20 s, with no `for=`.
        let plain = Spec::default();
        assert_eq!(plain.limit_ms(), Some(10_000));
        let sine =
            Spec { shape: Shape::Wave { kind: Wave::Sine, to: Rate::Pps(900), period_ms: 20_000 }, ..Spec::default() };
        assert_eq!(sine.limit_ms(), Some(20_000));
        assert!(sine.warning().is_none(), "nothing to warn about: it lasts one whole cycle");
        // A shape shorter than 10 s still gets 10 s.
        let quick = Spec { shape: Shape::Pulse { on_ms: 100, off_ms: 400 }, ..Spec::default() };
        assert_eq!(quick.limit_ms(), Some(10_000));

        let mut generator = Generator::new();
        let GenCmd::Start(p) = parse_gen("start 100kbit size=512 shape=sine:1Mbit:20s").unwrap() else {
            unreachable!()
        };
        generator.apply(GenCmd::Start(p), 0, 1).unwrap();
        let sent = run(&mut generator, 40, 1000);
        let last = sent.last().unwrap().0;
        assert!((19_900_000..=20_000_000).contains(&last), "the last datagram at {last} µs: it ran 20 s");
        // `for=` still wins, and `for=auto` brings the default back.
        let GenCmd::Start(p) = parse_gen("start shape=sine:1Mbit:20s for=5s").unwrap() else { unreachable!() };
        assert_eq!(p.limit, Some(Limit::Millis(5000)));
        assert_eq!(parse_gen("for auto"), Ok(GenCmd::Set(Patch { limit: Some(Limit::Auto), ..Patch::default() })));
        // The shape changes while it runs: the limit follows.
        let ramp = Spec { shape: Shape::Ramp { to: Rate::Pps(2000), over_ms: 30_000 }, ..Spec::default() };
        assert_eq!(ramp.limit_ms(), Some(30_000));
    }

    #[test]
    fn a_shape_longer_than_the_stream_is_flagged() {
        let ramp = Shape::Ramp { to: Rate::Pps(2000), over_ms: 30_000 };
        let spec = Spec { shape: ramp, limit: Limit::Millis(10_000), ..Spec::default() };
        assert_eq!(spec.shape_span_ms(), Some(30_000));
        assert!(spec.warning().is_some(), "10 s of a 30 s ramp");
        assert!(Spec { limit: Limit::Millis(30_000), ..spec }.warning().is_none());
        assert!(Spec { limit: Limit::Forever, ..spec }.warning().is_none());
        assert!(Spec::default().warning().is_none() && Spec::default().shape_span_ms().is_none());
        let wave = Shape::Wave { kind: Wave::Sine, to: Rate::Pps(900), period_ms: 20_000 };
        assert!(
            Spec { shape: wave, limit: Limit::Millis(10_000), ..Spec::default() }.warning().is_some(),
            "less than one cycle"
        );
    }

    #[test]
    fn a_pulse_is_silent_while_off() {
        let p = Patch {
            rate: Some(Rate::Pps(2000)),
            size: Some(Size::Fixed(64)),
            shape: Some(Shape::Pulse { on_ms: 200, off_ms: 800 }),
            limit: Some(Limit::Forever),
        };
        let mut g = start(p);
        let sent = run(&mut g, 4, 500);
        for (t, _) in &sent {
            assert!(t % 1_000_000 < 205_000, "a datagram at {t} µs, during the pause");
        }
        // 4 pulses of 200 ms at 2000 pps.
        assert!((1550..=1650).contains(&sent.len()), "{}", sent.len());
    }

    #[test]
    fn random_gaps_keep_the_average_rate() {
        let p = Patch { shape: Some(Shape::Jitter), ..patch(Rate::Pps(2000), Size::Fixed(64)) };
        let mut g = start(p);
        let sent = run(&mut g, 5, 100);
        assert!((9_600..=10_400).contains(&sent.len()), "{}", sent.len());
        // The gaps are not all the same.
        let gaps: Vec<u64> = sent.windows(2).map(|w| w[1].0 - w[0].0).collect();
        assert!(gaps.contains(&0) && gaps.iter().any(|g| *g >= 800), "gaps from none to long");
    }

    #[test]
    fn limits_end_the_stream_and_mark_its_end() {
        let mut g = start(Patch { limit: Some(Limit::Packets(100)), ..patch(Rate::Pps(1000), Size::Fixed(64)) });
        let mut seen = Vec::new();
        for ms in 0..400u64 {
            g.poll(ms * 1000, &mut |p| {
                seen.push(Header::parse(p).unwrap());
                true
            });
        }
        assert_eq!(seen.len(), 101);
        assert!(seen[..100].iter().enumerate().all(|(i, h)| h.seq == i as u32 && !h.end));
        assert!(seen[100].end && seen[100].seq == 100, "the end marker carries the count");
        assert!(g.status().state.is_done());
        let summary = g.take_summary().unwrap();
        assert_eq!((summary.sent, summary.stream), (100, 1));
        assert!(g.take_summary().is_none(), "reported once");

        // A time limit.
        let mut g = start(Patch { limit: Some(Limit::Millis(500)), ..patch(Rate::Pps(1000), Size::Fixed(64)) });
        let sent = run(&mut g, 2, 1000);
        assert!((495..=505).contains(&sent.len()), "{}", sent.len());
    }

    #[test]
    fn pause_freezes_the_clock_and_resume_goes_on() {
        let mut g = start(Patch { limit: Some(Limit::Millis(1000)), ..patch(Rate::Pps(1000), Size::Fixed(64)) });
        let mut sent = 0;
        let mut poll = |g: &mut Generator, t_ms: u64| {
            g.poll(t_ms * 1000, &mut |_| {
                sent += 1;
                true
            })
        };
        for ms in 0..=400 {
            poll(&mut g, ms);
        }
        g.apply(GenCmd::Pause, 400_000, 0).unwrap();
        for ms in 401..=5000 {
            poll(&mut g, ms);
        }
        assert_eq!(g.status().state, State::Paused);
        g.apply(GenCmd::Resume, 5_000_000, 0).unwrap();
        for ms in 5001..=7000 {
            poll(&mut g, ms);
        }
        // 1000 ms of running time in all: 400 before the pause, 600 after.
        assert!((995..=1006).contains(&(sent - 1)), "{sent}");
        assert!(g.status().state.is_done());
    }

    #[test]
    fn a_full_socket_delays_and_does_not_lose() {
        let mut g = start(Patch { limit: Some(Limit::Packets(10)), ..patch(Rate::Pps(1000), Size::Fixed(64)) });
        let mut accepted = Vec::new();
        for ms in 0..50u64 {
            // The socket refuses everything for the first 20 ms.
            g.poll(ms * 1000, &mut |p| {
                if ms < 20 {
                    return false;
                }
                accepted.push(Header::parse(p).unwrap());
                true
            });
        }
        let data: Vec<u32> = accepted.iter().filter(|h| !h.end).map(|h| h.seq).collect();
        assert_eq!(data, (0..10).collect::<Vec<_>>(), "all there, in order, none repeated");
        assert!(g.status().blocked > 0);
        assert!(accepted.last().unwrap().end, "and the end marker still follows");
    }

    #[test]
    fn a_stall_is_not_repaid_with_a_flood() {
        let mut g = start(patch(Rate::Pps(10_000), Size::Fixed(64)));
        let mut n = 0;
        g.poll(0, &mut |_| {
            n += 1;
            true
        });
        n = 0;
        g.poll(2_000_000, &mut |_| {
            n += 1;
            true
        }); // two seconds without a poll
        assert!(n <= 60, "{n} datagrams at once (5 ms at 10 kpps is 50)");
    }

    #[test]
    fn settings_change_while_running() {
        let mut g = start(patch(Rate::Pps(1000), Size::Fixed(64)));
        let before = run(&mut g, 1, 1000).len();
        g.apply(
            GenCmd::Set(Patch { rate: Some(Rate::Pps(4000)), size: Some(Size::Fixed(500)), ..Patch::default() }),
            1_000_000,
            0,
        )
        .unwrap();
        let mut after = Vec::new();
        for t in 1_000_000..2_000_000u64 {
            if t % 500 == 0 {
                g.poll(t, &mut |p| {
                    after.push(p.len());
                    true
                });
            }
        }
        assert!((before as i64 - 1000).abs() < 10);
        assert!((3900..=4100).contains(&after.len()), "{}", after.len());
        assert!(after.iter().skip(2).all(|l| *l == 500), "the new size applies at once");
        // An invalid change is refused and changes nothing.
        let status = g.status();
        assert!(g.apply(GenCmd::Set(Patch { size: Some(Size::Fixed(5)), ..Patch::default() }), 0, 0).is_err());
        assert_eq!(g.status().spec, status.spec);
    }

    #[test]
    fn starting_again_ends_the_old_stream_and_numbers_the_new_one() {
        let mut g = start(patch(Rate::Pps(1000), Size::Fixed(64)));
        run(&mut g, 1, 1000);
        g.apply(GenCmd::Start(Patch::default()), 2_000_000, 3).unwrap();
        assert_eq!(g.status().stream, 2);
        assert_eq!(g.status().sent, 0);
        assert!(g.take_summary().is_none(), "the new start discards it");
    }

    #[test]
    fn invalid_settings_are_refused() {
        let bad = |p: Patch| Generator::new().apply(GenCmd::Start(p), 0, 1).is_err();
        assert!(bad(Patch { rate: Some(Rate::Pps(0)), ..Patch::default() }));
        assert!(bad(Patch { rate: Some(Rate::Pps(2_000_000)), ..Patch::default() }));
        assert!(bad(Patch { rate: Some(Rate::Bps(10)), ..Patch::default() }));
        assert!(bad(Patch { size: Some(Size::Fixed(15)), ..Patch::default() }));
        assert!(bad(Patch { size: Some(Size::Fixed(1473)), ..Patch::default() }));
        assert!(bad(Patch { size: Some(Size::Range(300, 200)), ..Patch::default() }));
        assert!(bad(Patch { limit: Some(Limit::Packets(0)), ..Patch::default() }));
        assert!(bad(Patch { shape: Some(Shape::Pulse { on_ms: 0, off_ms: 5 }), ..Patch::default() }));
        assert!(!bad(Patch::default()));
    }

    #[test]
    fn headers_round_trip_and_garbage_is_ignored() {
        let h = Header { stream: 513, end: true, seq: 0xDEAD_BEEF, sent_us: 12345 };
        assert_eq!(Header::parse(&h.encode()), Some(h));
        assert_eq!(Header::parse(b"$DAT,node-A,1,2,3,4,5*00"), None);
        assert_eq!(Header::parse(&[0; 4]), None);
    }

    /// Feeds a receiver the datagrams `order` (sequence numbers), 1 ms apart, each sent 1 ms apart too.
    fn receive(order: &[u32], end_at: Option<u32>, transit: impl Fn(u32) -> u64) -> RxSummary {
        let mut rx = GenRx::new();
        let mut summary = None;
        for (i, &seq) in order.iter().enumerate() {
            let sent = u64::from(seq) * 1000;
            let h = Header { stream: 1, end: false, seq, sent_us: sent as u32 };
            let mut payload = h.encode().to_vec();
            payload.resize(100, 0);
            let at = i as u64 * 1000 + transit(seq);
            if let Some(s) = rx.receive(&payload, at) {
                summary = Some(s);
            }
        }
        if let Some(n) = end_at {
            let h = Header { stream: 1, end: true, seq: n, sent_us: 0 };
            return rx.receive(&h.encode(), 999_000).unwrap();
        }
        summary.unwrap_or_else(|| rx.poll(10_000_000).unwrap())
    }

    #[test]
    fn the_receiver_counts_loss_reordering_and_the_end() {
        let clean: Vec<u32> = (0..100).collect();
        let s = receive(&clean, Some(100), |_| 500);
        assert_eq!((s.received, s.lost, s.reordered, s.ended), (100, 0, 0, true));
        assert_eq!(s.jitter_us, 0, "a constant delay is no jitter");
        assert_eq!(s.wire_bytes, 100 * 142);

        // 10 and 11 missing, and 50 arriving after 51; the end marker says 100 were sent: 98 came.
        let mut order: Vec<u32> = (0..100).filter(|n| *n != 10 && *n != 11).collect();
        order.swap(48, 49);
        let s = receive(&order, Some(100), |_| 500);
        assert_eq!((s.received, s.lost, s.reordered), (98, 2, 1));
        assert_eq!(s.loss_per_10000(), 200);

        // The last three lost: without the end marker the receiver can't know; with it, it does.
        let cut: Vec<u32> = (0..97).collect();
        assert_eq!(receive(&cut, None, |_| 0).lost, 0);
        assert_eq!(receive(&cut, Some(100), |_| 0).lost, 3);
        assert!(!receive(&cut, None, |_| 0).ended);
    }

    #[test]
    fn jitter_follows_the_variation_of_the_delay_not_the_delay() {
        // The delay alternates between 500 and 1500 µs: every step changes it by 1000.
        let order: Vec<u32> = (0..400).collect();
        let s = receive(&order, Some(400), |seq| if seq % 2 == 0 { 500 } else { 1500 });
        assert!((900..=1000).contains(&s.jitter_us), "{} µs", s.jitter_us);
        // Clocks that differ by a lot: same jitter (the clock offset cancels).
        let s2 = receive(&order, Some(400), |seq| if seq % 2 == 0 { 3_000_500 } else { 3_001_500 });
        assert_eq!(s.jitter_us, s2.jitter_us);
    }

    #[test]
    fn a_silent_stream_is_closed_and_a_new_one_starts_clean() {
        let mut rx = GenRx::new();
        let mk = |stream, seq| {
            let mut p = Header { stream, end: false, seq, sent_us: 0 }.encode().to_vec();
            p.resize(64, 0);
            p
        };
        assert!(rx.receive(&mk(1, 0), 0).is_none());
        assert!(rx.receive(&mk(1, 1), 1000).is_none());
        assert!(rx.poll(500_000).is_none(), "not yet idle");
        let s = rx.poll(2_000_000).unwrap();
        assert_eq!((s.received, s.stream, s.ended), (2, 1, false));
        assert!(!rx.is_receiving());
        // A late datagram of the closed stream doesn't reopen it; a new stream does.
        assert!(rx.receive(&mk(1, 2), 2_100_000).is_none() && !rx.is_receiving());
        assert!(rx.receive(&mk(2, 0), 2_200_000).is_none() && rx.is_receiving());
        // A new stream arriving while one is open closes the old one.
        let s = rx.receive(&mk(3, 0), 2_300_000).unwrap();
        assert_eq!((s.stream, s.received), (2, 1));
    }

    #[test]
    fn rates_sizes_and_shapes_are_typed_naturally() {
        for (text, rate) in [
            ("5000", Rate::Pps(5000)),
            ("5000pps", Rate::Pps(5000)),
            ("5kpps", Rate::Pps(5000)),
            ("2Mbit", Rate::Bps(2_000_000)),
            ("1.5Mbit/s", Rate::Bps(1_500_000)),
            ("500kbit", Rate::Bps(500_000)),
            ("1Gbps", Rate::Bps(1_000_000_000)),
            ("64kbit", Rate::Bps(64_000)),
        ] {
            assert_eq!(parse_rate(text), Some(rate), "{text}");
        }
        for bad in ["", "fast", "1x", "-5", "2Mbitt", "1.2345kbit"] {
            assert_eq!(parse_rate(bad), None, "{bad:?}");
        }
        assert_eq!(parse_size("512"), Some(Size::Fixed(512)));
        assert_eq!(parse_size("512B"), Some(Size::Fixed(512)));
        assert_eq!(parse_size("64-1400"), Some(Size::Range(64, 1400)));
        assert_eq!(parse_size("IMIX"), Some(Size::Imix));
        assert_eq!(parse_size("big"), None);
        assert_eq!(parse_shape("ramp:10Mbit:30s"), Some(Shape::Ramp { to: Rate::Bps(10_000_000), over_ms: 30_000 }));
        assert_eq!(parse_shape("pulse:200ms:800ms"), Some(Shape::Pulse { on_ms: 200, off_ms: 800 }));
        assert_eq!(
            parse_shape("pulse:1:2"),
            Some(Shape::Pulse { on_ms: 1000, off_ms: 2000 }),
            "bare numbers are seconds"
        );
        assert_eq!(parse_shape("random"), Some(Shape::Jitter));
        assert_eq!(
            parse_shape("sine:3Mbit:20s"),
            Some(Shape::Wave { kind: Wave::Sine, to: Rate::Bps(3_000_000), period_ms: 20_000 })
        );
        assert_eq!(
            parse_shape("square:5000:500ms"),
            Some(Shape::Wave { kind: Wave::Square, to: Rate::Pps(5000), period_ms: 500 })
        );
        assert_eq!(
            parse_shape("steps:3Mbit:6:30s"),
            Some(Shape::Steps { to: Rate::Bps(3_000_000), steps: 6, over_ms: 30_000 })
        );
        assert_eq!(parse_shape("steps:3Mbit:30s"), None);
        assert_eq!(parse_shape("sine:3Mbit"), None);
        assert_eq!(parse_shape("ramp:5000"), None);
        assert_eq!(parse_millis("2min"), Some(120_000));
        assert_eq!(parse_millis("1.5s"), Some(1500));
    }

    #[test]
    fn short_words_say_what_they_are() {
        let start = |s: &str| match parse_gen(s).unwrap() {
            GenCmd::Start(p) => p,
            other => panic!("{other:?}"),
        };
        // The same as the long form.
        assert_eq!(
            start("start 100kbit 512B sine:3Mbit:20s forever"),
            start("start rate=100kbit size=512 shape=sine:3Mbit:20s for=forever")
        );
        assert_eq!(start("start 200kbit imix 90s").limit, Some(Limit::Millis(90_000)));
        assert_eq!(start("start 5000 64-1400").size, Some(Size::Range(64, 1400)));
        assert_eq!(start("start 5000").rate, Some(Rate::Pps(5000)), "a bare number is a rate");
        assert_eq!(start("start 5000").size, None, "and never a size");
        assert!(start("start triangle:2Mbit:10s").shape.is_some());
        assert_eq!(start("start 2Mbit 30s").limit, Some(Limit::Millis(30_000)));
        assert_eq!(start("start auto").limit, Some(Limit::Auto));
        // The commands people type are not cut by the 96 characters of the command line.
        for line in [
            "gen start 100kbit size=64-1400 shape=triangle:3Mbit:20s for=forever",
            "gen start 100kbit size=imix shape=steps:3Mbit:10:60s for=2min",
        ] {
            assert!(line.len() <= crate::cli::MAX_INPUT, "{} chars", line.len());
        }
    }

    #[test]
    fn the_command_grammar() {
        let start = |s: &str| match parse_gen(s).unwrap() {
            GenCmd::Start(p) => p,
            other => panic!("{other:?}"),
        };
        let p = start("start 2Mbit size=512 for=30s shape=pulse:200ms:800ms");
        assert_eq!(p.rate, Some(Rate::Bps(2_000_000)));
        assert_eq!(p.size, Some(Size::Fixed(512)));
        assert_eq!(p.limit, Some(Limit::Millis(30_000)));
        assert_eq!(p.shape, Some(Shape::Pulse { on_ms: 200, off_ms: 800 }));
        assert_eq!(start("start").rate, None, "what isn't said stays as it was");
        assert_eq!(start("start rate=100 count=5000").limit, Some(Limit::Packets(5000)));
        assert_eq!(start("start for=forever").limit, Some(Limit::Forever));
        assert_eq!(parse_gen(""), Ok(GenCmd::Status));
        assert_eq!(parse_gen("status"), Ok(GenCmd::Status));
        for (text, cmd) in [("stop", GenCmd::Stop), ("pause", GenCmd::Pause), ("resume", GenCmd::Resume)] {
            assert_eq!(parse_gen(text), Ok(cmd));
        }
        assert_eq!(
            parse_gen("rate 50Mbit"),
            Ok(GenCmd::Set(Patch { rate: Some(Rate::Bps(50_000_000)), ..Patch::default() }))
        );
        assert_eq!(parse_gen("size imix"), Ok(GenCmd::Set(Patch { size: Some(Size::Imix), ..Patch::default() })));
        assert_eq!(
            parse_gen("count 100"),
            Ok(GenCmd::Set(Patch { limit: Some(Limit::Packets(100)), ..Patch::default() }))
        );
        assert_eq!(
            parse_gen("for 2min"),
            Ok(GenCmd::Set(Patch { limit: Some(Limit::Millis(120_000)), ..Patch::default() }))
        );
        for bad in [
            "rate",
            "rate x",
            "start size=",
            "start color=red",
            "explode",
            "start for=soon",
            "start shape=zigzag",
            "start size=9000",
            "start 0",
            "size 5",
            "start size=300-200",
            "start count=0",
            "start shape=steps:1000:1:10s",
            "start shape=steps:1000:100:10s",
            "start shape=sine:1000:50ms",
        ] {
            assert!(parse_gen(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn specs_say_what_they_aim_at() {
        let spec = Spec { rate: Rate::Pps(1000), size: Size::Fixed(458), ..Spec::default() };
        assert_eq!((spec.nominal_pps(), spec.nominal_bps()), (1000, 4_000_000), "500 B frames, 1000 a second");
        let spec = Spec { rate: Rate::Bps(4_000_000), size: Size::Fixed(458), ..Spec::default() };
        assert_eq!(spec.nominal_pps(), 1000);
        assert_eq!(Rate::Bps(2_500_000).to_string(), "2.50 Mbit/s");
        assert_eq!(Rate::Pps(5000).to_string(), "5 kpps");
        assert_eq!(Rate::Pps(1234).to_string(), "1234 pps");
        assert_eq!(Limit::Millis(120_000).to_string(), "2 min");
    }
}
