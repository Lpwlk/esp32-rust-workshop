//! Two LEDs on the breadboard, each driven by a GPIO output (active high).
//!
//! Wiring per LED: GPIO -> 330 Ω resistor -> LED anode (long leg), cathode -> GND.
//! At 3.3 V that's ~4 mA for a red LED, well within the GPIO limits.

use esp_hal::{
    gpio::{AnyPin, Level, Output, OutputConfig},
    time::{Duration, Instant},
};

use crate::config::LedMode;

/// How long an LED stays on for one `rx`/`tx` flash.
const FLASH: Duration = Duration::from_millis(40);

/// Inputs the LED modes can follow, refreshed by the main loop.
pub struct LedInputs {
    pub button: bool,
    pub peer_button: bool,
    pub link_up: bool,
}

pub struct ExtLeds {
    outputs: [Output<'static>; 2],
    pub modes: [LedMode; 2],
    pub pins: [u8; 2],
    last_rx: Option<Instant>,
    last_tx: Option<Instant>,
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
            last_rx: None,
            last_tx: None,
        }
    }

    pub fn on_rx(&mut self) {
        self.last_rx = Some(Instant::now());
    }

    pub fn on_tx(&mut self) {
        self.last_tx = Some(Instant::now());
    }

    /// Recomputes both LEDs; call every loop iteration (cheap: two register writes).
    pub fn update(&mut self, inputs: &LedInputs) {
        let now_ms = Instant::now().duration_since_epoch().as_millis();
        let flashing = |t: Option<Instant>| t.is_some_and(|t| t.elapsed() < FLASH);
        for (out, mode) in self.outputs.iter_mut().zip(self.modes) {
            let on = match mode {
                LedMode::Off => false,
                LedMode::On => true,
                LedMode::Blink { period_ms } => now_ms % u64::from(period_ms.max(2)) < u64::from(period_ms / 2),
                LedMode::Button => inputs.button,
                LedMode::PeerButton => inputs.peer_button,
                LedMode::Link => inputs.link_up,
                LedMode::RxFlash => flashing(self.last_rx),
                LedMode::TxFlash => flashing(self.last_tx),
            };
            out.set_level(Level::from(on));
        }
    }
}
