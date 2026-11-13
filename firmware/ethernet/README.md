# The Ethernet app: the workshop node over Gigabit Ethernet

The same node as the UART firmware (`../src`), with the UART replaced by the board's RJ45: static
IP addresses, frames in **UDP datagrams** or in one **TCP connection**, between two
ESP32-S31-Function-CoreBoard-1 boards cabled to each other (no switch, no DHCP).

Everything that isn't the link is shared with the UART app through
[`firmware/shared/core`](../firmware/shared/core): the frames (`DAT`, `EVT`, `ECH`, `BST`), the
sequence and loss tracking, round-trip times, bursts, the CLI, the settings and their validation,
the button debouncing, the LED modes and RGB patterns, the saved-settings format.

## Run it

```sh
cd firmware/ethernet
cargo run --release --bin node-a      # board A: 192.168.77.11
cargo run --release --bin node-b      # board B: 192.168.77.12
```

Cable the two RJ45 ports together (a straight cat5e cable is enough: the PHYs detect the cable
type). Both consoles show `cable connected: 1 Gbit/s · full duplex`, then the frames. `help` lists
the CLI; `link` shows the cable, the negotiated speed, the addresses and the counters. By default the console
is one status line updated in place; `verbose frames` and `verbose wire` print every frame (and the raw frames,
checksums and timing breakdown). `write /leds/1 on` and `read /stats/rx` use the peer's UA-lite address space
([`protocols.md`](../docs/book/protocols.md)).

| Want | Do |
|---|---|
| TCP instead of UDP | `PRESET_NODE_A=node-a-tcp PRESET_NODE_B=node-b-tcp cargo build --release` |
| other addresses, port, names | copy a preset, edit it ([`docs/book/presets.md`](../docs/book/presets.md)) |
| RTT console through the probe | `--features rtt` |
| without saved settings / watchdog | `--no-default-features` (then add what you want) |
| the helper script | `python3 ../scripts/dev.py --app eth flash a` (or `b`, `all`) |

## How it is organised

| Module | What it does |
|---|---|
| [`net.rs`](src/net.rs) | PHY reset, EMAC bring-up, embassy-net with the static address, the live link speed |
| [`link.rs`](src/link.rs) | the UDP and TCP tasks; frames are stamped when they arrive and queued to the main loop |
| [`console.rs`](src/console.rs) | USB console through a pipe: printing never blocks the node or the network |
| [`lib.rs`](src/lib.rs) | the node and its main loop (the UART firmware's, minus the UART) |
| `term.rs`, `cli.rs`, `button.rs`, `ext_leds.rs`, `led.rs`, `fx.rs`, `settings.rs`, `trng.rs`, `rtt_console.rs` | the hardware glue of the UART firmware, ported |
| [`build.rs`](build.rs) | build info for the boot card, and the presets: `../firmware/ethernet/presets/*.toml` → `const NodeConfig` |

```text
 main loop ── Link::send ──▶ TX queue ──▶ UDP / TCP task ──▶ embassy-net ──▶ EMAC ──▶ RJ45
 main loop ◀─ Link::recv ─── RX queue ◀── UDP / TCP task ◀── embassy-net ◀── EMAC ◀── RJ45
 main loop ── print ──▶ pipe ──▶ console task ──▶ USB Serial/JTAG
```

### Pins

The PHY (Motorcomm YT8531, RGMII) owns **GPIO5-19** (MDC 5, MDIO 6, reset 7, RGMII data and clocks
8-19). The workshop pins (button 4, LEDs 20 and 21) don't clash, and the config check refuses 5-19
for anything else. The full table is generated in
[`docs/book/reference/eth-pins.md`](../docs/book/reference/eth-pins.md).

### Why a separate Cargo workspace

The EMAC driver for the S31 is merged upstream (esp-rs/esp-hal #6192) but is **not in a release**
(esp-hal 1.2.x has none), so this crate pins esp-hal to a commit of `main` (see `Cargo.toml`). It also
needs the async stack (`esp-rtos`, `embassy-net`), while the UART firmware stays on released crates and a
blocking loop. Two esp-hal versions can't live in one workspace (`links = "esp-hal"`), so
`firmware/ethernet/` has its own `[workspace]`, `Cargo.lock` and `target/`; it shares `.cargo/config.toml` (target,
runner) and `rust-toolchain.toml` with the repository, and `workshop-core` by path. When a release
ships the driver, switch to crates.io versions and this can join the main workspace.

### Differences from the UART firmware

| | UART | Ethernet |
|---|---|---|
| link settings | `shared::BAUD_RATE`, `pins.uart_*` | `[net]` in the preset: addresses, port, transport, role, max speed |
| link command | `baud [rate]` (speed change handshake, `BAU` frames) | `link` (cable, speed, addresses, counters) |
| speed shown in the status line | baud rate | negotiated Ethernet speed |
| burst share | % of the line (8N1) | % of the link (speed in bytes/s) |
| frame timing | UART interrupt notes when each `\n` was on the wire | the network task stamps each frame when it leaves the socket |
| main loop | blocking, one `loop` | async: the loop and the network tasks share an executor |
| console | `esp-println` (blocking) | a pipe and a task (never blocks the network) |
| settings in flash | `esp-storage` | `esp_hal::flash` (esp-storage is deprecated upstream) |

## What was measured, and what is still open

Two boards cabled directly, UDP, the default presets:

- **Link**: 1 Gbit/s full duplex (the firmware sets the MPLL to 500 MHz: esp-hal's default of 400 MHz caps
  RGMII at 100 Mbit/s, see `net.max_speed`).
- **Periodic traffic** (`DAT` every 2.5 s, `EVT`, `tx on-rx` echoes): 0 frames lost.
- **Bursts**: at a set rate (`burst 5000 1kHz`, `burst 500 100`) nothing is lost. **Back to back**
  (`burst 20000 max`, about 700 kB/s, 0.5 % of the link) 0.2-4 % of the frames are lost: the CPU, not the
  wire, is the limit (receive queue and DMA ring are bounded).
- **Round-trip time** (`tx on-rx`): about **4.4 ms**, against about 0.2 ms on the UART link. Not understood
  yet: the main loop runs about 50 000 times a second and printing no longer blocks, yet received frames
  wait about 0.4 ms for the loop and a sent frame about 1 ms for the network task (`verbose wire` shows
  both). The raw network round trip measured earlier, without the node's loop, was 0.4 ms.
- **TCP** (`node-a-tcp` / `node-b-tcp`): builds and passes the same checks, **not yet run on two boards**.
- Not covered by tests: this crate has no host tests of its own (the shared logic is tested in
  `workshop-core`), and CI doesn't build it yet.
