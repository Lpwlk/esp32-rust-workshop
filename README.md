# ESP32-S31 UART workshop

[![CI](https://github.com/Lpwlk/esp32-rust-workshop/actions/workflows/ci.yml/badge.svg)](https://github.com/Lpwlk/esp32-rust-workshop/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-blue)](https://lpwlk.github.io/esp32-rust-workshop/)

Two **ESP32-S31-Function-CoreBoard-1** boards talking over UART at 5 Mbaud, in
`no_std` Rust on [esp-hal](https://github.com/esp-rs/esp-hal) 1.2.

- Five frame types with checksums, lost-frame detection, interrupt-driven reception
- Link tests: speed change negotiated by both nodes, round-trip time, bursts
- Wire-timed delta between `DAT` frames, sent or received (µs resolution)
- A CLI on the USB console: history, in-line editing, `set` / `save` / `help`
- Settings checked at **compile time**, changed live, **saved to flash**
- Hardware watchdog, reset reason, RTT console through the debug probe
- Hardware-free logic in its own crate, **tested on the host** (unit, property, doc tests)

📖 **Documentation: <https://lpwlk.github.io/esp32-rust-workshop/>**

## Quick start

```sh
rustup target add riscv32imafc-unknown-none-elf   # rust-toolchain.toml does it too
cargo install espflash --locked

cargo run --release --bin node-a    # flash board A and open its console
cargo run --release --bin node-b    # board B, in another terminal
```

Wire TX↔RX (GPIO2 → GPIO3, both ways) and GND between the boards first:
[Wiring](https://lpwlk.github.io/esp32-rust-workshop/wiring/). Then type `help`.

With both boards plugged in, `scripts/dev.py` finds each one by its MAC address:

```sh
python3 scripts/dev.py setup          # once: Python venv for the tools
python3 scripts/dev.py boards assign a 6C:1A
python3 scripts/dev.py flash a -m     # build, flash, monitor
python3 scripts/dev.py check          # everything CI checks, locally
```

## Repository layout

```text
src/                    firmware: hardware glue (UART, GPIO, RMT, flash, watchdog)
src/bin/node_{a,b}.rs   one binary per node = its NodeConfig
crates/workshop-core/   hardware-free logic, no_std, tested on the host
tests/hil.rs            on-target tests (embedded-test)
docs/book/              documentation site (Zensical); reference/ is generated
scripts/                dev.py: boards, flash, monitor, RTT, tests, docs
```

## Configuration at a glance

| What | Where |
|---|---|
| What's compiled in | Cargo features: `persist`, `watchdog` (default), `rtt` |
| What both nodes share | `config::shared` (baud rate) |
| Each node's defaults | `src/bin/node_*.rs`, checked at build time |
| Live changes | CLI `set`, kept across reboots with `save` |

Details: [Configuration](https://lpwlk.github.io/esp32-rust-workshop/configuration/).

## Development

```sh
cargo test-host                                   # host tests + docs freshness
cargo clippy --release --all-features -- -D warnings
cargo fmt --all                                   # style: rustfmt.toml
UPDATE_DOCS=1 cargo test-host                     # regenerate reference pages
```

CI (`.github/workflows/ci.yml`) checks formatting, runs the host tests, builds and
lints the firmware for each feature combination, then publishes the docs on GitHub
Pages from `main`.

> **ESP32-S31 support is young.** esp-hal marks the chip as early-stage, and
> probe-rs 0.32 writes a wrong minimum chip revision into the images it flashes.
> Flash with **espflash**; use probe-rs only to attach (RTT, debugging). See the
> [Rust & Cargo guide](https://lpwlk.github.io/esp32-rust-workshop/guide-rust/).
