//! Color fade patterns for the on-board RGB LED: palettes and color math.
//!
//! A pattern is a loop of key colors. The LED moves from one key to the next with
//! a smoothstep ease (no visible "corner" at each key), then gamma correction is
//! applied so the fade looks even to the eye (LEDs are linear, eyes are not).
//! The timing (which phase to show now) is done by the firmware's `fx::Animator`.
//!
//! ```
//! use workshop_core::fx::{color_at, find, PATTERNS};
//!
//! let ocean = &PATTERNS[find("ocean").unwrap()];
//! // Phase 0 is the first key color; level 0 is black whatever the phase.
//! assert_eq!(color_at(ocean.keys, 0.0, 1.0), color_at(ocean.keys, 1.0, 1.0));
//! assert_eq!(color_at(ocean.keys, 0.3, 0.0), workshop_core::fx::RGB8 { r: 0, g: 0, b: 0 });
//! ```

pub use smart_leds_trait::RGB8;

const fn rgb(r: u8, g: u8, b: u8) -> RGB8 {
    RGB8 { r, g, b }
}

/// A named loop of key colors.
pub struct Pattern {
    /// Name used by the CLI and the config.
    pub name: &'static str,
    /// One line for `list`.
    pub description: &'static str,
    /// Key colors, in perceptual space (before gamma). The loop wraps to the first one.
    pub keys: &'static [RGB8],
}

/// All patterns, in `list` order.
pub const PATTERNS: [Pattern; 5] = [
    Pattern {
        name: "viridis",
        description: "matplotlib's viridis: purple, blue, teal, green, yellow",
        // The 5 reference stops of viridis (#440154 #3B528B #21918C #5EC962 #FDE725),
        // walked forward then back so the loop stays smooth (no jump yellow -> purple).
        keys: &[
            rgb(68, 1, 84),
            rgb(59, 82, 139),
            rgb(33, 145, 140),
            rgb(94, 201, 98),
            rgb(253, 231, 37),
            rgb(94, 201, 98),
            rgb(33, 145, 140),
            rgb(59, 82, 139),
        ],
    },
    Pattern {
        name: "sunset",
        description: "gold, orange, coral, purple dusk",
        keys: &[rgb(255, 170, 0), rgb(255, 90, 0), rgb(255, 30, 80), rgb(150, 0, 160)],
    },
    Pattern {
        name: "ocean",
        description: "deep blue, azure, lagoon turquoise",
        keys: &[rgb(0, 40, 255), rgb(0, 140, 255), rgb(0, 230, 200), rgb(0, 180, 255)],
    },
    Pattern {
        name: "aurora",
        description: "northern lights: emerald, ice blue, violet",
        keys: &[rgb(0, 255, 110), rgb(0, 190, 255), rgb(110, 40, 255), rgb(0, 255, 180)],
    },
    Pattern {
        name: "sakura",
        description: "cherry blossom: pink, pale rose, lavender",
        keys: &[rgb(255, 80, 140), rgb(255, 150, 190), rgb(190, 120, 255), rgb(255, 200, 220)],
    },
];

