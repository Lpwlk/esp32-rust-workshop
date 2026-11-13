<div align="center">

# ESP32-S31 workshop

**Two ESP32-S31 boards talking to each other, over a 5 Mbaud UART or over Gigabit Ethernet, in `no_std` Rust.**

[![CI](https://github.com/Lpwlk/esp32-rust-workshop/actions/workflows/ci.yml/badge.svg)](https://github.com/Lpwlk/esp32-rust-workshop/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-lpwlk.github.io-4D9DD1)](https://lpwlk.github.io/esp32-rust-workshop/)
[![Release](https://img.shields.io/github/v/release/Lpwlk/esp32-rust-workshop?include_prereleases&label=firmware)](https://github.com/Lpwlk/esp32-rust-workshop/releases)
![Rust](https://img.shields.io/badge/rust-stable%201.95%2B-orange)
![esp-hal](https://img.shields.io/badge/esp--hal-1.2-blue)

![Boot of node A over Ethernet: the boot card, the cable coming up at 1 Gbit/s, then frames exchanged with node B](docs/book/assets/demo/eth-boot.gif)

</div>

Each board sends periodic `DAT` frames and an `EVT` frame with a random 64-bit key on every
button edge, decodes everything the other one sends, and shows it all on a colored console with
a real CLI. Built on [esp-hal](https://github.com/esp-rs/esp-hal) for the
ESP32-S31-Function-CoreBoard-1. The same node exists with two different **links**:

| | **UART app** | **Ethernet app** |
|---|---|---|
| link | 5 Mbaud UART, three wires | the board's RJ45, Gigabit, UDP or TCP, static IPs |
| folder | [`firmware/uart/`](firmware/uart) | [`firmware/ethernet/`](firmware/ethernet) |
| run | `cargo run --release --bin node-a` | `cd firmware/ethernet && cargo run --release --bin node-a` |
| settings | [`firmware/uart/presets/`](firmware/uart/presets) | [`firmware/ethernet/presets/`](firmware/ethernet/presets) |
| esp-hal | 1.2.2, blocking main loop | `main` (the S31 EMAC driver isn't released yet), async with embassy |

Everything that isn't the link (frames, CLI, settings, LEDs, buttons, saved settings) is the same
code, in [`firmware/shared/core`](firmware/shared/core), tested on your computer.

## Quick start

```sh
rustup target add riscv32imafc-unknown-none-elf    # rust-toolchain.toml does it too
cargo install espflash --locked
```

**UART**: wire GPIO2 → GPIO3 both ways and GND between the boards
([wiring](https://lpwlk.github.io/esp32-rust-workshop/wiring/)), then:

```sh
cargo run --release --bin node-a    # flash board A and open its console
cargo run --release --bin node-b    # board B, in another terminal
```

**Ethernet**: plug a cable between the two RJ45 ports (a straight cat5e is enough, no switch, no DHCP):

```sh
cd firmware/ethernet
cargo run --release --bin node-a    # 192.168.77.11
cargo run --release --bin node-b    # 192.168.77.12, in another terminal
```

Type `help` in either console. No Rust at hand? Flash a
[release](https://github.com/Lpwlk/esp32-rust-workshop/releases): `espflash flash --chip esp32s31 --monitor node-a.elf`.
With both boards plugged in, `cargo run` asks which serial port to use; set `ESPFLASH_PORT=/dev/cu.usbmodem…`
to skip the question, or let `scripts/dev.py` tell the boards apart by MAC address:

```sh
python3 scripts/dev.py setup                       # once: Python venv for the tools
python3 scripts/dev.py boards assign a 6C:1A       # this board is node A
python3 scripts/dev.py flash a -m                  # build, flash, monitor (+ Flash/RAM table)
python3 scripts/dev.py --app eth flash a -m        # the same for the Ethernet app
python3 scripts/dev.py check                       # everything CI checks, locally
```

## What it looks like (Ethernet)

Every scene below is a real recording of two boards, made by `python3 scripts/record_demos.py`.
The look of the GIFs (font size, theme, speed, line height...) is in
[`render.toml`](docs/book/assets/demo/render.toml); to change it without a board, re-render the saved
recordings: `python3 scripts/render_demos.py --font-size 12`.
Each one is shown with the preset it runs.

### Boot: the cable comes up at 1 Gbit/s

```toml
# firmware/ethernet/presets/node-a.toml (everything else comes from base.toml)
extends = "base"
name    = "node-A"

[net]
address = "192.168.77.11"
peer    = "192.168.77.12"
```

The console starts on a **live line**: one line, updated in place, with the last frame and the counters, instead of a
scrolling wall of TX/RX. `verbose 1` shows every frame, `verbose` goes on to the raw wire.

![Boot card, "cable connected: 1 Gbit/s · full duplex", the live line, then verbose 1 and verbose](docs/book/assets/demo/eth-boot.gif)

### The CLI: `link`, `config`, `led`, `pattern`

The ranges, the units and the checks are the ones of the build: `set` refuses what a preset would refuse.
`link` replaces the UART's `baud`: cable, negotiated speed, addresses, transport, counters.

![link, config, led 2 blink 300, pattern ocean](docs/book/assets/demo/eth-cli.gif)

### Bursts and round-trip times

Frames sent back to back (`burst 2000 max`), at a fixed rate (`burst 5000 1kHz`), and `ECH` replies giving the
round-trip time when node B runs `tx on-rx`. Both nodes count lost and corrupted frames.

```toml
[burst]
count   = 1000       # defaults of the `burst` command
rate_hz = 0          # 0 = back to back
```

![burst 2000 max, burst 20000 max, burst 5000 1kHz, then ECH round-trip times](docs/book/assets/demo/eth-burst.gif)

### Same node over TCP

One preset line changes the transport; the client keeps retrying until the server is there.

```toml
# firmware/ethernet/presets/node-a-tcp.toml
extends = "node-a"

[net]
transport = "tcp"
tcp_role  = "client"    # node-b-tcp.toml has "server"
```

![TCP connected to the peer, link, burst 2000 max](docs/book/assets/demo/eth-tcp.gif)

The UART app's console looks the same, with `baud` instead of `link`:
[the console tour](https://lpwlk.github.io/esp32-rust-workshop/console/) plays those recordings as smooth, pausable text.

## All these TOML files: what each one is for

You will see a lot of `.toml` here. They are **four different kinds of file**, read by four different tools:

| File | Read by | What it is for |
|---|---|---|
| [`rust-toolchain.toml`](rust-toolchain.toml) | **rustup** | which Rust to use: stable, with `rust-src`, and the `riscv32imafc-unknown-none-elf` target the chip needs. Installed automatically the first time you run `cargo` here |
| [`.cargo/config.toml`](.cargo/config.toml) | **cargo** | build for that RISC-V target by default; the **runner** (`cargo run` = flash with `espflash` and open the console); linker flags; the aliases `cargo test-host`, `cargo presets`, `cargo test-target`. [`.cargo/esp-config.toml`](.cargo/esp-config.toml) holds the settings of the esp-rs crates |
| [`Cargo.toml`](Cargo.toml) (root) | **cargo** | the *workspace*: which packages live here, the versions they share, the build profiles (size-optimized, LTO). It has no code of its own. `Cargo.lock` freezes the exact version of every dependency |
| [`firmware/uart/Cargo.toml`](firmware/uart/Cargo.toml), [`firmware/ethernet/Cargo.toml`](firmware/ethernet/Cargo.toml) | **cargo** | one *firmware package* each: its binaries (`node-a`, `node-b`), its dependencies (esp-hal, embassy…) and its **features** (`persist`, `watchdog`, `rtt`: what gets compiled in) |
| [`firmware/uart/Embed.toml`](firmware/uart/Embed.toml) | **cargo-embed** (probe-rs) | how to attach to a running board and show the RTT console |
| [`rustfmt.toml`](rustfmt.toml), [`.clippy.toml`](.clippy.toml) | **rustfmt**, **clippy** | code style and lint thresholds |
| **[`firmware/*/presets/*.toml`](firmware/uart/presets)** | the firmware's **build script** | **not Cargo files.** The *settings of a node*: its name, pins, periods, LED modes and, on Ethernet, its IP addresses |
| [`docs/zensical.toml`](docs/zensical.toml) | **Zensical** | the documentation site |

The one to remember is the **preset**. Cargo decides *what is built* (features, dependencies); a preset decides
*what the node does* (settings). The build script reads the preset, checks it, and turns it into Rust
that the compiler checks again, so a reserved pin or an invalid IP address is a **build error that names
the file and the key**, not a crash on the board:

```text
presets: `my-preset` (firmware/ethernet/presets/my-preset.toml): `net.peer` is not in the subnet of `net.address`
```

[`base.toml`](firmware/ethernet/presets/base.toml) in each folder documents every setting, its range and its default;
`node-a.toml` / `node-b.toml` only list what differs, and `extends = "base"` brings the rest.
To make your own, copy one and build it:

```sh
cp firmware/ethernet/presets/node-a.toml firmware/ethernet/presets/fast.toml    # edit it
cd firmware/ethernet && PRESET_NODE_A=fast cargo run --release --bin node-a
cargo presets list          # every preset of both apps      cargo presets show eth fast   # what one results in
cargo presets check         # validate them all (CI does)    docs/book/presets.md          # the format, in full
```

The same settings can also be changed **live** in the console (`set rgb.pattern ocean`), and kept with `save`:
precedence is *preset → saved in flash → `set`*.

## Working with the boards: Cargo and the Rust ecosystem

The short version: **rustup** picks the compiler, **cargo** builds, **espflash** flashes, and everything else is a crate.

```text
 you ─ cargo run ─▶ rustc ─▶ ELF for the chip ─▶ espflash ─▶ board ◀─ USB ─ console / CLI
                    ▲                                                  (RTT through probe-rs, optional)
        rustup (rust-toolchain.toml) · crates: esp-hal, embassy, heapless…
```

| I want to… | Command | What happens |
|---|---|---|
| build for the board | `cargo build --release` | cross-compiles `no_std` Rust to RISC-V; `build.rs` adds the linker script and turns the presets into code |
| flash and watch | `cargo run --release --bin node-a` | the **runner** from `.cargo/config.toml`: `espflash flash --monitor` |
| pick the board | `ESPFLASH_PORT=/dev/cu.usbmodem… cargo run …` | or `dev.py boards assign` |
| test the logic | `cargo test-host` | runs the hardware-free logic on **your computer**: unit, property and doc tests |
| try another preset | `PRESET_NODE_A=name cargo run …` | the build script reads `presets/name.toml` |
| turn features on/off | `--features rtt`, `--no-default-features` | the `[features]` of the firmware's `Cargo.toml` |
| lint, format | `cargo clippy --release -- -D warnings`, `cargo fmt` | style from `rustfmt.toml` |
| read the API docs | `cargo doc --open -p workshop-core` | rustdoc of the shared crate |
| debug through a probe | `cargo embed --release --features rtt` (in `firmware/uart/`) | attach only: **flash with espflash**, see the warning below |

**Which crates do what.** `esp-hal` talks to the chip (GPIO, UART, RMT, flash, the Ethernet MAC);
`esp-rtos` and `embassy-executor` run the Ethernet app's tasks; `embassy-net` (smoltcp) is its TCP/IP stack;
`esp-println` and `esp-backtrace` print and report panics; `heapless` gives `Vec` and `String` without a heap;
`critical-section` shares data with interrupts. The logic that needs none of them, `workshop-core`, builds for the
chip *and* for your computer, which is why most of the tests don't need a board.

**Why two Cargo workspaces.** The UART app is a member of the root workspace and builds on released crates. The Ethernet
driver for the S31 is merged in esp-hal but not released, so the Ethernet app pins esp-hal to a commit and has
**its own workspace** (own `Cargo.lock`, own `target/`); two esp-hal versions can't live in one. Both use
`workshop-core` by path. When a release ships the driver, they can merge.

> [!WARNING]
> **ESP32-S31 support is young.** esp-hal marks the chip as early-stage, and
> probe-rs 0.32 writes a wrong minimum chip revision into the images it flashes.
> Flash with **espflash**; use probe-rs only to attach (RTT, debugging). See the
> [Rust & Cargo guide](https://lpwlk.github.io/esp32-rust-workshop/guide-rust/#flash-run-debug).

## Repository layout

```text
.
├── Cargo.toml  Cargo.lock       workspace root (no code), exact dependency versions
├── rust-toolchain.toml  rustfmt.toml  .clippy.toml  .cargo/    toolchain, style, runner, aliases
├── firmware/
│   ├── uart/                    the UART app: src/ (hardware glue), tests/ (on-target), presets/, Embed.toml
│   ├── ethernet/                the Ethernet app (own Cargo workspace): src/, presets/
│   └── shared/
│       ├── core/                hardware-free logic, no_std, tested on the host; examples/ run on your computer
│       └── presets-tool/        host-only: reads and validates the TOML presets (`cargo presets`)
├── docs/                        the documentation site: book/ (pages; book/reference/ is generated), zensical.toml
├── scripts/                     dev.py (boards, flash, tests, docs), record.py, record_demos.py, render_demos.py, gifrender.py, memsize.py
└── .github/                     CI and releases
```

## Documentation

📖 **<https://lpwlk.github.io/esp32-rust-workshop/>**, rebuilt on every push to `main`.

| | |
|---|---|
| [Getting started](https://lpwlk.github.io/esp32-rust-workshop/getting-started/) | tools, wiring, flashing, first commands |
| [The console](https://lpwlk.github.io/esp32-rust-workshop/console/) | every part of the output, the CLI, link tests, RTT |
| [Ethernet app](https://lpwlk.github.io/esp32-rust-workshop/ethernet/) | the RJ45 link: design, measurements, what is still open |
| [Protocols](https://lpwlk.github.io/esp32-rust-workshop/protocols/) | the frames, the three links, and UA-lite: an OPC UA-style address space of LEDs and buttons |
| [Presets](https://lpwlk.github.io/esp32-rust-workshop/presets/) | the TOML format of a node's settings |
| [Examples](https://lpwlk.github.io/esp32-rust-workshop/examples/) | runnable code: frames, settings, debouncing, CLI |
| [Firmware walkthrough](https://lpwlk.github.io/esp32-rust-workshop/firmware/) | the UART app's code explained: boot, main loop, interrupt, shared state, flash, RMT |
| [Configuration](https://lpwlk.github.io/esp32-rust-workshop/configuration/) | Cargo features, node config, saved settings, where to change what |
| [Architecture](https://lpwlk.github.io/esp32-rust-workshop/architecture/) | the crates, the main loop, 5 Mbaud reception, timing |
| [Rust & Cargo guide](https://lpwlk.github.io/esp32-rust-workshop/guide-rust/) | Cargo, then Cargo for microcontrollers, and a cheat sheet |
| [Reference](https://lpwlk.github.io/esp32-rust-workshop/reference/cli/) | CLI, settings, protocol, pins, patterns (UART and Ethernet): generated from the code |

## Development

```sh
cargo test-host                                   # host tests, examples' build, docs and presets freshness
cargo presets check                               # every preset loads and validates
cargo clippy --release --all-features -- -D warnings
cargo fmt --all                                   # style: rustfmt.toml (firmware/ethernet is its own workspace: cargo fmt there too)
UPDATE_DOCS=1 cargo test-host                     # regenerate the reference pages
python3 scripts/dev.py check                      # all of the above plus both firmware builds and the site
```

CI checks formatting, runs the host tests and the examples, builds and lints both firmwares for each feature
combination (with a Flash/RAM report, compared with the target branch on pull requests), then publishes the docs.
A `v*` tag publishes a release with ready-to-flash firmware for both nodes.
