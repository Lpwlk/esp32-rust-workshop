//! On-target tests: `cargo test-target` builds this file, flashes it with probe-rs
//! through the USB Serial/JTAG port and reports each test (embedded-test, over
//! semihosting). The board is reset between tests. No wiring needed.
//!
//! Two kinds of tests:
//! - the hardware-independent logic gives on the ESP32-S31 (RISC-V, hardware f32)
//!   exactly the results the host tests check (`workshop_core::test_vectors`);
//! - the hardware behaves as the firmware assumes (TRNG, UART at 5 Mbaud, GPIO).
#![no_std]
#![no_main]

esp_bootloader_esp_idf::esp_app_desc!();

#[cfg(test)]
#[embedded_test::tests]
mod tests {
    use esp_hal::{
        Config,
        gpio::{AnyPin, Flex, Level},
        peripherals::Peripherals,
        rng::Rng,
        time::{Duration, Instant},
    };
    use esp32_rust_workshop::{config::shared, link::Link, trng};
    use workshop_core::{
        config::NodeConfig,
        fx::{PATTERNS, color_at},
        line::{LineBuffer, LineEvent},
        protocol::{self, MAX_FRAME},
        test_vectors::{COLORS, FRAMES},
    };

    fn wait(ms: u64) {
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(ms) {}
    }

    #[init]
    fn init() -> Peripherals {
        esp_hal::init(Config::default())
    }

    // --- Same results as on the host ------------------------------------------

    #[test]
    fn frames_match_the_host_vectors() {
        for (frame, line) in FRAMES {
            assert_eq!(protocol::encode(frame).as_str(), *line);
            assert_eq!(protocol::parse(line.trim_end()), Ok(*frame));
        }
    }

    /// The RGB fade uses f32: the RISC-V FPU must round exactly like the host.
    #[test]
    fn colors_match_the_host_vectors() {
        assert!(!COLORS.is_empty());
        for &(pattern, phase, level, expected) in COLORS {
            assert_eq!(color_at(PATTERNS[pattern].keys, phase, level), expected);
        }
    }

    #[test]
    fn default_config_is_valid() {
        assert_eq!(NodeConfig::DEFAULT.validate(), Ok(()));
    }

    // --- Hardware -------------------------------------------------------------

    /// esp-hal 1.2.2 never starts the TRNG on the S31 (frozen value, identical
    /// keys): `trng::enable()` must fix it.
    #[test]
    fn trng_is_not_frozen_once_enabled(_p: Peripherals) {
        trng::enable();
        let rng = Rng::new();
        let mut samples = [0u32; 16];
        for s in &mut samples {
            *s = rng.random();
        }
        let distinct = (0..samples.len()).filter(|&i| !samples[..i].contains(&samples[i])).count();
        assert!(distinct >= 15, "only {} distinct values out of 16", distinct);
    }

    /// A frame sent at 5 Mbaud comes back through the real receive path (UART
    /// interrupt + ring buffer), using the UART's internal loopback (no wire).
    #[test]
    fn uart_loopback_at_5_mbaud(p: Peripherals) {
        // SAFETY: test-only, these pins aren't used by anything else here.
        let (tx, rx) = unsafe { (AnyPin::steal(2), AnyPin::steal(3)) };
        let mut link = Link::new(p.UART1, tx, rx, shared::BAUD_RATE);
        enable_uart1_loopback();

        let (frame, line) = FRAMES[2];
        link.send(protocol::encode(&frame).as_bytes()).unwrap();
        wait(5);

        let mut received = [0u8; MAX_FRAME];
        let n = link.read(&mut received);
        assert!(link.take_error().is_none());
        assert_eq!(&received[..n], line.as_bytes());

        let mut lines = LineBuffer::<MAX_FRAME>::new();
        let mut decoded = 0;
        for &b in &received[..n] {
            if let Some(LineEvent::Line(l)) = lines.push(b) {
                assert_eq!(protocol::parse(core::str::from_utf8(l).unwrap()), Ok(frame));
                decoded += 1;
            }
        }
        assert_eq!(decoded, 1);
    }

    /// The default LED and button pins can be driven and read back at the pad.
    #[test]
    fn default_gpios_drive_and_read_back(_p: Peripherals) {
        let pins = NodeConfig::DEFAULT.pins;
        for n in [pins.led1, pins.led2, pins.button] {
            // SAFETY: test-only, one pin at a time.
            let mut pin = Flex::new(unsafe { AnyPin::steal(n) });
            pin.set_input_enable(true);
            pin.set_output_enable(true);
            for level in [Level::High, Level::Low, Level::High] {
                pin.set_level(level);
                wait(1);
                assert_eq!(pin.level(), level, "GPIO{}", n);
            }
            pin.set_output_enable(false);
        }
    }

    /// UART1 internal loopback (TX -> RX inside the peripheral), from the
    /// ESP32-S31 register map (ESP-IDF v6.1 `uart_reg.h`): `CONF0_SYNC.LOOPBACK`
    /// (bit 12), then `REG_UPDATE` to sync it to the UART clock domain.
    fn enable_uart1_loopback() {
        const UART1: usize = 0x2038_B000;
        // SAFETY: fixed MMIO registers of UART1, owned by this test's Link.
        unsafe {
            let conf0 = (UART1 + 0x20) as *mut u32;
            conf0.write_volatile(conf0.read_volatile() | (1 << 12));
            ((UART1 + 0x98) as *mut u32).write_volatile(1);
        }
        wait(1);
    }
}
