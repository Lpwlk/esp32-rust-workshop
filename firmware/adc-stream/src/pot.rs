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
//! `pwm` set. The ADC's pattern table then alternates the two pins, so the signal's sample rate is half of the
//! conversion rate (at most 41 666 Hz): that costs nothing while the mode is `off`.

/// The input pin and its ADC channel.
pub const PIN: u8 = 44;
pub const CHANNEL: u8 = 2;

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
}

impl Config {
    pub const DEFAULT: Config = Config { mode: Mode::Off, freq: (100.0, 5_000.0), duty: (0.0, 100.0) };

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
        let mode = change.mode.unwrap_or(self.mode);
        if mode == Mode::Off && (change.low.is_some() || change.high.is_some()) {
            return Err("pick freq or duty before a range");
        }
        let mut next = *self;
        next.mode = mode;
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
}

/// `pot [off|freq|duty] [min=N] [max=N]`: `freq` and `duty` take their range in Hz and percent. `None` when the
/// words make no sense.
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
    /// 64 × the average code.
    sum: u32,
    primed: bool,
}

impl Filter {
    pub const fn new() -> Self {
        Self { sum: 0, primed: false }
    }

    /// Takes a conversion in: the average moves by a 64th of the way to it.
    pub fn push(&mut self, code: u16) {
        if self.primed {
            self.sum = self.sum - self.sum / 64 + u32::from(code);
        } else {
            self.sum = u32::from(code) * 64;
            self.primed = true;
        }
    }

    /// Forgets everything (the next code starts the average again).
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The averaged code, or `None` before the first one.
    pub fn code(&self) -> Option<f32> {
        self.primed.then(|| self.sum as f32 / 64.0)
    }

    /// The knob's position from 0 (GND end) to 1 (3V3 end), with the end of the travel snapped: the ADC never
    /// reads exactly its limits, and the ends of a cheap pot are not exact either.
    pub fn position(&self, zero: u16, full: u16) -> Option<f32> {
        let span = f32::from(full) - f32::from(zero);
        let raw = (self.code()? - f32::from(zero)) / span;
        Some(((raw - EDGE) / (1.0 - 2.0 * EDGE)).clamp(0.0, 1.0))
    }
}

impl Default for Filter {
    fn default() -> Self {
        Self::new()
    }
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

/// The live state, for the computer's page: `POT1` (4 bytes), the mode (`u8`, [`Mode::code`]), a spare byte,
/// the position in hundredths of a percent (`u16`, 0 to 10000), the averaged code (`u16`), two spare bytes, and
/// the value the generator was given (`f32`: Hz or percent; 0 for `off`). Little-endian.
pub fn encode(mode: Mode, position: f32, code: f32, value: f32) -> [u8; DATAGRAM] {
    let mut out = [0u8; DATAGRAM];
    out[0..4].copy_from_slice(b"POT1");
    out[4] = mode.code();
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
            Some(Change { mode: Some(Mode::Duty), low: Some(10.0), high: Some(90.0) })
        );
        assert_eq!(words("min=20 high=2e4").map(|c| (c.low, c.high)), Some((Some(20.0), Some(20000.0))));
        assert_eq!(words("off").unwrap().mode, Some(Mode::Off));
        assert_eq!(words("freq min=abc"), None);
        assert_eq!(words("freq max=inf"), None);
        assert_eq!(words("freq speed=3"), None);
        assert_eq!(words("loud"), None);
        assert_eq!(words("10"), None);
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
        let mut f = Filter::new();
        assert_eq!(f.position(ZERO, FULL), None);
        for _ in 0..600 {
            f.push(ZERO);
        }
        assert_eq!(f.position(ZERO, FULL), Some(0.0));
        for _ in 0..600 {
            f.push(FULL);
        }
        assert_eq!(f.position(ZERO, FULL), Some(1.0));
        for _ in 0..600 {
            f.push((ZERO + FULL) / 2);
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
        let d = encode(Mode::Freq, 0.5, 3295.4, 707.25);
        assert_eq!(&d[0..4], b"POT1");
        assert_eq!(d[4], 1);
        assert_eq!(u16::from_le_bytes([d[6], d[7]]), 5000);
        assert_eq!(u16::from_le_bytes([d[8], d[9]]), 3295);
        assert_eq!(f32::from_le_bytes([d[12], d[13], d[14], d[15]]), 707.25);
        assert_eq!(encode(Mode::Duty, 7.0, -3.0, 0.0)[6..8], 10_000u16.to_le_bytes());
        assert_eq!(d.len(), DATAGRAM);
    }
}
