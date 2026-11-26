//! The potentiometer on **GPIO44** (`ADC1_CH2`, pin `44` of header J2): its position becomes the PWM generator's
//! frequency or its duty cycle, so that turning a knob changes the signal on the ADC input (the spectrum shows it).
//! The arithmetic has no hardware in it, so that it can be tested on the computer
//! (`rustc --edition 2024 --test src/pot.rs`); [`crate::dma`] reads the pin and [`crate::stream`] follows it.
//!
//! ```text
//!  3V3 ── pot ── GND            wiper ─▶ GPIO44 ─▶ ADC1 ch2 ─▶ [`Filter`] ─▶ position 0..1 ─▶ [`Config::value`]
//!                                                                                        │
//!   GPIO20 (PWM) ──▶ GPIO43 (ADC1 ch1) ─▶ the spectrum  ◀── `pwm` frequency or duty ◀───┘ (one of the two)
//! ```
//!
//! The pot owns one number of the generator, chosen by the `pot` command ([`Mode`]); the other one stays what
//! `pwm` set. The ADC's pattern table then holds up to 16 entries, one of them the pot's pin: the pot is read once
//! every `every` conversions (2 to 16, 16 by default) and the signal gets the other ones. A knob needs a hundred
//! readings a second, not thousands, so by default the signal keeps 15/16 of the conversions (78 125 Hz at most,
//! instead of 41 666 Hz when the two pins alternate). While the mode is `off` the pin is not read at all.

/// The input pin and its ADC channel.
pub const PIN: u8 = 44;
pub const CHANNEL: u8 = 2;
/// The pot is read once every `every` conversions: the pattern table has 16 entries at most, and two at least
/// (one for the signal, one for the pot).
pub const EVERY_MIN: u8 = 2;
pub const EVERY_MAX: u8 = 16;
/// The S31's SAR is differential, and an even channel is wired the other way round: its code falls from the
/// zero code (0 V) towards 0 (3.3 V), where an odd channel's (GPIO43, the signal) rises from the zero code to the
/// full scale. Measured on the board: GPIO44 reads 2192 at the GND end of the pot and 1 at the 3V3 end.
pub const INVERTED: bool = CHANNEL.is_multiple_of(2);

/// What the potentiometer drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Not read at all (the ADC samples the signal only).
    Off,
    /// The generator's frequency, from the range's low end to its high end (logarithmic).
    Freq,
    /// The generator's duty cycle, in percent (linear).
    Duty,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Freq => "freq",
            Mode::Duty => "duty",
        }
    }

    pub fn code(self) -> u8 {
        self as u8
    }
}

/// What the knob does: the mode, and the range each mode sweeps (the low end is at the pot's GND end; a low end
/// above the high end reverses the knob).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub mode: Mode,
    /// Hz, both inside the generator's limits.
    pub freq: (f32, f32),
    /// Percent, both from 0 to 100.
    pub duty: (f32, f32),
    /// The pot takes one conversion in this many ([`EVERY_MIN`] to [`EVERY_MAX`]).
    pub every: u8,
}

impl Config {
    pub const DEFAULT: Config =
        Config { mode: Mode::Off, freq: (100.0, 5_000.0), duty: (0.0, 100.0), every: EVERY_MAX };

    /// The range of the current mode: Hz, or percent.
    pub fn range(&self) -> (f32, f32) {
        match self.mode {
            Mode::Duty => self.duty,
            _ => self.freq,
        }
    }

    /// The generator's value for a knob `position` from 0 to 1: Hz in `Freq` mode, percent in `Duty` mode.
    pub fn value(&self, position: f32) -> Option<f32> {
        let position = position.clamp(0.0, 1.0);
        match self.mode {
            Mode::Off => None,
            Mode::Freq => {
                let (low, high) = self.freq;
                Some(low * exp(position * ln(high / low)))
            }
            Mode::Duty => {
                let (low, high) = self.duty;
                Some(low + (high - low) * position)
            }
        }
    }

