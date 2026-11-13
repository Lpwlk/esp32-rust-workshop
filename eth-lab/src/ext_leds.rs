//! Two LEDs on the breadboard, each driven by a GPIO output (active high).
//!
//! Wiring per LED: GPIO -> 330 Ω resistor -> LED anode (long leg), cathode -> GND.
//! At 3.3 V that's ~4 mA for a red LED, well within the GPIO limits.
//!
//! What each mode shows is decided by [`workshop_core::leds::led_on`] (tested on
//! the host); this module only drives the pins.

use esp_hal::{
    gpio::{AnyPin, Level, Output, OutputConfig},
    time::Instant,
};
pub use workshop_core::leds::LedInputs;
use workshop_core::{
    config::LedMode,
    leds::{Activity, led_on},
};

fn now_ms() -> u64 {
    Instant::now().duration_since_epoch().as_millis()
}

pub struct ExtLeds {
    outputs: [Output<'static>; 2],
    pub modes: [LedMode; 2],
    pub pins: [u8; 2],
    activity: Activity,
}

impl ExtLeds {
    pub fn new(pins: [(AnyPin<'static>, u8); 2], modes: [LedMode; 2]) -> Self {
        let [(p1, n1), (p2, n2)] = pins;
        Self {
            outputs: [
                Output::new(p1, Level::Low, OutputConfig::default()),
                Output::new(p2, Level::Low, OutputConfig::default()),
            ],
            modes,
            pins: [n1, n2],
            activity: Activity::default(),
        }
    }

    pub fn on_rx(&mut self) {
        self.activity.last_rx_ms = Some(now_ms());
    }

    pub fn on_tx(&mut self) {
        self.activity.last_tx_ms = Some(now_ms());
    }

    /// Recomputes both LEDs; call every loop iteration (cheap: two register writes).
    pub fn update(&mut self, inputs: &LedInputs) {
        let now = now_ms();
        for (out, mode) in self.outputs.iter_mut().zip(self.modes) {
            out.set_level(Level::from(led_on(mode, inputs, self.activity, now)));
        }
    }
}
