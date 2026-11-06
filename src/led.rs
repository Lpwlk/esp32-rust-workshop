//! On-board addressable RGB LED (WS2812-type, GPIO60), driven by the RMT peripheral.
//!

use esp_hal::{Blocking, gpio::interconnect::PeripheralOutput, peripherals::RMT, rmt::Rmt, time::Rate};
use esp_hal_smartled::{RmtSmartLeds, WS2812_TIMING, buffer_size, color_order};
pub use smart_leds_trait::RGB8;
use smart_leds_trait::SmartLedsWrite;

const RMT_FREQ: Rate = Rate::from_mhz(80);

type Driver = RmtSmartLeds<'static, { buffer_size::<RGB8>(1) }, Blocking, RGB8, color_order::Grb>;

pub struct Led {
    driver: Driver,
}

impl Led {
    pub fn new(rmt: RMT<'static>, pin: impl PeripheralOutput<'static>) -> Self {
        let rmt = Rmt::new(rmt, RMT_FREQ).expect("RMT init failed");
        let driver =
            Driver::new_with_memsize(WS2812_TIMING, rmt.channel0, pin, 2, RMT_FREQ).expect("RGB LED init failed");
        Self { driver }
    }

    pub fn set(&mut self, color: RGB8) {
        // A failed LED update is cosmetic: don't stop the UART link for it.
        let _ = self.driver.write([color]);
    }
}