    /// Takes a parsed `pot` command in. Refuses a range outside the limits or with no width; nothing changes then.
    pub fn apply(&mut self, change: &Change, min_hz: f32, max_hz: f32) -> Result<(), &'static str> {
        let every = change.every.unwrap_or(self.every);
        if !(EVERY_MIN..=EVERY_MAX).contains(&every) {
            return Err("every is 2 to 16 (the pot is read once in that many conversions)");
        }
        let mode = change.mode.unwrap_or(self.mode);
        if mode == Mode::Off && (change.low.is_some() || change.high.is_some()) {
            return Err("pick freq or duty before a range");
        }
        let mut next = *self;
        next.mode = mode;
        next.every = every;
        if change.low.is_some() || change.high.is_some() {
            let (low, high) = if mode == Mode::Duty { self.duty } else { self.freq };
            let range = (change.low.unwrap_or(low), change.high.unwrap_or(high));
            let (floor, ceiling) = if mode == Mode::Duty { (0.0, 100.0) } else { (min_hz, max_hz) };
            if !(floor..=ceiling).contains(&range.0) || !(floor..=ceiling).contains(&range.1) {
                return Err(if mode == Mode::Duty { "duty is 0 to 100 %" } else { "frequency out of range" });
            }
            if range.0 == range.1 {
                return Err("min and max are the same");
            }
            if mode == Mode::Duty {
                next.duty = range;
            } else {
                next.freq = range;
            }
        }
        *self = next;
        Ok(())
    }
}

/// What a `pot` command changes (nothing at all: it asks for the state).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Change {
    pub mode: Option<Mode>,
    pub low: Option<f32>,
    pub high: Option<f32>,
    pub every: Option<u8>,
}

/// `pot [off|freq|duty] [min=N] [max=N] [every=N]`: `freq` and `duty` take their range in Hz and percent, `every`
/// is how many conversions the pot takes one of. `None` when the words make no sense.
pub fn parse<'a>(words: impl Iterator<Item = &'a str>) -> Option<Change> {
    let mut change = Change::default();
    for word in words {
        match word {
            "off" => change.mode = Some(Mode::Off),
            "freq" | "frequency" => change.mode = Some(Mode::Freq),
            "duty" => change.mode = Some(Mode::Duty),
            _ => {
                let (key, value) = word.split_once('=')?;
                let value = value.parse::<f32>().ok().filter(|v| v.is_finite())?;
                match key {
                    "min" | "low" => change.low = Some(value),
                    "max" | "high" => change.high = Some(value),
                    "every" if (0.0..=255.0).contains(&value) && value == f32::from(value as u8) => {
                        change.every = Some(value as u8)
                    }
                    _ => return None,
                }
            }
        }
    }
    Some(change)
}

/// A slow average of the pin's codes: the pot's wiper is a high impedance, and the ADC's input is noisy.
#[derive(Clone, Copy, Debug)]
pub struct Filter {
    /// `weight` × the average code.
    sum: u32,
    /// How many conversions the average spans (1 to 64).
    weight: u32,
    primed: bool,
}

/// The averaging time of the knob, in seconds: the weight follows how often the pot is read, so that a slow pot
/// reading doesn't make a slow knob.
pub const SETTLE_S: f32 = 0.02;

/// How many readings the filter averages for a pot read `pot_hz` times a second: [`SETTLE_S`] worth, 1 to 64.
pub fn filter_weight(pot_hz: f32) -> u32 {
    (pot_hz * SETTLE_S).clamp(1.0, 64.0) as u32
}

impl Filter {
    pub const fn new() -> Self {
        Self { sum: 0, weight: 64, primed: false }
    }

    /// A filter that averages `weight` readings (1 to 64); it starts again.
    pub const fn with_weight(weight: u32) -> Self {
        let weight = if weight < 1 {
            1
        } else if weight > 64 {
            64
        } else {
            weight
        };
        Self { sum: 0, weight, primed: false }
    }

    /// Takes a conversion in: the average moves by a `weight`-th of the way to it.
    pub fn push(&mut self, code: u16) {
        if self.primed {
            self.sum = self.sum - self.sum / self.weight + u32::from(code);
        } else {
            self.sum = u32::from(code) * self.weight;
            self.primed = true;
        }
    }

    /// Forgets everything (the next code starts the average again), and averages `weight` readings from now on.
    pub fn reset_with(&mut self, weight: u32) {
        *self = Self::with_weight(weight);
    }

    /// Forgets everything (the next code starts the average again).
    pub fn reset(&mut self) {
        *self = Self::with_weight(self.weight);
    }

    /// The averaged code, or `None` before the first one.
    pub fn code(&self) -> Option<f32> {
        self.primed.then(|| self.sum as f32 / self.weight as f32)
    }

