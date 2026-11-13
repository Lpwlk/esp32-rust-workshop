<!-- Generated from the source by `workshop_core::docgen`. Do not edit: run `UPDATE_DOCS=1 cargo test-host`. -->

# Pins (Ethernet app)

## Default assignment

| Function | GPIO |
|---|---|
| button | 4 |
| LED 1 | 20 |
| LED 2 | 21 |
| RGB LED (fixed) | 60 |

## Used by the Ethernet PHY

Wired on the board, not configurable. Nothing else may use them:

| GPIO | Signal |
|---|---|
| 5 | MDC (PHY management clock) |
| 6 | MDIO (PHY management data) |
| 7 | PHY reset |
| 8 | RGMII TXD0 |
| 9 | RGMII TXD1 |
| 10 | RGMII TXD2 |
| 11 | RGMII TXD3 |
| 12 | RGMII TX_CTL |
| 13 | RGMII TX_CLK |
| 14 | RGMII RX_CLK |
| 15 | RGMII RX_CTL |
| 16 | RGMII RXD3 |
| 17 | RGMII RXD2 |
| 18 | RGMII RXD1 |
| 19 | RGMII RXD0 |

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
| 50 | on-board audio codec and amplifier (not on the header) |
| 51 | on-board audio codec and amplifier (not on the header) |
| 52 | on-board audio codec and amplifier (not on the header) |
| 53 | on-board audio codec and amplifier (not on the header) |
| 54 | on-board audio codec and amplifier (not on the header) |
| 55 | on-board audio codec and amplifier (not on the header) |
| 56 | on-board audio codec and amplifier (not on the header) |
| 57 | on-board audio codec and amplifier (not on the header) |
| 58 | UART0 TX: ROM / bootloader console |
| 59 | UART0 RX: ROM / bootloader console |
| 60 | on-board RGB LED (strapping pin) |
| 61 | BOOT button, read by the firmware like the jumper (strapping pin) |
| 29 | does not exist on the ESP32-S31 |
| 41 | does not exist on the ESP32-S31 |
| 5-19 | used by the Ethernet PHY (see above) |
