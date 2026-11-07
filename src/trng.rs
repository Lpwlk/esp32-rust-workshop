//! Starts the ESP32-S31 true random number generator.
//!
//! esp-hal 1.2.2 reads random numbers from `TRNG_CRC_SYNC_DATA` on the S31 but,
//! unlike on the other chips, never starts the TRNG. The ESP-IDF bootloader
//! turns it on for its own use and turns it off again before jumping to the
//! app, so without this every `Rng::random()` returns the same frozen value.
//!
//! This mirrors `trng_ll_enable()` from ESP-IDF v6.1
//! (`components/hal/esp32s31/include/hal/trng_ll.h`). Remove it once esp-hal
//! enables the TRNG itself on the S31.

// --8<-- [start:trng]
/// `LP_PERI_CLKRST.rng_ctrl`: bit 30 = bus clock enable, bit 31 = reset.
const LP_PERI_CLKRST_RNG_CTRL: u32 = 0x2071_0000 + 0x3c;
/// `LP_TRNG.conf`: bit 30 = noise CRC enable, bit 31 = sampling enable.
const TRNG_CONF: u32 = 0x2081_4000;
/// `LP_TRNG.date`: bit 28 = TRNG clock enable.
const TRNG_DATE: u32 = 0x2081_4000 + 0xfc;

fn modify(addr: u32, f: impl FnOnce(u32) -> u32) {
    let reg = addr as *mut u32;
    // SAFETY: fixed MMIO addresses from the ESP32-S31 register map, only touched here.
    unsafe { reg.write_volatile(f(reg.read_volatile())) }
}

pub fn enable() {
    modify(LP_PERI_CLKRST_RNG_CTRL, |v| v | (1 << 30)); // bus clock on
    modify(LP_PERI_CLKRST_RNG_CTRL, |v| v | (1 << 31)); // reset pulse
    modify(LP_PERI_CLKRST_RNG_CTRL, |v| v & !(1 << 31));
    modify(TRNG_DATE, |v| v | (1 << 28)); // TRNG clock on
    modify(TRNG_CONF, |v| v | (1 << 31) | (1 << 30)); // sampling + noise CRC on
}
// --8<-- [end:trng]