/// `const` version of [`find`] (exact, case-sensitive), used by the compile-time config check.
pub const fn find_const(name: &str) -> Option<usize> {
    let name = name.as_bytes();
    let mut i = 0;
    while i < PATTERNS.len() {
        let candidate = PATTERNS[i].name.as_bytes();
        if candidate.len() == name.len() {
            let mut j = 0;
            while j < name.len() && candidate[j] == name[j] {
                j += 1;
            }
            if j == name.len() {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Finds a pattern by name, ignoring case.
pub fn find(name: &str) -> Option<usize> {
    PATTERNS.iter().position(|p| p.name.eq_ignore_ascii_case(name))
}

/// Accepted `rgb.freq_hz` values (inclusive).
pub const FREQ_RANGE: (f32, f32) = (1.0, 200.0);
/// Accepted `rgb.cycle_ms` values (inclusive).
pub const CYCLE_RANGE: (f32, f32) = (200.0, 60_000.0);

/// Runtime-adjustable settings (changed from the CLI).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Index in [`PATTERNS`].
    pub pattern: usize,
    /// LED refresh rate, in Hz.
    pub freq_hz: f32,
    /// Duration of one full loop through the pattern, in ms.
    pub cycle_ms: f32,
    /// 0-100 %.
    pub brightness: u8,
}

impl Settings {
    /// The selected pattern.
    pub fn pattern(&self) -> &'static Pattern {
        &PATTERNS[self.pattern]
    }
}

/// Color of `keys` at `phase` (0..1), scaled by `level` (0..1), gamma-corrected.
pub fn color_at(keys: &[RGB8], phase: f32, level: f32) -> RGB8 {
    let n = keys.len();
    let pos = phase * n as f32;
    let i = (pos as usize) % n;
    let f = pos - pos as usize as f32;
    let e = f * f * (3.0 - 2.0 * f); // smoothstep
    let (a, b) = (keys[i], keys[(i + 1) % n]);
    let mix = |x: u8, y: u8| {
        let v = (f32::from(x) + (f32::from(y) - f32::from(x)) * e) / 255.0 * level;
        // Gamma 2.0: cheap and close enough to the eye's response for a status LED.
        (v * v * 255.0 + 0.5) as u8
    };
    RGB8 { r: mix(a.r, b.r), g: mix(a.g, b.g), b: mix(a.b, b.b) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_vectors;

    fn gamma(c: RGB8) -> RGB8 {
        let g = |x: u8| ((f32::from(x) / 255.0).powi(2) * 255.0 + 0.5) as u8;
        RGB8 { r: g(c.r), g: g(c.g), b: g(c.b) }
    }

    #[test]
    fn known_colors() {
        for &(pattern, phase, level, expected) in test_vectors::COLORS {
            assert_eq!(
                color_at(PATTERNS[pattern].keys, phase, level),
                expected,
                "{} @ {phase}",
                PATTERNS[pattern].name
            );
        }
    }

    #[test]
    fn patterns_are_well_formed() {
        for (i, p) in PATTERNS.iter().enumerate() {
            assert!(p.keys.len() >= 2, "{}", p.name);
            assert!(!p.description.is_empty());
            assert!(PATTERNS[i + 1..].iter().all(|q| !q.name.eq_ignore_ascii_case(p.name)), "{} twice", p.name);
            // Names must be accepted by the CLI (no spaces, lowercase) and fit `config` columns.
            assert!(p.name.bytes().all(|b| b.is_ascii_lowercase()) && p.name.len() <= 7, "{}", p.name);
        }
    }

    #[test]
    fn const_and_runtime_lookup_agree() {
        for (i, p) in PATTERNS.iter().enumerate() {
            assert_eq!(find_const(p.name), Some(i));
            assert_eq!(find(p.name), Some(i));
            assert_eq!(find(&p.name.to_uppercase()), Some(i));
            assert_eq!(find_const(&p.name.to_uppercase()), None, "find_const is case-sensitive");
        }
        assert_eq!(find("nope"), None);
        assert_eq!(find_const(""), None);
    }

    #[test]
    fn each_key_is_reached_exactly() {
        for p in &PATTERNS {
            let n = p.keys.len() as f32;
            for (i, key) in p.keys.iter().enumerate() {
                assert_eq!(color_at(p.keys, i as f32 / n, 1.0), gamma(*key), "{} key {i}", p.name);
            }
        }
    }

    #[test]
    fn brightness_zero_is_black_and_scaling_never_brightens() {
        for p in &PATTERNS {
            for step in 0..100 {
                let phase = step as f32 / 100.0;
                assert_eq!(color_at(p.keys, phase, 0.0), RGB8::default());
                let (full, half) = (color_at(p.keys, phase, 1.0), color_at(p.keys, phase, 0.5));
                assert!(half.r <= full.r && half.g <= full.g && half.b <= full.b);
            }
        }
    }

    /// The fade is smooth: no visible jump between two refreshes, including where
    /// the loop wraps (that's why viridis walks its stops forward then back).
    #[test]
    fn fades_have_no_jumps() {
        const STEPS: usize = 2000; // e.g. 50 Hz refresh over a 40 s cycle
        for p in &PATTERNS {
            let mut prev = color_at(p.keys, 0.0, 1.0);
            for step in 1..=STEPS {
                let c = color_at(p.keys, step as f32 / STEPS as f32, 1.0);
                let jump = [c.r.abs_diff(prev.r), c.g.abs_diff(prev.g), c.b.abs_diff(prev.b)];
                assert!(jump.iter().all(|&d| d <= 8), "{}: jump {jump:?} at step {step}", p.name);
                prev = c;
            }
        }
    }
}
