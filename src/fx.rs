//! Color fade patterns for the on-board RGB LED.
//!
//! A pattern is a loop of key colors. The LED moves from one key to the next with
//! a smoothstep ease (no visible "corner" at each key), then gamma correction is
//! applied so the fade looks even to the eye (LEDs are linear, eyes are not).

use esp_hal::time::{Duration, Instant};

use crate::led::{Led, RGB8};

const fn rgb(r: u8, g: u8, b: u8) -> RGB8 {
    RGB8 { r, g, b }
}

pub struct Pattern {
    pub name: &'static str,
    pub description: &'static str,
    /// Key colors, in perceptual space (before gamma). The loop wraps to the first one.
    pub keys: &'static [RGB8],
}

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

pub fn find(name: &str) -> Option<usize> {
    PATTERNS.iter().position(|p| p.name.eq_ignore_ascii_case(name))
}

pub const FREQ_RANGE: (u32, u32) = (1, 200);
pub const CYCLE_RANGE: (u32, u32) = (200, 60_000);

/// Runtime-adjustable settings (changed from the CLI).
#[derive(Clone, Copy)]
pub struct Settings {
    pub pattern: usize,
    /// LED refresh rate, in Hz.
    pub freq_hz: u32,
    /// Duration of one full loop through the pattern, in ms.
    pub cycle_ms: u32,
    /// 0-100 %.
    pub brightness: u8,
}

impl Settings {
    pub fn pattern(&self) -> &'static Pattern {
        &PATTERNS[self.pattern]
    }
}

pub struct Animator {
    pub settings: Settings,
    start: Instant,
    last_frame: Instant,
}

impl Animator {
    pub fn new(settings: Settings) -> Self {
        Self { settings, start: Instant::now(), last_frame: Instant::now() }
    }

    /// Renders a new frame when one is due (every `1 / freq_hz`).
    pub fn tick(&mut self, led: &mut Led) {
        let frame = Duration::from_micros(1_000_000 / u64::from(self.settings.freq_hz.max(1)));
        if self.last_frame.elapsed() < frame {
            return;
        }
        self.last_frame = Instant::now();

        let Settings { cycle_ms, brightness, .. } = self.settings;
        let pattern = self.settings.pattern();
        let t = self.start.elapsed().as_millis() % u64::from(cycle_ms.max(1));
        let phase = t as f32 / cycle_ms as f32;

        let level = f32::from(brightness.min(100)) / 100.0;
        led.set(color_at(pattern.keys, phase, level));
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
