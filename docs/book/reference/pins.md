<!-- Generated from the source by `workshop_core::docgen`. Do not edit: run `UPDATE_DOCS=1 cargo test-host`. -->

# Pins

## Default assignment

| Function | GPIO |
|---|---|
| UART1 TX | 2 |
| UART1 RX | 3 |
| button | 4 |
| LED 1 | 20 |
| LED 2 | 21 |
| RGB LED (fixed) | 60 |

## Refused by the config check

GPIO numbers go from 0 to 61.

| GPIO | Why |
|---|---|
| 26 | SPI0/1: in-package flash |
| 27 | SPI0/1: in-package flash |
| 28 | SPI0/1: in-package flash |
| 30 | SPI0/1: in-package flash |
| 31 | SPI0/1: in-package flash |
| 32 | SPI0/1: in-package flash |
| 33 | USB Serial/JTAG D- (CLI, espflash, probe-rs) |
| 34 | USB Serial/JTAG D+ (CLI, espflash, probe-rs) |
| 58 | UART0 TX: ROM / bootloader console |
| 59 | UART0 RX: ROM / bootloader console |
| 60 | on-board RGB LED (strapping pin) |
| 29 | does not exist on the ESP32-S31 |
| 41 | does not exist on the ESP32-S31 |
