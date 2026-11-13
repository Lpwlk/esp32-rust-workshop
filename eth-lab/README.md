# eth-lab: the CoreBoard's RJ45 in Rust

Gigabit Ethernet on the ESP32-S31-Function-CoreBoard-1: **two boards cabled directly to each
other, static addresses, no DHCP, no switch**. They ping each other over UDP and measure TCP
throughput; each also serves a status page on port 80. Standalone crate (own workspace); the node
firmware is untouched.

## Two boards, one cable

Plug a cable from the RJ45 of one board to the RJ45 of the other (the PHYs detect the cable
type, no crossover needed). Same firmware on both; the role comes from the chip's base MAC:

| Board (base MAC) | Address | Role |
|---|---|---|
| `1c:29:04:d0:6c:1a` | `192.168.77.11/24` | initiator: 20 UDP pings (port 7777), then 2 s of TCP (port 7779), repeat |
| `1c:29:04:d0:21:fe` | `192.168.77.12/24` | responder: UDP echo, TCP sink |
| any other board | `192.168.77.20/24` (or `ETH_LAB_HOST=13 cargo run --release`) | responder |

The table is `BOARDS` in `src/main.rs` (`espflash board-info` prints the base MAC). Console of the
initiator:

```text
udp ping 192.168.77.12: 20/20 answered, rtt min/avg/max = 358/407/901 us
tcp  192.168.77.12: 13122 kB in 2011 ms = 52.19 Mbit/s
```

Measured with a direct cable at **100 Mbit/s** (see the next section for gigabit): UDP round trip
about 0.4 ms, TCP about 52 Mbit/s (limited by the CPU and a 8 kB window, not by the wire).

```sh
cd eth-lab
cargo run --release          # builds, flashes with espflash, opens the monitor
```

## Gigabit needs the MPLL at 500 MHz

The EMAC drives the RGMII TX clock from the MPLL through an integer divider. esp-hal's default
MPLL is 400 MHz (what 200 MHz PSRAM wants), which caps Ethernet at **100 Mbit/s**: the HAL says so
with a `log` warning, which is invisible without a logger. `main` sets `MpllClkConfig::_500` for
1000 Mbit/s. (Not used here: PSRAM, which needs the 400 MHz setting.) The link then needs a cat5e
or better cable, with all four pairs. The negotiated speed is printed at boot
(`Link up: 1000 Mbit/s, Full duplex`).

## A computer on the same cable (optional)

For the status page, give the computer an address in the subnet, on the interface that faces the
board, e.g. `sudo ifconfig en10 alias 192.168.77.1/24`, then `curl http://192.168.77.11/`.

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
