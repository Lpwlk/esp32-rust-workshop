<div align="center">

# ESP32-S31 UART workshop

**Two ESP32-S31 boards talking over UART at 5 Mbaud, in `no_std` Rust.**

[![CI](https://github.com/Lpwlk/esp32-rust-workshop/actions/workflows/ci.yml/badge.svg)](https://github.com/Lpwlk/esp32-rust-workshop/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-lpwlk.github.io-4D9DD1)](https://lpwlk.github.io/esp32-rust-workshop/)
[![Release](https://img.shields.io/github/v/release/Lpwlk/esp32-rust-workshop?include_prereleases&label=firmware)](https://github.com/Lpwlk/esp32-rust-workshop/releases)
![Rust](https://img.shields.io/badge/rust-stable%201.95%2B-orange)
![esp-hal](https://img.shields.io/badge/esp--hal-1.2-blue)

![Boot of node A: the boot card, then frames exchanged with node B](docs/book/assets/demo/boot.gif)

</div>

Each board sends periodic `DAT` frames and an `EVT` frame with a random 64-bit key on
every button edge, decodes everything the other one sends, and shows it all on a
colored console with a real CLI. Built on [esp-hal](https://github.com/esp-rs/esp-hal)
1.2, for the ESP32-S31-Function-CoreBoard-1.

## Highlights

|   |   |
|---|---|
| **UART at 5 Mbaud** | interrupt-driven RX into a ring buffer, XOR-checksummed text frames, lost-frame detection, µs-accurate timing measured on the wire |
| **Link tests** | change the speed on both nodes at once (`baud 1M`, automatic fallback), round-trip times (`tx on-rx`), bursts with throughput |
| **A real CLI** | history, in-line editing, `help <cmd>`, periods or frequencies (`period 2.5Hz`), over USB **or RTT** |
| **Settings** | checked **at compile time** (`const fn`), changed live with `set`, **saved to flash** with `save` |
| **Embedded hygiene** | hardware watchdog, reset reason, Cargo features (`persist`, `watchdog`, `rtt`), Flash/RAM report after each build |
| **Tested on the host** | the hardware-free logic is its own crate: ~100 unit and property tests, examples and doctests, all run by CI |

## The console

Every frame, sent or received, on one aligned line; a CLI that edits like a shell
(history, cursor, `help <cmd>`); settings changed live:

![CLI session: help period, pattern ocean, then period 4Hz: the Δ column drops to 250 ms](docs/book/assets/demo/cli.gif)

More recordings, as smooth and pausable text (link tests at 1 Mbaud, bursts, the
console over RTT with probe-rs): **[the console tour](https://lpwlk.github.io/esp32-rust-workshop/console/)**.

## Quick start

```sh
rustup target add riscv32imafc-unknown-none-elf    # rust-toolchain.toml does it too
cargo install espflash --locked

cargo run --release --bin node-a    # flash board A and open its console
cargo run --release --bin node-b    # board B, in another terminal
```

Wire GPIO2 → GPIO3 both ways and GND between the boards
([wiring](https://lpwlk.github.io/esp32-rust-workshop/wiring/)), then type `help`.
No Rust at hand? Flash a [release](https://github.com/Lpwlk/esp32-rust-workshop/releases):
`espflash flash --chip esp32s31 --monitor node-a.elf`.

With both boards plugged in, `scripts/dev.py` tells them apart by MAC address:

```sh
python3 scripts/dev.py setup                  # once: Python venv for the tools
python3 scripts/dev.py boards assign a 6C:1A  # this board is node A
python3 scripts/dev.py flash a -m             # build, flash, monitor (+ Flash/RAM table)
python3 scripts/dev.py check                  # everything CI checks, locally
```

## A node is its config

```rust
use esp32_rust_workshop::config::{LedMode, NodeConfig};

const CONFIG: NodeConfig = NodeConfig {
    name: "node-A",
    data_period_ms: 1000.0,
    led1: LedMode::PeerButton,       // breadboard LED shows the peer's button
    ..NodeConfig::DEFAULT
};

esp32_rust_workshop::node_firmware!(CONFIG); // checks CONFIG at compile time
```

A reserved pin or an out-of-range period is a **build error**, not a crash on the
board. The same rules apply to `set` in the CLI and to settings loaded from flash.

The logic underneath runs on your computer too:

```rust
use workshop_core::protocol::{self, Frame, Payload};

let frame = Frame { node: "node-A", seq: 42, t_ms: 12_345,
                    payload: Payload::Data { pressed: true, events: 3, period_us: 2_500_000 } };
let line = protocol::encode(&frame);           // "$DAT,node-A,42,12345,1,3,2500000*3F\n"
assert_eq!(protocol::parse(line.trim_end()), Ok(frame));
```

```sh
cargo run -p workshop-core --target host-tuple --example frames
```

## Documentation

📖 **<https://lpwlk.github.io/esp32-rust-workshop/>**, rebuilt on every push to `main`.

| | |
|---|---|
| [Getting started](https://lpwlk.github.io/esp32-rust-workshop/getting-started/) | tools, wiring, flashing, first commands |
| [The console](https://lpwlk.github.io/esp32-rust-workshop/console/) | every part of the output, the CLI, link tests, RTT |
| [Examples](https://lpwlk.github.io/esp32-rust-workshop/examples/) | runnable code: frames, settings, debouncing, CLI |
| [Firmware walkthrough](https://lpwlk.github.io/esp32-rust-workshop/firmware/) | the embedded code explained: boot, main loop, UART interrupt, shared state, flash, RMT |
| [Configuration](https://lpwlk.github.io/esp32-rust-workshop/configuration/) | Cargo features, node config, saved settings, where to change what |
| [Architecture](https://lpwlk.github.io/esp32-rust-workshop/architecture/) | two crates, the main loop, 5 Mbaud reception, timing |
| [Rust & Cargo guide](https://lpwlk.github.io/esp32-rust-workshop/guide-rust/) | Cargo, then Cargo for microcontrollers, and a cheat sheet |
| [Reference](https://lpwlk.github.io/esp32-rust-workshop/reference/cli/) | CLI, settings, protocol, pins, patterns: generated from the code |

## Repository layout

```text
src/                     firmware: hardware glue (UART, GPIO, RMT, flash, watchdog)
src/bin/node_{a,b}.rs    one binary per node = its NodeConfig
crates/workshop-core/    hardware-free logic, no_std, tested on the host
  examples/              runnable examples, included in the docs
tests/hil.rs             on-target tests (embedded-test)
docs/book/               documentation site (Zensical); reference/ is generated
scripts/                 dev.py (boards, flash, tests, docs), memsize.py, record.py
```

## Development

```sh
cargo test-host                                   # host tests, examples' build, docs freshness
cargo clippy --release --all-features -- -D warnings
cargo fmt --all                                   # style: rustfmt.toml
UPDATE_DOCS=1 cargo test-host                     # regenerate the reference pages
```

CI checks formatting, runs the host tests and the examples, builds and lints the
firmware for each feature combination (with a Flash/RAM report, compared with the
target branch on pull requests), then publishes the docs. A `v*` tag publishes a
release with ready-to-flash firmware for both nodes.

> [!WARNING]
> **ESP32-S31 support is young.** esp-hal marks the chip as early-stage, and
> probe-rs 0.32 writes a wrong minimum chip revision into the images it flashes.
> Flash with **espflash**; use probe-rs only to attach (RTT, debugging). See the
> [Rust & Cargo guide](https://lpwlk.github.io/esp32-rust-workshop/guide-rust/#flash-run-debug).
