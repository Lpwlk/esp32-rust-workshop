#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]

use esp_backtrace as _;
/// RGB LED pattern at boot: aurora, sunset, ocean, ember or sakura.
/// It can be changed at runtime from the CLI (`list`, `pattern <name>`).
const LED_PATTERN: &str = "sunset";

// App descriptor required by the ESP-IDF second-stage bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    esp32_rust_workshop::run("node-B", LED_PATTERN)
}