    /// The knob's position from 0 (GND end) to 1 (3V3 end), with the end of the travel snapped: the ADC never
    /// reads exactly its limits, and the ends of a cheap pot are not exact either.
    pub fn position(&self, zero: u16, full: u16) -> Option<f32> {
        let span = f32::from(full) - f32::from(zero);
        let (code, zero) = (self.code()?, f32::from(zero));
        let raw = if INVERTED { (zero - code) / span } else { (code - zero) / span };
        Some(((raw - EDGE) / (1.0 - 2.0 * EDGE)).clamp(0.0, 1.0))
    }
}

impl Default for Filter {
    fn default() -> Self {
        Self::new()
    }
}

/// The conversion rate that gives the signal `signal_hz` when the pot takes one conversion in `every`
/// (`every` = 1 stands for no pot at all).
pub fn conversion_rate(signal_hz: u32, every: u8) -> u32 {
    if every <= 1 {
        return signal_hz;
    }
    let every = u64::from(every);
    (u64::from(signal_hz) * every).div_ceil(every - 1).min(u64::from(u32::MAX)) as u32
}

/// What the signal gets of a conversion rate: all of it with no pot (`every` = 1), else all but the pot's share.
pub fn signal_share(conversion: u64, every: u8) -> u64 {
    let every = u64::from(every.max(1));
    if every == 1 { conversion } else { conversion * (every - 1) / every }
}

/// The four SAR1 pattern tables for `entries` conversions in turn: the signal's channel in all but the last, the
/// pot's channel in the last (with one entry: the signal alone). An entry is six bits (the channel, shifted by
/// two), the first in bits 23:18 of a table and each next one six bits lower (`adc_ll_digi_set_pattern_table`).
pub fn pattern_tables(signal: u8, pot: u8, entries: u8) -> [u32; 4] {
    let mut tables = [0u32; 4];
    for k in 0..usize::from(entries.clamp(1, 16)) {
        let channel = if entries > 1 && k == usize::from(entries) - 1 { pot } else { signal };
        tables[k / 4] |= (u32::from(channel & 0xF) << 2) << (18 - 6 * (k % 4));
    }
    tables
}

/// The part of the travel at each end that reads as exactly 0 or 1.
const EDGE: f32 = 0.015;
/// The knob has to move this much (0.5 % of its travel) before the generator is changed again: no flicker from
/// the noise, and each change restarts the wave.
pub const DEADBAND: f32 = 0.005;

/// Decides when the generator follows the knob.
#[derive(Clone, Copy, Debug, Default)]
pub struct Follower {
    last: Option<f32>,
}

impl Follower {
    /// `true` when `position` is far enough from the one last followed (or the first): follow it now.
    pub fn moved(&mut self, position: f32) -> bool {
        let far = match self.last {
            Some(last) => {
                (position - last).abs() >= DEADBAND || (position != last && (position == 0.0 || position == 1.0))
            }
            None => true,
        };
        if far {
            self.last = Some(position);
        }
        far
    }

