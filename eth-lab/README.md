# eth-lab: the CoreBoard's RJ45 in Rust

Gigabit Ethernet on the ESP32-S31-Function-CoreBoard-1: DHCP, a status page on port 80,
the two breadboard LEDs and the jumper button. Standalone crate (own workspace); the node
firmware is untouched.

```sh
cd eth-lab
cargo run --release          # builds, flashes with espflash, opens the monitor
curl http://<ip printed on the console>/
```

Plug a cable into the RJ45 first (router, switch, or straight to your computer/another
board: with no DHCP answer after 15 s the board takes a `169.254.x.y/16` address).

## Why a separate crate

The S31 EMAC driver is merged in esp-hal (PR #6192, 2026-09-04) but **not in any release**:
esp-hal 1.2.x has the `ethernet` module for the ESP32 and ESP32-P4 only. So the `esp-*` crates
are pinned to one upstream commit (`rev` in `Cargo.toml`). It also needs the async stack
(`esp-rtos` + `embassy-net`), unlike the blocking node firmware. When a release ships it,
switch to crates.io versions.

## Board wiring (fixed by the PCB)

| Signal | GPIO | | Signal | GPIO |
|---|---|---|---|---|
| MDC | 5 | | RXD0..3 | 19, 18, 17, 16 |
| MDIO | 6 | | RX_CTL | 15 |
| PHY reset | 7 | | RX_CLK | 14 |
| TXD0..3 | 8, 9, 10, 11 | | TX_CLK | 13 |
| TX_CTL | 12 | | | |

PHY: Motorcomm YT8531 (found through MDIO, `GenericPhy::new_auto`). **GPIO5-19 are
unavailable for anything else** while Ethernet is used. The workshop pins (UART 2/3, button 4,
LEDs 20/21) do not clash.

## What it does

* LED 1 (GPIO20): link up. LED 2 (GPIO21): blinks on a request or a button edge.
* The link speed and duplex are read from the PHY at boot (gigabit needs a gigabit peer
  and a cat5e cable). embassy-net does not expose them, so the page shows the value at boot.
* MAC = chip base MAC + 3 (as ESP-IDF), so several boards on one LAN do not collide.
