//! What a breadboard LED shows, for each [`LedMode`].
//!
//! ```
//! use workshop_core::config::LedMode;
//! use workshop_core::leds::{led_on, Activity, LedInputs};
//!
//! let inputs = LedInputs { button: false, peer_button: true, link_up: true };
//! let activity = Activity { last_rx_ms: Some(1000), last_tx_ms: None };
//! assert!(led_on(LedMode::PeerButton, &inputs, activity, 1010));
//! assert!(led_on(LedMode::RxFlash, &inputs, activity, 1039)); // 40 ms flash
//! assert!(!led_on(LedMode::RxFlash, &inputs, activity, 1040));
//! ```

use crate::config::LedMode;
use crate::units::ms_to_us;

/// How long an LED stays on for one `rx`/`tx` flash, ms.
pub const FLASH_MS: u64 = 40;

/// Inputs the LED modes can follow, refreshed by the main loop.
#[derive(Clone, Copy, Debug, Default)]
pub struct LedInputs {
    /// This node's button is pressed.
    pub button: bool,
    /// The peer's button is pressed (from its last frame).
    pub peer_button: bool,
    /// Frames from the peer arrive in time.
    pub link_up: bool,
}

/// When the last frames were received and sent, ms since boot.
#[derive(Clone, Copy, Debug, Default)]
pub struct Activity {
    /// Last valid received frame.
    pub last_rx_ms: Option<u64>,
    /// Last sent frame.
    pub last_tx_ms: Option<u64>,
}

/// Whether an LED in `mode` is lit at `now_ms`.
pub fn led_on(mode: LedMode, inputs: &LedInputs, activity: Activity, now_ms: u64) -> bool {
    let flashing = |t: Option<u64>| t.is_some_and(|t| now_ms.saturating_sub(t) < FLASH_MS);
    match mode {
        LedMode::Off => false,
        LedMode::On => true,
        LedMode::Blink { period_ms } => {
            // In µs, so decimal periods (e.g. 2.5 Hz = 400 ms, 3 Hz = 333.333 ms) stay exact enough.
            let period_us = ms_to_us(period_ms).max(2);
            (now_ms * 1000) % period_us < period_us / 2
        }
        LedMode::Button => inputs.button,
        LedMode::PeerButton => inputs.peer_button,
        LedMode::Link => inputs.link_up,
        LedMode::RxFlash => flashing(activity.last_rx_ms),
        LedMode::TxFlash => flashing(activity.last_tx_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: LedInputs = LedInputs { button: false, peer_button: false, link_up: false };
    const NONE: Activity = Activity { last_rx_ms: None, last_tx_ms: None };

    #[test]
    fn steady_modes() {
        let all = LedInputs { button: true, peer_button: true, link_up: true };
        for t in [0, 1, 999_999] {
            assert!(!led_on(LedMode::Off, &all, NONE, t));
            assert!(led_on(LedMode::On, &IDLE, NONE, t));
        }
        for (mode, inputs) in [
            (LedMode::Button, LedInputs { button: true, ..IDLE }),
            (LedMode::PeerButton, LedInputs { peer_button: true, ..IDLE }),
            (LedMode::Link, LedInputs { link_up: true, ..IDLE }),
        ] {
            assert!(led_on(mode, &inputs, NONE, 0), "{mode:?}");
            assert!(!led_on(mode, &IDLE, NONE, 0), "{mode:?}");
        }
    }

    #[test]
    fn blink_has_a_50_percent_duty_cycle() {
        for period in [20u32, 500, 1001, 60_000] {
            let mode = LedMode::Blink { period_ms: period as f32 };
            let lit = (0..u64::from(period) * 3).filter(|&t| led_on(mode, &IDLE, NONE, t)).count() as u64;
            assert_eq!(lit, 3 * u64::from(period).div_ceil(2), "period {period}");
            assert!(led_on(mode, &IDLE, NONE, 0), "starts lit");
        }
    }

    #[test]
    fn degenerate_blink_periods_dont_panic() {
        for period_ms in [0.0, 1.0, 0.0001] {
            let _ = led_on(LedMode::Blink { period_ms }, &IDLE, NONE, 12_345);
        }
    }

    #[test]
    fn flashes_last_40_ms() {
        let act = Activity { last_rx_ms: Some(100), last_tx_ms: Some(500) };
        let lit = |mode, t| led_on(mode, &IDLE, act, t);
        assert!(lit(LedMode::RxFlash, 100) && lit(LedMode::RxFlash, 139) && !lit(LedMode::RxFlash, 140));
        assert!(lit(LedMode::TxFlash, 539) && !lit(LedMode::TxFlash, 540));
        assert!(!led_on(LedMode::RxFlash, &IDLE, NONE, 0), "no frame yet");
    }
}