    /// Follows the next position again, whatever it is (the mode or the range changed).
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Bytes of a `POT1` datagram.
pub const DATAGRAM: usize = 16;

/// The live state, for the computer's page: `POT1` (4 bytes), the mode (`u8`, [`Mode::code`]), `every` (`u8`: the
/// pot takes one conversion in that many), the position in hundredths of a percent (`u16`, 0 to 10000), the
/// averaged code (`u16`), two spare bytes, and the value the generator was given (`f32`: Hz or percent; 0 for
/// `off`). Little-endian.
pub fn encode(mode: Mode, every: u8, position: f32, code: f32, value: f32) -> [u8; DATAGRAM] {
    let mut out = [0u8; DATAGRAM];
    out[0..4].copy_from_slice(b"POT1");
    out[4] = mode.code();
    out[5] = every;
    out[6..8].copy_from_slice(&((position.clamp(0.0, 1.0) * 10_000.0 + 0.5) as u16).to_le_bytes());
    out[8..10].copy_from_slice(&((code.max(0.0) + 0.5) as u16).to_le_bytes());
    out[12..16].copy_from_slice(&value.to_le_bytes());
    out
}

// `core` has no `exp` and `ln` (they are `std`'s): these two are accurate to about 1e-6, far below what a
// frequency needs (the generator's own step is 6 ns in a period).

/// Natural logarithm for `x > 0`.
fn ln(x: f32) -> f32 {
    // x = m · 2^e with m in [1, 2): ln x = e ln 2 + ln m, and ln m = 2 atanh((m - 1) / (m + 1)).
    let bits = x.to_bits();
    let e = ((bits >> 23) & 0xFF) as i32 - 127;
    let m = f32::from_bits((bits & 0x007F_FFFF) | 0x3F80_0000);
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let series = s * (2.0 + s2 * (2.0 / 3.0 + s2 * (2.0 / 5.0 + s2 * (2.0 / 7.0 + s2 * (2.0 / 9.0)))));
    e as f32 * core::f32::consts::LN_2 + series
}

/// `e` to the `x`, for `|x|` up to about 20.
fn exp(x: f32) -> f32 {
    // x = k ln 2 + r with |r| <= ln 2 / 2: e^x = 2^k · e^r, e^r from its Taylor series.
    let k = (x / core::f32::consts::LN_2 + if x >= 0.0 { 0.5 } else { -0.5 }) as i32;
    let r = x - k as f32 * core::f32::consts::LN_2;
    let series = 1.0
        + r * (1.0 + r / 2.0 * (1.0 + r / 3.0 * (1.0 + r / 4.0 * (1.0 + r / 5.0 * (1.0 + r / 6.0 * (1.0 + r / 7.0))))));
    series * f32::from_bits(((k + 127) as u32) << 23)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO: u16 = 2198;
    const FULL: u16 = 4393;

    fn words(text: &str) -> Option<Change> {
        parse(text.split_ascii_whitespace())
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= b.abs() * 1e-4 + 1e-4
    }

    #[test]
    fn the_math_functions_agree_with_std() {
        let mut x = 0.01f32;
        while x < 1.0e6 {
            assert!(close(ln(x), x.ln()), "ln({x}) = {} instead of {}", ln(x), x.ln());
            x *= 1.31;
        }
        let mut y = -15.0f32;
        while y < 15.0 {
            assert!(close(exp(y), y.exp()), "exp({y}) = {} instead of {}", exp(y), y.exp());
            y += 0.37;
        }
    }

    #[test]
    fn a_frequency_sweeps_the_range_by_decades() {
        let c = Config { mode: Mode::Freq, ..Config::DEFAULT };
        assert!(close(c.value(0.0).unwrap(), 100.0));
        assert!(close(c.value(1.0).unwrap(), 5000.0));
        // The middle of a logarithmic range is the geometric mean: sqrt(100 · 5000).
        assert!(close(c.value(0.5).unwrap(), 707.1068));
        assert!(close(c.value(-3.0).unwrap(), 100.0), "out of the knob's travel is clamped");
        assert!(close(c.value(7.0).unwrap(), 5000.0));
    }

    #[test]
    fn a_duty_is_linear_and_a_reversed_range_reverses_the_knob() {
        let mut c = Config { mode: Mode::Duty, ..Config::DEFAULT };
        assert!(close(c.value(0.25).unwrap(), 25.0));
        c.duty = (90.0, 10.0);
        assert!(close(c.value(0.0).unwrap(), 90.0));
        assert!(close(c.value(0.5).unwrap(), 50.0));
        assert!(close(c.value(1.0).unwrap(), 10.0));
        c.freq = (5000.0, 100.0);
        c.mode = Mode::Freq;
        assert!(close(c.value(0.0).unwrap(), 5000.0));
        assert!(close(c.value(1.0).unwrap(), 100.0));
    }

    #[test]
    fn off_gives_nothing() {
        assert_eq!(Config::DEFAULT.value(0.5), None);
    }

    #[test]
    fn the_command_words() {
        assert_eq!(words(""), Some(Change::default()));
        assert_eq!(words("freq"), Some(Change { mode: Some(Mode::Freq), ..Change::default() }));
        assert_eq!(
            words("duty min=10 max=90"),
            Some(Change { mode: Some(Mode::Duty), low: Some(10.0), high: Some(90.0), every: None })
        );
        assert_eq!(words("min=20 high=2e4").map(|c| (c.low, c.high)), Some((Some(20.0), Some(20000.0))));
        assert_eq!(words("off").unwrap().mode, Some(Mode::Off));
        assert_eq!(words("freq min=abc"), None);
        assert_eq!(words("freq max=inf"), None);
        assert_eq!(words("freq speed=3"), None);
        assert_eq!(words("loud"), None);
        assert_eq!(words("10"), None);
        assert_eq!(words("every=8").unwrap().every, Some(8));
        assert_eq!(words("freq every=2 min=50").unwrap().every, Some(2));
        assert_eq!(words("every=2.5"), None, "a whole number of conversions");
        assert_eq!(words("every=-1"), None);
        assert_eq!(words("every=300"), None);
    }

    #[test]
    fn every_is_checked_and_kept_with_the_mode() {
        let mut c = Config::DEFAULT;
        assert_eq!(c.every, 16);
        let limits = (0.2, 1_000_000.0);
        let apply = |c: &mut Config, text: &str| c.apply(&words(text).unwrap(), limits.0, limits.1);
        assert_eq!(apply(&mut c, "freq every=4"), Ok(()));
        assert_eq!((c.mode, c.every), (Mode::Freq, 4));
        assert!(apply(&mut c, "every=1").is_err(), "the pot needs one conversion of its own, the signal another");
        assert!(apply(&mut c, "every=17").is_err());
        assert!(apply(&mut c, "every=0").is_err());
        assert_eq!(c.every, 4, "a refused value changes nothing");
        assert_eq!(apply(&mut c, "off every=2"), Ok(()));
        assert_eq!((c.mode, c.every), (Mode::Off, 2), "it can be set while the pot is off");
        assert_eq!(apply(&mut c, "duty"), Ok(()));
        assert_eq!(c.every, 2, "a mode keeps it");
    }

    #[test]
    fn the_signal_keeps_what_the_pot_leaves_it() {
        // The pot takes one conversion in `every`: ask 20 kHz of signal and the ADC runs a bit faster.
        assert_eq!(conversion_rate(20_000, 1), 20_000, "no pot");
        assert_eq!(conversion_rate(20_000, 2), 40_000);
        assert_eq!(conversion_rate(15_000, 16), 16_000);
        assert_eq!(conversion_rate(75_000, 16), 80_000);
        assert_eq!(signal_share(83_333_000, 16), 78_124_687, "15/16 of 83.3 kS/s, in mHz");
        assert_eq!(signal_share(83_333_000, 2), 41_666_500);
        assert_eq!(signal_share(83_333_000, 1), 83_333_000);
        for every in EVERY_MIN..=EVERY_MAX {
            let wanted = 10_000;
            let share = signal_share(u64::from(conversion_rate(wanted, every)) * 1000, every);
            assert!(share >= u64::from(wanted) * 1000, "every={every}: the signal gets {share} mHz");
        }
    }

    #[test]
    fn the_pattern_tables_hold_the_signal_and_the_pot() {
        let s = 1u8;
        let k = 2u8;
        // Alone: one entry, the signal.
        assert_eq!(pattern_tables(s, k, 1), [(1 << 2) << 18, 0, 0, 0]);
        // Two entries: signal in 23:18, pot in 17:12.
        assert_eq!(pattern_tables(s, k, 2), [((1 << 2) << 18) | ((2 << 2) << 12), 0, 0, 0]);
        // Sixteen: fifteen signal entries and the pot last, in the fourth table's last slot (5:0).
        let t = pattern_tables(s, k, 16);
        let signal_entry = 1u32 << 2;
        assert_eq!(t[0], signal_entry << 18 | signal_entry << 12 | signal_entry << 6 | signal_entry);
        assert_eq!(t[3] & 0x3F, 2 << 2, "the pot's entry is the last");
        assert_eq!((t[3] >> 6) & 0x3F, signal_entry, "the one before is the signal's");
        assert!(t.iter().all(|w| w >> 24 == 0), "each table is 24 bits");
        // Five entries: the pot is the first slot of the second table.
        let t = pattern_tables(s, k, 5);
        assert_eq!(t[1], (2 << 2) << 18);
        assert_eq!(t[0] & 0x3F, signal_entry);
    }

    #[test]
    fn the_filter_settles_in_the_same_time_whatever_the_pot_rate() {
        assert_eq!(filter_weight(5_208.0), 64, "a quick pot reading averages 64");
        assert_eq!(filter_weight(1_250.0), 25);
        assert_eq!(filter_weight(38.0), 1, "a slow one doesn't average at all");
        assert_eq!(filter_weight(0.0), 1);
        let mut f = Filter::with_weight(1);
        f.push(1000);
        f.push(3000);
        assert_eq!(f.code(), Some(3000.0), "weight 1 is the last reading");
        let mut f = Filter::with_weight(25);
        for _ in 0..300 {
            f.push(2000);
        }
        assert!((f.code().unwrap() - 2000.0).abs() < 1.0);
        f.reset();
        assert_eq!(f.code(), None);
        f.push(500);
        assert_eq!(f.code(), Some(500.0), "a reset keeps the weight and starts again");
    }

    #[test]
    fn a_range_is_checked_before_it_is_taken() {
        let mut c = Config::DEFAULT;
        let limits = (0.2, 1_000_000.0);
        let apply = |c: &mut Config, text: &str| c.apply(&words(text).unwrap(), limits.0, limits.1);
        assert_eq!(apply(&mut c, "freq"), Ok(()));
        assert_eq!(c.mode, Mode::Freq);
        assert_eq!(apply(&mut c, "min=50 max=20000"), Ok(()));
        assert_eq!(c.freq, (50.0, 20000.0));
        assert_eq!(apply(&mut c, "max=2000000"), Err("frequency out of range"));
        assert_eq!(apply(&mut c, "min=0.1"), Err("frequency out of range"));
        assert_eq!(apply(&mut c, "min=50 max=50"), Err("min and max are the same"));
        assert_eq!(c.freq, (50.0, 20000.0), "nothing changed");
        assert_eq!(apply(&mut c, "duty min=10 max=120"), Err("duty is 0 to 100 %"));
        assert_eq!(c.mode, Mode::Freq, "nor the mode");
        assert_eq!(apply(&mut c, "duty min=10 max=60"), Ok(()));
        assert_eq!((c.mode, c.duty, c.freq), (Mode::Duty, (10.0, 60.0), (50.0, 20000.0)));
        assert_eq!(apply(&mut c, "off"), Ok(()));
        assert_eq!(apply(&mut c, "max=40"), Err("pick freq or duty before a range"));
        assert_eq!(c.range(), (50.0, 20000.0));
    }

    #[test]
    fn the_filter_settles_and_the_ends_of_the_travel_snap() {
        // The code at 0 V, at 3.3 V and half way, whichever way round the channel reads.
        let (gnd, rail) = if INVERTED { (ZERO, 0) } else { (ZERO, FULL) };
        let mut f = Filter::new();
        assert_eq!(f.position(ZERO, FULL), None);
        for _ in 0..600 {
            f.push(gnd);
        }
        assert_eq!(f.position(ZERO, FULL), Some(0.0));
        for _ in 0..600 {
            f.push(rail);
        }
        assert_eq!(f.position(ZERO, FULL), Some(1.0));
        for _ in 0..600 {
            f.push((gnd + rail) / 2);
        }
        let middle = f.position(ZERO, FULL).unwrap();
        assert!((middle - 0.5).abs() < 0.01, "the middle reads {middle}");
        f.reset();
        assert_eq!(f.code(), None);
    }

    #[test]
    fn the_filter_averages_the_noise_out() {
        let mut f = Filter::new();
        let mut noise = 12345u32;
        for _ in 0..4000 {
            noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let jitter = (noise >> 24) as i32 % 21 - 10;
            f.push((3300 + jitter) as u16);
        }
        let code = f.code().unwrap();
        assert!((code - 3300.0).abs() < 4.0, "averaged to {code}");
    }

    #[test]
    fn the_follower_ignores_the_noise_and_follows_a_turn() {
        let mut follower = Follower::default();
        assert!(follower.moved(0.5), "the first one is followed");
        assert!(!follower.moved(0.502));
        assert!(!follower.moved(0.497), "(0.497 is still within the dead band of 0.5)");
        assert!(follower.moved(0.51));
        assert!(!follower.moved(0.512));
        assert!(follower.moved(0.0), "the end of the travel is reached exactly");
        assert!(!follower.moved(0.0));
        follower.reset();
        assert!(follower.moved(0.0), "after a reset it follows whatever comes");
    }

    #[test]
    fn the_datagram_layout() {
        let d = encode(Mode::Freq, 16, 0.5, 3295.4, 707.25);
        assert_eq!(&d[0..4], b"POT1");
        assert_eq!(d[4], 1);
        assert_eq!(d[5], 16);
        assert_eq!(u16::from_le_bytes([d[6], d[7]]), 5000);
        assert_eq!(u16::from_le_bytes([d[8], d[9]]), 3295);
        assert_eq!(f32::from_le_bytes([d[12], d[13], d[14], d[15]]), 707.25);
        assert_eq!(encode(Mode::Duty, 2, 7.0, -3.0, 0.0)[6..8], 10_000u16.to_le_bytes());
        assert_eq!(d.len(), DATAGRAM);
    }
}
