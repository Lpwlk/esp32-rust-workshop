# Rust, Cargo and embedded Rust

This guide gathers in one place what you need to work on this project: **Cargo**
(Rust's build tool), then **Cargo for embedded** (MCUs without an OS), with the
pitfalls specific to the ESP32-S31.

**The commands to copy are at the end**: [Cheat sheet](#cheat-sheet). One command
per block, with the copy button at the top right of each block.

---

## Part 1: Cargo

### The tools

| Tool | Role |
|---|---|
| **rustup** | installs and selects toolchains (`stable`, `nightly`, a given version), *targets* (`rustup target add …`) and *components* (`rust-src`, `clippy`, `llvm-tools`) |
| **cargo** | build and dependency manager; calls `rustc` for you |
| **rustc** | the compiler (rarely called directly) |
| **`rust-toolchain.toml`** | pins a project's toolchain; rustup installs it on the fly |

In this project, `rust-toolchain.toml` holds `channel = "stable"`,
`targets = ["riscv32imafc-unknown-none-elf"]` and `components = ["rust-src"]`.

### Anatomy of a project

```text
Cargo.toml        manifest: metadata, dependencies, features, profiles
Cargo.lock        exact resolved versions (commit it for an application)
src/lib.rs        the package's library
src/main.rs       the default binary
src/bin/*.rs      extra binaries (discovered automatically)
tests/*.rs        integration tests (each file is a separate crate)
examples/*.rs     examples (cargo run --example x)
benches/*.rs      benchmarks
build.rs          script run on the host before compiling
target/           everything generated (ignored by git)
```

A **package** is one `Cargo.toml`. It holds compilation **targets**: at most one
lib, and as many binaries, tests and examples as you like. Each target is compiled
into a **crate**.

### The `Cargo.toml` manifest

```toml
[package]
name = "esp32-rust-workshop"
version = "0.1.0"
edition = "2024"          # language dialect
rust-version = "1.95"     # MSRV: minimum Rust version required

[lib]                     # settings of the lib target
test = false

[[bin]]                   # an explicitly declared binary
name = "node-a"           # binary name: cargo run --bin node-a
path = "src/bin/node_a.rs"  # convention: dash in the name, underscore in the file

[[test]]
name = "hil"
harness = false           # not the standard test harness (useful on embedded)

[dependencies]            # used by the code
[dev-dependencies]        # only for tests, examples, benchmarks
[build-dependencies]      # only for build.rs
[features]
[profile.release]
[lints]
```

### Essential commands

| Command | Role |
|---|---|
| `cargo new x` / `cargo init` | create a project (`--lib` for a library) |
| `cargo check` | check without producing a binary (the fastest) |
| `cargo build` | compile (`dev` profile); `--release` for the release profile |
| `cargo run` | compile then run (`--bin x`, `--example y`, `-- args`) |
| `cargo test` | unit, integration and doc tests; `cargo test name` filters; `-- --nocapture` shows `println` output |
| `cargo doc --open` | build the docs; `--no-deps` for your own code only |
| `cargo clippy` | linter; `-- -D warnings` fails on warnings (as in CI) |
| `cargo fmt` | format; `--check` in CI |
| `cargo add serde -F derive` / `cargo remove` | edit dependencies |
| `cargo update` / `cargo update -p heapless` | refresh `Cargo.lock` (everything, or one crate) |
| `cargo tree` | dependency tree: `-i heapless` (who pulls it in?), `-d` (duplicates), `-e features` |
| `cargo clean` | empty `target/` |
| `cargo install tool` | install a command-line tool |

Options that apply everywhere:

- `-p <package>`, `--workspace`: which package to build;
- `--features a,b`, `--no-default-features`, `--all-features`: which features;
- `--target <triple>`: for which platform;
- `--locked`: forbids changing `Cargo.lock` (a must in CI); `--offline`;
- `-v` shows the `rustc` calls; `-j N` sets the parallelism.

### Dependencies

```toml
heapless = "0.9.3"                 # = "^0.9.3": accepts >=0.9.3 and <0.10.0 (semver)
esp-hal = { version = "~1.2.2" }   # ~: only 1.2.x
foo = { path = "crates/foo" }      # local crate
bar = { git = "https://…", rev = "abc123" }
baz = { version = "1", default-features = false, features = ["x"] }
qux = { version = "1", optional = true }   # only exists if a feature enables it
tokio2 = { package = "tokio", version = "1" }  # rename a dependency

[target.'cfg(unix)'.dependencies]          # platform-specific dependency
```

- Cargo keeps **a single version per compatible series**, but can keep several
  incompatible ones. Here `heapless` 0.8 (pulled in by esp-hal) and 0.9 (ours)
  coexist: `cargo tree -d` shows it.
- `Cargo.lock` makes builds reproducible: commit it for a firmware or an
  application.

### Features

```toml
[features]
default = []
rtt = ["dep:rtt-target"]           # enables an optional dependency
std = ["serde?/std"]               # `?`: only if serde is already enabled
```

In the code: `#[cfg(feature = "rtt")]`. Three rules:

- **Features are additive**: they add capabilities, never remove any.
- **Unification**: when two dependencies ask for the same crate with different
  features, it's compiled once with the union of both.
- **No mutually exclusive features** (`board-a` vs `board-b`): they can end up
  enabled together, which is fragile. For variants, prefer binaries or config
  constants.

### Profiles

```toml
[profile.dev]          # cargo build / run / test
opt-level = 0          # 0..3, "s" (size), "z" (smallest)
debug = true

[profile.release]      # --release
opt-level = 3
lto = "fat"            # link-time optimization: slower to build, smaller and faster
codegen-units = 1      # better optimization, less parallel compilation
panic = "abort"        # no stack unwinding
overflow-checks = false

[profile.release.package.foo]  # settings for one dependency
opt-level = 3
```

### Workspaces

```toml
[workspace]
members = [".", "crates/workshop-core"]
default-members = ["."]     # what a `cargo build` without -p builds
resolver = "3"

[workspace.dependencies]    # shared versions: `heapless.workspace = true` in members
[workspace.lints.clippy]    # shared lints: `[lints] workspace = true` in members
```

Members share one `Cargo.lock` and one `target/` folder.

### `.cargo/config.toml`

This file configures **how to build**, not what the package contains. Cargo looks
for it in the current folder and its parents, then in `~/.cargo/`.

```toml
[build]
target = "riscv32imafc-unknown-none-elf"   # default target
rustflags = ["-C", "force-frame-pointers"]

[target.riscv32imafc-unknown-none-elf]
runner = "espflash flash --monitor --chip esp32s31"   # what `cargo run` launches

[alias]
test-host = ["test", "-p", "workshop-core", "--target", "host-tuple"]

[env]
ESP_LOG = "info"
```

- Everything can also be passed on the command line (`--config 'build.target="…"'`)
  or through environment variables (`CARGO_BUILD_TARGET`, `RUSTFLAGS`).
- `rustflags` precedence: `CARGO_ENCODED_RUSTFLAGS` > `RUSTFLAGS` >
  `[target.…].rustflags` > `[build].rustflags`.

### `build.rs`

A program run **on the host** before the crate is compiled. It talks to Cargo by
writing to its standard output:

```rust
println!("cargo:rustc-link-arg=-Tlinkall.x");               // linker option
println!("cargo:rustc-link-arg-tests=-Tembedded-test.x");   // for tests only
println!("cargo:rustc-env=BUILD_DATE=…");                   // read with env!("BUILD_DATE")
println!("cargo:rustc-cfg=has_fpu");                         // enables a #[cfg(has_fpu)]
println!("cargo:rerun-if-changed=src");                      // when to rerun it
println!("cargo:warning=…");
```

Available variables: `OUT_DIR` (to generate files), `TARGET`, `HOST`, `PROFILE`,
`CARGO_FEATURE_<NAME>`… The firmware's boot card (rustc version, build date, git)
comes from there.

### Tests

| Kind | Where | Notes |
|---|---|---|
| Unit | `#[cfg(test)] mod tests { #[test] fn … }`, in the file | can reach private items |
| Integration | `tests/x.rs` | a separate crate, public API only |
| Doctests | code blocks of `///` comments | compiled and run: the docs can't lie |
| Custom harness | `harness = false` | e.g. `embedded-test` on target |

Ecosystem: `proptest` (property tests over thousands of random inputs),
`cargo-nextest` (a faster runner).

### Useful tools (`cargo install …`)

| Tool | Role |
|---|---|
| `cargo-binstall` | install prebuilt binaries |
| `cargo-expand` | see the code produced by macros |
| `cargo-bloat` | what takes room in the binary |
| `cargo-deny` | licenses and known vulnerabilities |
| `cargo-udeps` | unused dependencies |
| `cargo-watch` | rerun a command on every change |
| `cargo-nextest` | test runner |

---

## Part 2: Cargo for embedded

### Targets and toolchain

The **triple** describes the target CPU:

| Triple | Example chips |
|---|---|
| `thumbv7em-none-eabihf` | Cortex-M4F, M7 |
| `riscv32imac-unknown-none-elf` | ESP32-C3, ESP32-C6 |
| `riscv32imafc-unknown-none-elf` | **ESP32-S31** (RISC-V with a single-precision FPU `f`, no `d`) |

- `none` means **no OS**, hence no `std`: only `core`, plus `alloc` if you provide
  an allocator.
- **Xtensa** (ESP32, ESP32-S3) isn't in official Rust: you need `espup` and the
  `esp` toolchain.
- **`build-std`** (nightly) rebuilds `core` from source (`rust-src`): useful for
  targets without a prebuilt `core`, or for `panic_immediate_abort`. On stable, the
  `[unstable]` section of `.cargo/config.toml` is ignored.

### `no_std` / `no_main`

```rust
#![no_std]      // no standard library
#![no_main]     // no regular main(): the entry point comes from the runtime

use esp_backtrace as _;   // provides the mandatory #[panic_handler]

#[esp_hal::main]          // entry point (with cortex-m-rt: #[entry])
fn main() -> ! {
    loop {}
}
```

- **Linker scripts**: `memory.x` describes flash and RAM, `linkall.x` is esp-hal's.
  They're passed by `build.rs` (`rustc-link-arg=-T…`), and the order matters:
  `linkall.x` last.
- **Panic handler**: `esp-backtrace`, `panic-probe` (with probe-rs), `panic-halt`…
  You need exactly one.

### `.cargo/config.toml` for an MCU

- `[build] target` saves typing `--target` on every command.
- `runner` is **the flashing tool** `cargo run` launches: `espflash flash --monitor`
  or `probe-rs run --chip …`.
- `rustflags`: for example `-C force-frame-pointers` to get backtraces.
- To test the logic **on the PC** while the default target is the MCU:
  `--target host-tuple` (this project's `cargo test-host` alias).
- A different runner for tests:
  `--config 'target.<triple>.runner="probe-rs run …"'` (the `test-target` alias).

### Profiles for an MCU

```toml
[profile.dev]
opt-level = "s"        # unoptimized code is huge and slow, sometimes too big for the flash

[profile.dev.package."*"]
opt-level = "s"        # variant: optimize dependencies, keep your code easy to debug

[profile.release]
opt-level = "s"        # or "z" for size; 3 when speed matters
lto = "fat"
codegen-units = 1
debug = 2              # free: debug info stays in the ELF, not in the flash
```

Measuring size:

- `python3 scripts/dev.py size` (also printed after every `dev.py build` / `flash`):
  flash and RAM used, like GCC's `--print-memory-usage`, with the change since the
  previous build;
- `cargo size` (cargo-binutils) for the size of each section, `cargo bloat` for
  the biggest functions, or espflash's `App/part. size` line.

!!! note "Why not the linker's own table?"
    `rust-lld` knows `--print-memory-usage` too (`-C link-arg=--print-memory-usage`,
    shown with `-W linker-messages`), but on esp-hal it prints one line per region
    *alias*, and RAM always at 100 %: the linker script gives all the RAM left to
    the stack. `scripts/memsize.py` reads the ELF sections instead and shows the
    stack apart. `core::fmt` formatting is expensive in size; `defmt` moves formatting to the
PC.

### Features and the HAL

- **Chip selection by feature**: `esp-hal = { features = ["esp32s31", "unstable"] }`.
  One chip at a time: the accepted exception to the "no exclusive features" rule.
- **`default-features = false`** keeps a dependency from pulling `std` into a
  `no_std` build.
- **Resolver 2 and later** keep the features of `dev-dependencies` and
  `build-dependencies` apart from the firmware's. Without that, `proptest` (which
  uses `std`) would contaminate the MCU build.
- **A feature changes behavior, not only what gets compiled.** Seen on this
  project: esp-println's `critical-section` feature masks interrupts during every
  print, which made the UART overflow at 5 Mbaud. It's disabled.

### Flash, run, debug

| Tool | Role | On this project's ESP32-S31 |
|---|---|---|
| **espflash** | flashes over serial or USB-JTAG, ESP-IDF image (bootloader + partition table + app), monitor | ✅ the reference |
| **probe-rs** | `run`, `download`, `attach`, `reset`, `erase`, RTT, GDB server, VS Code debugging | ⚠️ see below |
| **cargo-embed** | probe-rs driven by `Embed.toml`: flash, reset, RTT UI, GDB | ⚠️ only with flashing disabled |

!!! warning "ESP32-S31 pitfall (probe-rs 0.32)"
    Before building the image, probe-rs reads the chip revision and writes it as
    the **minimum** revision in the app header. On our boards it reads v3.4 while
    they're v0.0: the bootloader refuses the app and the board boot-loops
    (`chip revision check failed. Required >= v3.4, found v0.0`).

    - **Don't**: `cargo embed` with flashing enabled, or `probe-rs run` /
      `probe-rs download` on the ELF.
    - **Fix a board**: reflash it with `cargo run` (espflash).
    - **Flash through the probe anyway**: have espflash build the image, then write
      it as a raw binary with probe-rs (see the cheat sheet).

An ESP doesn't boot the ELF directly: the ESP-IDF bootloader loads an **app image**
whose header gives the chip, the minimum revision and the segments. That's where
the pitfall above lives.

Backtraces: with `force-frame-pointers` and `esp-backtrace`, espflash decodes the
addresses into function names in the monitor.

### Logs and console

| | Link | Cost | For |
|---|---|---|---|
| `esp-println` | USB Serial/JTAG or UART0 | formatting on the MCU | interactive console, no probe needed |
| RTT (`rtt-target`) | memory read by the probe | very low, never blocks | logs while debugging, several channels, input |
| `defmt` + `defmt-rtt` | RTT, compact encoding | minimal (formatting on the PC) | lots of logs in little flash |

#### Choosing serial or RTT in this project

Two things decide where the console output (frames, stats, CLI replies) goes:

1. **At build time**, the `rtt` Cargo feature puts RTT in the firmware (an up
   channel `console`, and a down channel `cli` for typed commands). Without it, RTT
   costs nothing and everything goes to the USB serial console.
2. **At run time**, the `console` setting chooses the route:

| `console` | USB serial (`espflash monitor`) | RTT (`probe-rs attach`, `cargo embed`) |
|---|---|---|
| `auto` (default) | ✅ | ✅ if built with `rtt` |
| `serial` (aliases `usb`, `uart`) | ✅ | ✗ |
| `rtt` | ✗, prompt included (but ✅ in a build without `rtt`: the output never disappears) | ✅ |
| `both` | ✅ | ✅ if built with `rtt` |

Set it per node in `src/bin/node_*.rs`:

```rust
use esp32_rust_workshop::config::{Console, NodeConfig};

const CONFIG: NodeConfig = NodeConfig { name: "node-A", console: Console::Rtt, ..NodeConfig::DEFAULT };
```

or at run time from either console: `set console rtt`, `set console serial`,
`reset console`. The reply to `set console` is printed on both sides before
switching, so the console you leave knows where the output went.

Which to use when:

| Situation | Choice |
|---|---|
| Everyday use, one cable per board | `serial` or `auto`, `espflash monitor` / `dev.py monitor` |
| Debugging with the probe attached (GDB, `probe-rs attach`) | `rtt` build + `console rtt`: RTT writes to RAM and never blocks, even with no host reading |
| Comparing both, or logging RTT to a file while typing on USB | `rtt` build + `console both` |
| Long `burst` at high rates | `serial` or `rtt` alone: one output fewer to format per line |

### Testing a firmware

1. **Isolate the pure logic** (protocol, config, state machines) in a crate without
   a HAL, tested on the PC: that's `crates/workshop-core`.
2. **Share test vectors** to check that the MCU gives the same results as the PC,
   especially for float math.
3. **On-target tests** (HIL) with `embedded-test`: `harness = false`, the
   `embedded-test.x` script, probe-rs as runner.
4. **Validate the whole system** on the boards: here, two nodes and the stats line
   (`lost 0 · bad 0 · link UP`).

### Configuring an embedded project

| Method | When |
|---|---|
| **Typed constants validated at compile time**: `const CONFIG: NodeConfig = …` + `const _: () = CONFIG.check();` | application settings; an error fails the build, no runtime cost (this project) |
| **One binary per variant** (`src/bin/x.rs` pointing at its constant) | board or node variants, rather than exclusive features |
| **`esp-config`**: `ESP_HAL_…` variables in `[env]` of `.cargo/esp-config.toml` | settings of the esp-rs crates themselves |
| **A `build.rs` generating code from a TOML file** | powerful, but you lose type checking in the editor |

### Mixed workspace (PC and MCU)

- One Cargo invocation builds for **a single target**: `default-members` for the
  firmware, and `-p crate --target host-tuple` aliases for the PC side.
- The shared crate must stay `no_std` (`#![cfg_attr(not(test), no_std)]`) to build
  on both sides.
- `per-package-target` / `forced-target` exist, but only on nightly.

### CI

- `cargo build --release --locked` for the MCU target, `cargo clippy -- -D warnings`,
  then the tests on the PC (`cargo test-host`).
- On-target tests need a self-hosted runner with the boards plugged in, for
  example a Raspberry Pi.

---

## Cheat sheet

One command per block: copy button at the top right. Replace `/dev/cu.usbmodemXXXX`
with your port (`espflash list-ports`) and `<id>` with your probe (`probe-rs list`).

### Install

Update Rust:

```sh
rustup update stable
```

Add the ESP32-S31 target:

```sh
rustup target add riscv32imafc-unknown-none-elf
```

Install espflash:

```sh
cargo install espflash --locked
```

Install probe-rs and cargo-embed:

```sh
cargo install probe-rs-tools --locked
```

Install cargo-binutils (`cargo size`, `cargo objdump`). The `llvm-tools` they
call are installed by rustup from `rust-toolchain.toml`:

```sh
cargo install cargo-binutils --locked
```

Create the project's Python environment (`.venv` with Python 3.13, Zensical, rich, pyserial and black):

```sh
python3 scripts/dev.py setup
```

### Build, flash, monitor

Build both nodes:

```sh
cargo build --release
```

Flash node-a and open its console (with the CLI):

```sh
cargo run --release --bin node-a
```

Flash node-b and open its console:

```sh
cargo run --release --bin node-b
```

Pick the port when two boards are plugged in:

```sh
cargo run --release --bin node-a -- --port /dev/cu.usbmodemXXXX
```

List serial ports:

```sh
espflash list-ports
```

Open the console without reflashing:

```sh
espflash monitor --chip esp32s31 --port /dev/cu.usbmodemXXXX
```

Board info (chip, revision, MAC):

```sh
espflash board-info --port /dev/cu.usbmodemXXXX
```

### RTT console (probe)

Build with the RTT console:

```sh
cargo build --release --features rtt --bin node-b
```

Flash with espflash (right bootloader, revision accepted):

```sh
espflash flash --chip esp32s31 --port /dev/cu.usbmodemXXXX target/riscv32imafc-unknown-none-elf/release/node-b
```

Attach over RTT with probe-rs, without flashing; type commands at its `cli>` prompt:

```sh
probe-rs attach --chip esp32s31 --probe <id> --no-timestamps target/riscv32imafc-unknown-none-elf/release/node-b
```

cargo-embed UI (Embed.toml: flashing disabled; not tested on these boards):

```sh
cargo embed --release --features rtt --bin node-b --probe <id>
```

Then, in the board's console, send the output to RTT only:

```text
set console rtt
```

List probes:

```sh
probe-rs list
```

### Flash through the probe (revision pitfall workaround)

Build the full image with espflash:

```sh
espflash save-image --chip esp32s31 --merge --skip-padding target/riscv32imafc-unknown-none-elf/release/node-b node-b.bin
```

Write it as is at address 0:

```sh
probe-rs download --chip esp32s31 --probe <id> --binary-format bin --base-address 0 node-b.bin
```

Restart the chip:

```sh
probe-rs reset --chip esp32s31 --probe <id>
```

### Tests and quality

Core tests on the PC (unit, property tests, doctests, docs freshness):

```sh
cargo test-host
```

Regenerate the docs' reference pages:

```sh
UPDATE_DOCS=1 cargo test-host
```

Clippy as in CI (firmware):

```sh
cargo clippy --release -- -D warnings
```

Clippy as in CI (core, PC side):

```sh
cargo clippy -p workshop-core --target host-tuple --all-targets -- -D warnings
```

Build the on-target tests without running them:

```sh
cargo test --test hil --release --no-run
```

Format the code:

```sh
cargo fmt
```

### Inspection

Duplicate dependencies:

```sh
cargo tree -d
```

Who pulls in a dependency:

```sh
cargo tree -i heapless
```

Flash and RAM used, GCC-style (also printed after every `dev.py build`):

```sh
python3 scripts/dev.py size
```

Firmware size per section:

```sh
cargo size --release --bin node-a -- -A
```

What takes room:

```sh
cargo bloat --release --bin node-a
```

### Project scripts (`scripts/dev.py`)

Check the tools, the Rust target and the plugged-in boards:

```sh
python3 scripts/dev.py doctor
```

List boards (MAC, serial port, probe-rs probe, assigned node):

```sh
python3 scripts/dev.py boards
```

Say which board is node A (end of the MAC, port, or number from `boards`):

```sh
python3 scripts/dev.py boards assign a 6C:1A
```

Build, flash with espflash and open node A's console:

```sh
python3 scripts/dev.py flash a -m
```

Flash both nodes on their boards:

```sh
python3 scripts/dev.py flash all
```

Console only (backtraces decoded with the ELF):

```sh
python3 scripts/dev.py monitor a
```

RTT console: `--features rtt` build, espflash flash, then cargo-embed on the right probe:

```sh
python3 scripts/dev.py rtt a
```

Core tests on the PC (`-u` regenerates the reference pages):

```sh
python3 scripts/dev.py test host -u
```

On-target tests with node A's board probe, then the firmware put back (the test
image has the wrong minimum revision, see the probe-rs pitfall):

```sh
python3 scripts/dev.py test target a
```

Everything CI runs, locally (tests, clippy, build, site):

```sh
python3 scripts/dev.py check
```

Erase a board's whole flash:

```sh
python3 scripts/dev.py erase a
```

Preview the site locally (http://localhost:3000), reloaded on every change:

```sh
python3 scripts/dev.py docs serve --open
```

Build the site, regenerating the reference pages first:

```sh
python3 scripts/dev.py docs build --reference
```

API docs (rustdoc) of the core and the firmware:

```sh
python3 scripts/dev.py docs api --open
```

See what's outdated or missing in `.venv`, without installing anything:

```sh
python3 scripts/dev.py update --dry-run
```

Update pip and the project's Python tools:

```sh
python3 scripts/dev.py update
```

Format the Rust code and the Python scripts:

```sh
python3 scripts/dev.py format
```

New branch from an up-to-date `main`:

```sh
python3 scripts/dev.py git branch my-branch
```

Commit with a conventional type (`feat`, `fix`, `docs`…):

```sh
python3 scripts/dev.py git commit -a -t feat "message"
```

Squash-merge the current branch into `main`:

```sh
python3 scripts/dev.py git squash main
```

### Documentation

Core API docs:

```sh
cargo doc -p workshop-core --target host-tuple --open
```

Firmware API docs:

```sh
cargo doc -p esp32-rust-workshop --no-deps --open
```

### In the board's console (CLI)

| Command | Effect |
|---|---|
| `help` / `help <cmd>` | list of commands / details of a command or a key |
| `info` | show the boot card again |
| `config` | every setting, current and compiled-in |
| `set <key> <value>` / `reset [key]` | change / restore a setting |
| `set console serial\|rtt\|both\|auto` | where the console output goes |
| `period 2.5Hz` / `tx periodic 400ms` | rate of DAT frames |
| `tx on-rx` | answer every frame: the other node shows the RTT |
| `baud 1M` | change the link speed on both nodes |
| `burst` / `burst 1000 max` | measurement burst |
| `pattern ocean` / `next` / `list` | RGB LED patterns |
| `led 1 peer` / `led 2 blink 2.5Hz` | breadboard LEDs |
| `verbose` | raw frames, checksums, counters |

Keyboard: `←` `→` to edit, `↑` `↓` for history, `Home`/`End`, `Ctrl+U` to clear
the line, `Ctrl+C` to quit espflash.
