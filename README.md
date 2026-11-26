<div align="center">

# ESP32-S31 workshop

**Two ESP32-S31 boards talking to each other, over Gigabit Ethernet or over a UART, in `no_std` Rust.**

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

| | **Ethernet app** | **UART app** |
|---|---|---|
| link | the board's RJ45, Gigabit, UDP or TCP, static IPs | UART, three wires |
| folder | [`firmware/ethernet/`](firmware/ethernet) | [`firmware/uart/`](firmware/uart) |
| run | `cd firmware/ethernet && cargo run --release --bin node-a` | `cd firmware/uart && cargo run --release --bin node-a` |
| settings | [`firmware/ethernet/presets/`](firmware/ethernet/presets) | [`firmware/uart/presets/`](firmware/uart/presets) |
| esp-hal | `main` (the S31 EMAC driver isn't released yet), async with embassy | 1.2.2, blocking main loop |

Everything that isn't the link (frames, CLI, settings, LEDs, buttons, saved settings) is the same
code, in [`firmware/shared/core`](firmware/shared/core), tested on your computer.

## Quick start

```sh
rustup target add riscv32imafc-unknown-none-elf    # rust-toolchain.toml does it too
cargo install espflash --locked
```

**Ethernet**: plug a cable between the two RJ45 ports (a straight cat5e is enough, no switch, no DHCP):

```sh
cd firmware/ethernet
cargo run --release --bin node-a    # flash board A and open its console: 192.168.77.11
cargo run --release --bin node-b    # board B, in another terminal: 192.168.77.12
```

**UART**: wire GPIO2 → GPIO3 both ways and GND between the boards
([wiring](https://lpwlk.github.io/esp32-rust-workshop/wiring/)), then:

```sh
cd firmware/uart
cargo run --release --bin node-a
cargo run --release --bin node-b    # in another terminal
```

Type `help` in either console. No Rust at hand? Flash a
[release](https://github.com/Lpwlk/esp32-rust-workshop/releases): `espflash flash --chip esp32s31 --monitor node-a.elf`.
With both boards plugged in, `cargo run` asks which serial port to use; set `ESPFLASH_PORT=/dev/cu.usbmodem…`
to skip the question, or let `scripts/dev.py` tell the boards apart by MAC address:

```sh
python3 scripts/dev.py setup                       # once: Python venv for the tools
python3 scripts/dev.py boards assign a 6C:1A       # this board is node A
python3 scripts/dev.py flash a -m                  # build, flash, monitor (+ Flash/RAM table): Ethernet app
python3 scripts/dev.py flash a -m --app uart       # the same for the UART app
python3 scripts/dev.py check                       # everything CI checks, locally
```

## A third app: the ADC to your computer

One board samples an analog pin (up to 83 kS/s, with the ESP32-S31's continuous ADC and DMA) and streams it over
Ethernet to your computer, which draws a live spectrum in the browser (Plotly). Folder
[`firmware/adc-stream/`](firmware/adc-stream), computer side [`tools/adc-fft/`](tools/adc-fft).

```sh
python3 scripts/dev.py flash a -m --app adc  # node A's board (or b)
python3 scripts/dev.py fft                   # the page (or `fft --demo` without a board)
```

`dev.py` flashes any of the three firmwares from the repository root, with `--app eth` (default), `--app uart`
or `--app adc`: no need to `cd` into a firmware folder.

The page has a spectrum, a waveform with an oscilloscope trigger, a harmonics view (with aliasing shown), a waterfall,
a digital filter, measurements, a status box for the link and global colour themes, and the board's log with a command line; the board can also play a **PWM wave** (RMT, GPIO20) to give its ADC a signal you
know. **Your settings are kept** in `~/.config/adc-fft/settings.json`: rebuild, restart, change the port, and the
page comes back as you left it (`--settings FILE`, `--no-save`, `--reset-settings`, or *Reset all* in the page).
Everything about it: [the ADC app page](https://lpwlk.github.io/esp32-rust-workshop/adc-stream/).

## A dashboard for both boards

The Ethernet and UART apps are driven from the USB console of each board. `dev.py dash` opens both consoles for you
and puts a page around them ([`tools/node-dash/`](tools/node-dash)): the consoles as you know them (colours, live
line, traffic dashboard, a command line with history), every CLI command as a control (verbose level, RGB pattern,
LEDs, tx mode and period, baud, burst, traffic generator, network trace, the peer's address space, the settings
table with `set` / `save` / `forget` / `reset`, and pull / push of the peer's whole config over the Ethernet or UART link), and charts read from what the boards print (frames per second, round-trip
time, interval between data frames, losses, burst and generator results). Nothing changes in the firmware.

```sh
python3 scripts/dev.py dash                  # node A and node B (`boards assign`), close `monitor` first
python3 scripts/dev.py dash --demo [uart]    # two simulated nodes, no hardware
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

**Why four Cargo workspaces.** The UART app is a member of the root workspace and builds on released crates. The Ethernet
driver for the S31 is merged in esp-hal but not released, so the Ethernet app pins esp-hal to a commit and has
**its own workspace** (own `Cargo.lock`, own `target/`); two esp-hal versions can't live in one. The ADC app
(`firmware/adc-stream/`) and the Ferrite port (`firmware/ferrite-esp32s31/`) do the same, and the ADC app reuses the
Ethernet app's bring-up by path. They all use `workshop-core` by path. When a release ships the driver, they can merge.
Run `cargo` **in the folder** of the one you want, or let `dev.py … --app …` do it.

> [!WARNING]
> **ESP32-S31 support is young.** esp-hal marks the chip as early-stage, and
> probe-rs 0.32 writes a wrong minimum chip revision into the images it flashes.
> Flash with **espflash**; use probe-rs only to attach (RTT, debugging). See the
> [Rust & Cargo guide](https://lpwlk.github.io/esp32-rust-workshop/guide-rust/#flash-run-debug).

## Tools and workflows

**Rust tools** (all but the first two are optional):

| Tool | For | Command |
|---|---|---|
| `rustup` | the toolchain of `rust-toolchain.toml`, installed on the first `cargo` | `rustup show` |
| `cargo` + `rustc` | build, run, test; `.cargo/config.toml` sets the target and the flashing runner | `cargo build --release` |
| `espflash` | flash and serial console (the runner); `save-image` for releases | `cargo install espflash --locked` |
| `probe-rs`, `cargo-embed` | RTT console and debugging, attach only | `cargo install probe-rs-tools --locked` |
| `rustfmt`, `clippy` | style (`rustfmt.toml`, 120 columns) and lints (`-D warnings` in CI) | `cargo fmt --all`, `cargo clippy --release -- -D warnings` |
| `rustdoc` | the API pages of the site | `cargo doc -p workshop-core --target host-tuple --open` |
| `cargo-binutils` | `cargo size`; `scripts/memsize.py` does the same table | `cargo install cargo-binutils --locked` |
| `cargo test-host`, `cargo presets`, `cargo test-target` | aliases: tests on your computer, the TOML presets tool, tests on a board | see [Development](#development) |

**Python tools** (`python3 scripts/dev.py setup` once; they all run in `.venv`): `scripts/dev.py` (boards, flash, tests,
docs, git), Zensical (this site), black (formatting), `tools/adc-fft` (the ADC page), `tools/node-dash` (the nodes'
dashboard), `tools/wireshark` (Lua dissectors: `wsnode`, `wsgen`, `wsadc`), `scripts/record_demos.py` (the GIFs).

| I want to… | Run |
|---|---|
| check what is installed and plugged in | `python3 scripts/dev.py doctor` |
| flash and watch a node | `python3 scripts/dev.py flash a -m [--app eth\|uart\|adc]` |
| test without a board | `cargo test-host` |
| see the ADC spectrum, or both nodes on a page | `python3 scripts/dev.py fft [--demo]`, `python3 scripts/dev.py dash [--demo]` |
| format and check everything before a push | `python3 scripts/dev.py format`, then `python3 scripts/dev.py check` |
| preview the docs | `python3 scripts/dev.py docs serve --open` |
| release | bump `version` in `Cargo.toml`, then `git tag vX.Y.Z && git push origin vX.Y.Z` |

The four Cargo workspaces, every job of CI and the checklist "when you change…" are on the
[Tools and workflows](https://lpwlk.github.io/esp32-rust-workshop/tools/) page.

## Repository layout

```text
.
├── Cargo.toml  Cargo.lock       workspace root (no code), exact dependency versions
├── rust-toolchain.toml  rustfmt.toml  .clippy.toml  .cargo/    toolchain, style, runner, aliases
├── firmware/
│   ├── uart/                    the UART app: src/ (hardware glue), tests/ (on-target), presets/, Embed.toml
│   ├── ethernet/                the Ethernet app (own Cargo workspace): src/, presets/
│   ├── adc-stream/              the ADC app (own Cargo workspace): continuous ADC + DMA, UDP stream, PWM generator
│   ├── ferrite-esp32s31/        the Ferrite RTOS port for the S31 (own Cargo workspace): context switch, tick, demo
│   └── shared/
│       ├── core/                hardware-free logic, no_std, tested on the host; examples/ run on your computer
│       ├── ferrite/             the RTOS kernel: tasks, scheduler, semaphore/mutex/queue, tested on the host
│       └── presets-tool/        host-only: reads and validates the TOML presets (`cargo presets`)
├── tools/                       computer side, Python: adc-fft/ (the ADC page), node-dash/ (the nodes' dashboard), wireshark/ (Lua dissectors)
├── docs/                        the documentation site: book/ (pages; book/reference/ is generated), zensical.toml
├── scripts/                     dev.py + devlib.py + boards.py (boards, flash, tests, docs), memsize.py, record*.py, render_demos.py, gifrender.py, requirements.txt
└── .github/                     CI (ci.yml) and releases (release.yml)
```

## Documentation

📖 **<https://lpwlk.github.io/esp32-rust-workshop/>**, rebuilt on every push to `main`.

| | |
|---|---|
| [Getting started](https://lpwlk.github.io/esp32-rust-workshop/getting-started/) | tools, wiring, flashing, first commands |
| [The console](https://lpwlk.github.io/esp32-rust-workshop/console/) | every part of the output, the CLI, link tests, RTT |
| [Ethernet app](https://lpwlk.github.io/esp32-rust-workshop/ethernet/) | the RJ45 link: design, measurements, what is still open |
| [ADC app](https://lpwlk.github.io/esp32-rust-workshop/adc-stream/) | one board's ADC streamed to a live page, the PWM generator, the saved settings |
| [Tools and workflows](https://lpwlk.github.io/esp32-rust-workshop/tools/) | every Rust and Python tool, the four workspaces, what to run before a push, CI |
| [Ferrite](https://lpwlk.github.io/esp32-rust-workshop/ferrite/) | the project's own RTOS in Rust: kernel (host-tested), ESP32-S31 port (21-minute demo and a self-checking test pass on a board) |
| [Protocols](https://lpwlk.github.io/esp32-rust-workshop/protocols/) | the frames, the three links, and UA-lite: an OPC UA-style address space of LEDs and buttons |
| [Presets](https://lpwlk.github.io/esp32-rust-workshop/presets/) | the TOML format of a node's settings |
| [Examples](https://lpwlk.github.io/esp32-rust-workshop/examples/) | runnable code: frames, settings, debouncing, CLI |
| [Firmware walkthrough](https://lpwlk.github.io/esp32-rust-workshop/firmware/) | the UART app's code explained: boot, main loop, interrupt, shared state, flash, RMT |
| [Configuration](https://lpwlk.github.io/esp32-rust-workshop/configuration/) | Cargo features, node config, saved settings, where to change what |
| [Architecture](https://lpwlk.github.io/esp32-rust-workshop/architecture/) | the crates, the main loop, frame reception, timing |
| [Rust & Cargo guide](https://lpwlk.github.io/esp32-rust-workshop/guide-rust/) | Cargo, then Cargo for microcontrollers, and a cheat sheet |
| [Reference](https://lpwlk.github.io/esp32-rust-workshop/reference/cli/) | CLI, settings, protocol, pins, patterns (UART and Ethernet): generated from the code |

## Development

```sh
cargo test-host                                   # host tests (core, Ferrite kernel, presets), examples' build, docs freshness
cargo presets check                               # every preset loads and validates
cargo clippy --release --all-features -- -D warnings
cargo fmt --all                                   # style: rustfmt.toml; the other three workspaces need `cargo fmt` in their folder
UPDATE_DOCS=1 cargo test-host                     # regenerate the reference pages
python3 tools/adc-fft/adc_fft.py --selftest       # the ADC page: receiver, FFT, web server, saved settings (no board)
python3 tools/node-dash/node_dash.py --selftest   # the nodes' dashboard, against two simulated nodes
python3 scripts/dev.py format                     # cargo fmt (root, Ethernet, ADC) + black, in one go
python3 scripts/dev.py check                      # all of the above plus the three firmware builds, the PWM arithmetic and the site
```

CI checks formatting (the four workspaces), runs the host tests and the examples, builds and lints the UART, Ethernet
(four feature sets), ADC and Ferrite firmwares (with a Flash/RAM report, compared with the target branch on pull
requests), runs the self tests of both Python tools, then publishes the docs. A `v*` tag publishes a release with
ready-to-flash firmware for both UART nodes.
