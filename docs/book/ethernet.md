# Ethernet app

The same two nodes as the [UART firmware](firmware.md), talking over the board's **RJ45** instead of
three wires: Gigabit Ethernet through the on-board YT8531 PHY, static IP addresses, and frames in
**UDP datagrams** or in one **TCP connection**. It lives in `firmware/ethernet/`.

Everything that isn't the link is the same code, shared through `workshop-core`: the frame
protocol (`DAT`, `EVT`, `ECH`, `BST`), lost-frame detection, round-trip times, bursts, the CLI, the
settings and their validation, the saved settings, the button debouncing, the LED modes and the RGB
patterns.

## Two apps, two Cargo workspaces

```text
esp32-rust-workshop/
├── Cargo.toml                   workspace root: uart + the two shared crates (no package of its own)
└── firmware/
    ├── uart/                    the UART app            presets/ = its nodes' settings (TOML)
    ├── ethernet/                the Ethernet app: its own Cargo workspace (esp-hal main, embassy)
    │                            presets/ = its nodes' settings (TOML)
    └── shared/
        ├── core/                no_std, host-tested: protocol, config (Link::Uart | Link::Net), CLI, ...
        └── presets-tool/        host only: reads and validates the TOML presets (`cargo presets`)
```

| | UART app | Ethernet app |
|---|---|---|
| directory | `firmware/uart/` (workspace root) | `firmware/ethernet/` (own workspace) |
| run | `cargo run --release --bin node-a` (from the root or `firmware/uart/`) | `cd firmware/ethernet && cargo run --release --bin node-a` |
| presets | `firmware/uart/presets/` | `firmware/ethernet/presets/` |
| esp-hal | 1.2.2 (crates.io) | `main`, pinned to a commit |
| main loop | blocking | async (esp-rtos + embassy-net) |
| helper script | `dev.py flash a` | `dev.py --app eth flash a` |

!!! info "Why not one workspace"
    The EMAC driver for the ESP32-S31 is merged in esp-hal (PR #6192) but isn't in a release yet, and it
    needs the async stack. Two esp-hal versions can't live in one Cargo workspace (`links = "esp-hal"`),
    so the Ethernet app has its own `[workspace]`, `Cargo.lock` and `target/`. Both share
    `.cargo/config.toml` and `workshop-core`. When a release ships the driver, the two can merge.

## Wiring and addresses

Cable the two RJ45 ports together: a straight cat5e cable is enough, the PHYs detect the cable type,
and **no switch and no DHCP are needed**. The default addresses are in the presets:

| Node | Address | Peer |
|---|---|---|
| node-A | `192.168.77.11/24` | `192.168.77.12` |
| node-B | `192.168.77.12/24` | `192.168.77.11` |

The breadboard LEDs and the jumper button are wired as for the UART app (GPIO20, GPIO21, GPIO4). The PHY
owns **GPIO5-19** (MDC, MDIO, reset, RGMII): the config check refuses them for anything else, see
the [Ethernet pins](reference/eth-pins.md).

## Running it

```sh
cd firmware/ethernet
cargo run --release --bin node-a      # then, on the other board: --bin node-b
```

The console shows `cable connected: 1 Gbit/s · full duplex`, then, by default, **one status line updated in
place** (`verbose live`): the elapsed time, the frame counters, the link, the round trip and the last event. `verbose
frames` prints every frame, `verbose wire` adds the raw frames, checksums and the timing breakdown (see the
[console](console.md#how-much-the-console-prints-verbose)). In the CLI, `link` replaces `baud`: the cable, what the PHY negotiated, the
addresses, the transport, the counters.
TCP instead of UDP is a preset: `PRESET_NODE_A=node-a-tcp PRESET_NODE_B=node-b-tcp cargo build --release`.

## How it works

```text
 main loop ── send ──▶ TX queue ──▶ UDP / TCP task ──▶ embassy-net ──▶ EMAC ──▶ RJ45
 main loop ◀─ recv ─── RX queue ◀── UDP / TCP task ◀── embassy-net ◀── EMAC ◀── RJ45
 main loop ── print ──▶ pipe ──▶ console task ──▶ USB Serial/JTAG
```

- **1 Gbit/s needs the MPLL at 500 MHz.** The RGMII transmit clock comes from the MPLL through an integer
  divider; esp-hal's default of 400 MHz (made for 200 MHz PSRAM) caps Ethernet at 100 Mbit/s, with a
  `log` warning that is easy to miss. The firmware sets 500 MHz (`net.max_speed = 1000`).
- **Received frames are stamped by the network task** when they leave the socket, so the `Δ` and `rtt`
  values don't include the main loop's own delays.
- **Printing never blocks.** A frame line takes about 3 ms to reach the USB console (64 bytes per
  millisecond), and the main loop shares an executor with the network tasks, so output goes into a pipe
  that a task drains. Without it the packets being described were held back by their own log lines.
- **`baud` and `BAU` frames don't exist here.** An Ethernet node ignores a `BAU` frame.
- **The peer's LEDs, buttons and settings are an address space.** The same link also carries
  [UA-lite](protocols.md) requests (`BRW`, `RED`, `WRT`, `SUB`): `write /leds/1 on` lights the other board's LED,
  `read /stats/rx`, `subscribe /buttons/jumper`. It is the same code on both apps (`workshop-core::space`).

## What was measured

Two boards cabled directly, default presets (UDP), and the TCP presets:

| | UDP | TCP |
|---|---|---|
| link | 1 Gbit/s full duplex | 1 Gbit/s full duplex |
| periodic frames | none lost | none lost, connected a few seconds after boot |
| `burst 2000 max`, `burst 20000 max` | 0 lost | 0 lost |
| `burst 5000 1kHz` | 0 lost | not run |
| `tx on-rx` round trip | about 4 ms | about 5 ms |

The back-to-back burst reaches about 365 kB/s (0.3 % of the link): the node's frame handling, not the
wire, is the limit. (UA-lite and the live line were written after these measurements and are tested on the
host, not yet measured on two boards.) The **round-trip time is an open problem**: it is about 20 times the UART link's
and the raw network path measured on its own was 0.4 ms. The main loop runs about 50 000 times a second
and printing no longer blocks, yet received frames wait about 0.4 ms for the loop and a sent frame about
1 ms for the network task (`verbose wire` shows both, with the slowest main-loop iteration and the task pickup
delay); the cause isn't found yet. The leading suspect is one slow step of the main loop (probably the RGB LED
write) delaying the single cooperative executor, to be confirmed by timing it.

## Reference

[CLI](reference/eth-cli.md) · [Settings](reference/eth-config.md) · [Pins](reference/eth-pins.md) ·
[Protocol](reference/eth-protocol.md): generated from the code, like the UART ones.
