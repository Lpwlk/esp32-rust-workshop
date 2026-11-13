//! Periods and frequencies typed in the CLI: decimals, units, and either form.
//!
//! Wherever a setting is a period, a frequency is accepted too (and the other way
//! round), so `period 250ms`, `period 0.25s` and `period 4Hz` mean the same thing.
//!
//! ```
//! use workshop_core::units::{parse_freq_hz, parse_period_ms, Num};
//!
//! assert_eq!(parse_period_ms("12.5"), Some(12.5));    // no unit: ms
//! assert_eq!(parse_period_ms("2.5s"), Some(2500.0));
//! assert_eq!(parse_period_ms("4Hz"), Some(250.0));     // a frequency, turned into a period
//! assert_eq!(parse_freq_hz("37.5"), Some(37.5));       // no unit: Hz
//! assert_eq!(parse_freq_hz("20ms"), Some(50.0));
//! assert_eq!(Num(12.5).to_string(), "12.5");
//! assert_eq!(Num(2500.0).to_string(), "2500");
//! ```

use core::fmt;

/// Splits `"12.5ms"` into `(12.5, "ms")`; the unit is lowercased-compared later.
fn number_and_unit(text: &str) -> Option<(f32, &str)> {
    let text = text.trim();
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let value: f32 = number.parse().ok()?;
    (value.is_finite() && value > 0.0).then_some((value, unit.trim()))
}

/// A period in ms, from `12.5` / `12.5ms` / `2s` / `250us` (or `µs`), or from a
/// frequency `4Hz` / `2kHz` (= its period). `None` if invalid, zero or negative.
pub fn parse_period_ms(text: &str) -> Option<f32> {
    let (v, unit) = number_and_unit(text)?;
    Some(match unit {
        "" | "ms" => v,
        "s" => v * 1000.0,
        "us" | "µs" => v / 1000.0,
        u if u.eq_ignore_ascii_case("hz") => 1000.0 / v,
        u if u.eq_ignore_ascii_case("khz") => 1.0 / v,
        _ => return None,
    })
}

/// A frequency in Hz, from `37.5` / `37.5Hz` / `2kHz`, or from a period `20ms` /
/// `0.5s` / `500us` (= its frequency).
pub fn parse_freq_hz(text: &str) -> Option<f32> {
    let (v, unit) = number_and_unit(text)?;
    Some(match unit {
        "" => v,
        u if u.eq_ignore_ascii_case("hz") => v,
        u if u.eq_ignore_ascii_case("khz") => v * 1000.0,
        "ms" => 1000.0 / v,
        "s" => 1.0 / v,
        "us" | "µs" => 1_000_000.0 / v,
        _ => return None,
    })
}

/// A number as short as possible: `2500`, `12.5`, `0.333` (3 decimals at most).
/// [`parse_period_ms`] / [`parse_freq_hz`] read it back.
#[derive(Clone, Copy, Debug)]
pub struct Num(pub f32);

impl fmt::Display for Num {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // f64: an f32 times 1000 loses digits above ~16 000 (3600000 ms would
        // print as 3599999.744). Display only, so the soft-float cost is fine.
        let thousandths = (f64::from(self.0) * 1000.0 + 0.5) as u64;
        let (int, mut frac) = (thousandths / 1000, thousandths % 1000);
        let mut s: heapless::String<24> = heapless::String::new();
        let _ = fmt::Write::write_fmt(&mut s, format_args!("{int}"));
        if frac != 0 {
            let mut digits = 3;
            while frac % 10 == 0 {
                frac /= 10;
                digits -= 1;
            }
            let _ = fmt::Write::write_fmt(&mut s, format_args!(".{frac:0digits$}"));
        }
        f.pad(&s)
    }
}

/// A period for humans: `2.5 s`, `12.5 ms`, `250 µs`.
#[derive(Clone, Copy, Debug)]
pub struct Period(pub f32);

impl fmt::Display for Period {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s: heapless::String<24> = heapless::String::new();
        let ms = self.0;
        let _ = if ms >= 1000.0 {
            fmt::Write::write_fmt(&mut s, format_args!("{} s", Num(ms / 1000.0)))
        } else if ms >= 1.0 {
            fmt::Write::write_fmt(&mut s, format_args!("{} ms", Num(ms)))
        } else {
            fmt::Write::write_fmt(&mut s, format_args!("{} µs", Num(ms * 1000.0)))
        };
        f.pad(&s)
    }
}

/// A frequency for humans: `0.4 Hz`, `37.5 Hz`, `2 kHz`.
#[derive(Clone, Copy, Debug)]
pub struct Freq(pub f32);

impl fmt::Display for Freq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s: heapless::String<24> = heapless::String::new();
        let _ = if self.0 >= 1000.0 {
            fmt::Write::write_fmt(&mut s, format_args!("{} kHz", Num(self.0 / 1000.0)))
        } else {
            fmt::Write::write_fmt(&mut s, format_args!("{} Hz", Num(self.0)))
        };
        f.pad(&s)
    }
}

/// `ms` as whole microseconds (rounded), for timers.
pub fn ms_to_us(ms: f32) -> u64 {
    (f64::from(ms) * 1000.0 + 0.5) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods() {
        for (text, ms) in [
            ("250", 250.0),
            ("12.5", 12.5),
            ("12.5ms", 12.5),
            ("0.25s", 250.0),
            ("2s", 2000.0),
            ("500us", 0.5),
            ("500µs", 0.5),
            ("4Hz", 250.0),
            ("4hz", 250.0),
            ("2kHz", 0.5),
            (" 10 ms ", 10.0),
        ] {
            assert_eq!(parse_period_ms(text), Some(ms), "{text}");
        }
        for text in ["", "0", "-5", "abc", "5 parsecs", "1.2.3", "inf", "NaN", ".", "Hz"] {
            assert_eq!(parse_period_ms(text), None, "{text}");
        }
    }

    #[test]
    fn frequencies() {
        for (text, hz) in
            [("50", 50.0), ("37.5Hz", 37.5), ("2kHz", 2000.0), ("20ms", 50.0), ("0.5s", 2.0), ("500us", 2000.0)]
        {
            assert_eq!(parse_freq_hz(text), Some(hz), "{text}");
        }
        assert_eq!(parse_freq_hz("0"), None);
    }

    #[test]
    fn numbers_print_short_and_read_back() {
        for (v, text) in
            [(2500.0, "2500"), (12.5, "12.5"), (0.333, "0.333"), (1.05, "1.05"), (0.001, "0.001"), (7.0, "7")]
        {
            assert_eq!(Num(v).to_string(), text);
            assert_eq!(parse_period_ms(text), Some(v), "{text}");
        }
        assert_eq!(Period(2500.0).to_string(), "2.5 s");
        assert_eq!(Period(12.5).to_string(), "12.5 ms");
        assert_eq!(Period(0.25).to_string(), "250 µs");
        assert_eq!(Freq(0.4).to_string(), "0.4 Hz");
        assert_eq!(Freq(2000.0).to_string(), "2 kHz");
        assert_eq!(ms_to_us(12.5), 12_500);
        assert_eq!(Num(3_600_000.0).to_string(), "3600000");
        assert_eq!(Num(3_599_999.0).to_string(), "3599999");
    }
}
