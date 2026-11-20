//! ADC streaming firmware for the ESP32-S31-Function-CoreBoard-1: one board samples an analog pin and
//! streams the samples over Ethernet (UDP) to a computer, which draws the spectrum
//! (`tools/adc-fft/adc_fft.py`).
//!
//! - [`dma`]: the ADC's continuous mode with DMA (611 Hz to 83.3 kHz, hardware-timed);
//! - [`stream`]: the UDP protocol (`start` / `stop` from the computer, sample datagrams back) and the main loop;
//! - [`board_log`]: the board's log, in colour on the USB console and sent to the computer's page.
//!
//! The Ethernet bring-up (PHY, EMAC, network stack) is `esp32_rust_workshop_eth::net`, from `firmware/ethernet`.
#![no_std]

pub mod board_log;
pub mod dma;
pub mod stream;
