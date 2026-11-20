//! ADC streaming mode: samples GPIO43 and sends the samples over UDP (see `stream` in the library
//! and `tools/adc-fft/adc_fft.py` for the spectrum on the computer).
#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]

use esp_backtrace as _;

// App descriptor required by the ESP-IDF second-stage bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) {
    esp32_rust_workshop_adc::stream::run(spawner).await
}
