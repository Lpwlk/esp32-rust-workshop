//! The arithmetic of the PWM generator ([`crate::pwm`]), with no hardware in it, so that it can be tested on
//! the computer (`rustc --test src/pwm_plan.rs`).
//!
//! The generator is the RMT's transmitter in loop mode: the channel RAM holds one period of the wave as
//! *pulse codes*, each of them two (level, length) pairs with a 15-bit length in ticks, and the hardware
//! replays it forever. A period longer than 32 767 ticks is written as several segments of the same level,
//! so a period can take up to [`SEGMENTS`] chunks: at the RMT's 80 MHz that is 26 Hz and up with the finest
//! tick (12.5 ns), and 0.2 Hz with the channel's divider.

/// Slowest and fastest wave the generator makes, Hz.
pub const MIN_HZ: f32 = 0.2;
pub const MAX_HZ: f32 = 1_000_000.0;

/// Pulse codes of one channel RAM block (48 words), the last of which is the end marker the loop needs.
pub const CODES: usize = 47;
/// Segments of one period (two per pulse code).
pub const SEGMENTS: usize = 2 * CODES;
/// The longest segment a pulse code holds, in ticks (15 bits).
const CHUNK: u32 = 32_767;
/// Fewest ticks in a period: below that the duty cycle would have no resolution left.
const MIN_PERIOD: u32 = 8;

/// One period, in ticks of the divided RMT clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// The channel's clock divider (1 to 255).
    pub divider: u8,
    /// Ticks of the whole period, and of its high part.
    pub period: u32,
    pub high: u32,
    /// The tick rate, Hz.
    pub tick_hz: f32,
}

impl Plan {
    /// The frequency the plan really makes (the period is a whole number of ticks).
    pub fn freq(&self) -> f32 {
        self.tick_hz / self.period as f32
    }

    /// The duty cycle the plan really makes, in percent.
    pub fn duty(&self) -> f32 {
        self.high as f32 * 100.0 / self.period as f32
    }
}

/// How many segments `ticks` takes.
fn chunks(ticks: u32) -> u32 {
    ticks.div_ceil(CHUNK)
}

/// The finest plan for `freq` Hz and `duty` percent, with the RMT clock at `clock_hz`: the smallest divider
/// whose period fits in the channel RAM. `None` outside [`MIN_HZ`]..=[`MAX_HZ`] (or for a NaN).
pub fn plan(clock_hz: u32, freq: f32, duty: f32) -> Option<Plan> {
    if !(MIN_HZ..=MAX_HZ).contains(&freq) || duty.is_nan() {
        return None;
    }
    let duty = duty.clamp(0.0, 100.0);
    for divider in 1..=255u32 {
        let tick_hz = clock_hz as f32 / divider as f32;
        let period = (tick_hz / freq + 0.5) as u32;
        if period < MIN_PERIOD {
            return None;
        }
        let high = ((period as f32 * duty / 100.0 + 0.5) as u32).min(period);
        if chunks(high) + chunks(period - high) <= SEGMENTS as u32 {
            return Some(Plan { divider: divider as u8, period, high, tick_hz });
        }
    }
    None
}

/// The period as (level, ticks) segments, none of them empty (a zero length is the end marker) and in an
/// even number, to fill whole pulse codes. Returns how many.
pub fn segments(plan: &Plan, out: &mut [(bool, u16); SEGMENTS]) -> usize {
    let mut n = 0;
    for (level, mut ticks) in [(true, plan.high), (false, plan.period - plan.high)] {
        while ticks > 0 {
            let len = ticks.min(CHUNK);
            out[n] = (level, len as u16);
            n += 1;
            ticks -= len;
        }
    }
    if n % 2 == 1 {
        // An odd count: cut the longest segment in two (n is at most 93 here, and it has at least 2 ticks).
        let longest = (0..n).max_by_key(|&i| out[i].1).unwrap_or(0);
        let (level, len) = out[longest];
        for i in (longest + 1..=n).rev() {
            out[i] = out[i - 1];
        }
        out[longest] = (level, len / 2);
        out[longest + 1] = (level, len - len / 2);
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOCK: u32 = 80_000_000;

    fn check(freq: f32, duty: f32) -> Plan {
        let p = plan(CLOCK, freq, duty).unwrap_or_else(|| panic!("no plan for {freq} Hz"));
        let mut seg = [(false, 0u16); SEGMENTS];
        let n = segments(&p, &mut seg);
        assert!(n % 2 == 0 && n >= 2 && n <= SEGMENTS, "{freq} Hz: {n} segments");
        assert!(seg[..n].iter().all(|&(_, len)| len >= 1 && u32::from(len) <= CHUNK), "{freq} Hz: empty segment");
        let total: u32 = seg[..n].iter().map(|&(_, len)| u32::from(len)).sum();
        assert_eq!(total, p.period, "{freq} Hz {duty} %: the segments add up to the period");
        let high: u32 = seg[..n].iter().filter(|s| s.0).map(|&(_, len)| u32::from(len)).sum();
        assert_eq!(high, p.high, "{freq} Hz {duty} %: the high part");
        // High first, then low: one rising edge per period.
        let edges = seg[..n].windows(2).filter(|w| w[0].0 != w[1].0).count();
        assert!(edges <= 1, "{freq} Hz {duty} %: {edges} level changes inside the period");
        assert!((p.freq() - freq).abs() <= freq / p.period as f32 * 0.51 + 1e-4, "{freq} Hz made {}", p.freq());
        p
    }

    #[test]
    fn a_thousand_hertz_takes_the_finest_tick() {
        let p = check(1000.0, 25.0);
        assert_eq!((p.divider, p.period, p.high), (1, 80_000, 20_000));
        assert_eq!(p.freq(), 1000.0);
        assert_eq!(p.duty(), 25.0);
    }

    #[test]
    fn the_whole_range_and_every_duty() {
        let mut f = MIN_HZ;
        while f <= MAX_HZ {
            for duty in [0.0, 0.001, 1.0, 25.0, 50.0, 99.0, 99.999, 100.0] {
                check(f, duty);
            }
            f *= 1.37;
        }
        check(MIN_HZ, 50.0);
        check(MAX_HZ, 50.0);
        check(26.0, 50.0);
        check(25.0, 33.0);
    }

    #[test]
    fn slow_waves_use_the_divider() {
        assert_eq!(check(100.0, 50.0).divider, 1);
        assert!(check(5.0, 50.0).divider > 1);
        assert!(check(MIN_HZ, 50.0).divider > 100);
    }

    #[test]
    fn constant_levels() {
        let p = check(1000.0, 0.0);
        assert_eq!(p.high, 0);
        let p = check(1000.0, 100.0);
        assert_eq!(p.high, p.period);
    }

    #[test]
    fn out_of_range_is_refused() {
        assert!(plan(CLOCK, 0.1, 50.0).is_none());
        assert!(plan(CLOCK, 2_000_000.0, 50.0).is_none());
        assert!(plan(CLOCK, f32::NAN, 50.0).is_none());
        assert!(plan(CLOCK, 1000.0, f32::NAN).is_none());
        // A duty outside 0..100 is clamped, not refused.
        assert_eq!(plan(CLOCK, 1000.0, 250.0).map(|p| p.high), Some(80_000));
        assert_eq!(plan(CLOCK, 1000.0, -3.0).map(|p| p.high), Some(0));
    }
}
