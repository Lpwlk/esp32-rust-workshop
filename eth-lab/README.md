# eth-lab: the CoreBoard's RJ45 in Rust

Gigabit Ethernet on the ESP32-S31-Function-CoreBoard-1: **static addresses, no DHCP**, a
status page on port 80, the two breadboard LEDs and the jumper button. Standalone crate (own
workspace); the node firmware is untouched.

## Network plan

Subnet `192.168.77.0/24`, no gateway, all boards and your computer on the same hub/switch:

| Who | Address |
|---|---|
| your computer | `192.168.77.1` |
| board `1c:29:04:d0:6c:1a` (USB `…6C:1A`) | `192.168.77.11` |
| board `1c:29:04:d0:21:fe` | `192.168.77.12` |
| any other board | `192.168.77.20`, or `ETH_LAB_HOST=13 cargo run --release` |

The table is `BOARDS` in `src/main.rs`, keyed by the chip's base MAC (`espflash board-info`),
so a plain `cargo run --release` is right on either board.

Give the computer an address on the interface that faces the hub (here `en10`):

```sh
sudo ifconfig en10 alias 192.168.77.1/24          # temporary, gone at unplug/reboot
# or permanently, for the "USB 10/100/1000 LAN" service:
sudo networksetup -setmanual "USB 10/100/1000 LAN" 192.168.77.1 255.255.255.0
```

```sh
cd eth-lab
cargo run --release          # builds, flashes with espflash, opens the monitor
ping 192.168.77.11
curl http://192.168.77.11/
```

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
