//! RGB LED animation: renders the patterns of [`workshop_core::fx`] over time.
//!
//! The palettes and the color math are in `workshop-core` (re-exported here), where
//! they are unit-tested on the host; this module only adds the timing.

use esp_hal::time::{Duration, Instant};
pub use workshop_core::fx::*;

use crate::led::Led;

pub struct Animator {
    pub settings: Settings,
    /// Last color actually sent to the LED (after brightness and gamma).
    pub last: RGB8,
    start: Instant,
    last_frame: Instant,
}

impl Animator {
    pub fn new(settings: Settings) -> Self {
        Self { settings, last: RGB8 { r: 0, g: 0, b: 0 }, start: Instant::now(), last_frame: Instant::now() }
    }

    /// Renders a new frame when one is due (every `1 / freq_hz`).
    pub fn tick(&mut self, led: &mut Led) {
        let frame = Duration::from_micros((1e6 / self.settings.freq_hz.max(1.0)) as u64);
        if self.last_frame.elapsed() < frame {
            return;
        }
        self.last_frame = Instant::now();

        let Settings { cycle_ms, brightness, .. } = self.settings;
        let pattern = self.settings.pattern();
        // In µs, so decimal cycles (e.g. 2.5 s, 333.3 ms) are exact enough.
        let cycle_us = workshop_core::units::ms_to_us(cycle_ms).max(1);
        let phase = (self.start.elapsed().as_micros() % cycle_us) as f32 / cycle_us as f32;

        let level = f32::from(brightness.min(100)) / 100.0;
        self.last = color_at(pattern.keys, phase, level);
        led.set(self.last);
    }
}
