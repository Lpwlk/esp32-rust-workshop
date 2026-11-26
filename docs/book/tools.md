# Tools and workflows

Everything you run in this repository, what it is for, and the order to run it in. The short version is in the
[README](https://github.com/Lpwlk/esp32-rust-workshop#tools-and-workflows); the cheat sheet of commands is in the
[Rust & Cargo guide](guide-rust.md#cheat-sheet).

## The Rust tools

| Tool | Installed by | What it does here | Typical command |
|---|---|---|---|
| **rustup** | <https://rustup.rs> | picks the compiler from `rust-toolchain.toml` (stable, `rust-src`, `llvm-tools`, the `riscv32imafc-unknown-none-elf` target) and installs it the first time you run `cargo` | `rustup show` |
| **cargo** | rustup | builds, runs, tests, lints; reads `.cargo/config.toml` (default target, the **runner** that flashes, the aliases below) | `cargo build --release` |
| **rustc** | rustup | the compiler. Called directly for one thing only: the ADC app's pure-arithmetic tests | `rustc --edition 2024 --test src/pwm_plan.rs` |
| **espflash** | `cargo install espflash --locked` | flashes through the board's USB Serial/JTAG and opens the serial console; it is the project's `runner`, and `save-image` makes the release images | `cargo run --release --bin node-a` |
| **probe-rs**, **cargo-embed** | `cargo install probe-rs-tools --locked` | *optional*: RTT console, debugging, on-target tests. **Attach only**: probe-rs 0.32 writes a wrong minimum chip revision into what it flashes | `cargo embed --release --features rtt`, `cargo test-target` |
| **rustfmt** | rustup component | formatting, style in `rustfmt.toml` (120 columns); CI refuses unformatted code | `cargo fmt --all` |
| **clippy** | rustup component | lints, thresholds in `.clippy.toml`; CI runs it with `-D warnings` | `cargo clippy --release -- -D warnings` |
| **rustdoc** | rustup | the API pages, linked from the site; broken intra-doc links are denied | `cargo doc -p workshop-core --target host-tuple --open` |
| **cargo-binutils** | `cargo install cargo-binutils --locked` | *optional*: `cargo size`; `scripts/memsize.py` gives the same table without it | `cargo size --release -- -A` |
| **presets tool** | the workspace (`firmware/shared/presets-tool`) | reads and validates the TOML presets | `cargo presets list`, `cargo presets check` |

### Cargo aliases (`.cargo/config.toml`)

| Alias | Runs |
|---|---|
| `cargo test-host` | the unit, property and doc tests of `workshop-core`, `ferrite` (the RTOS kernel) and `workshop-presets` **on your computer**, plus the check that the generated reference pages are up to date (`UPDATE_DOCS=1` regenerates them) |
| `cargo presets …` | the presets tool: `list`, `show <app> <name>`, `check` |
| `cargo test-target` | `firmware/uart/tests/hil.rs` on one board, through probe-rs (`PROBE_RS_PROBE=…` picks the board) |

## Four Cargo workspaces

The root workspace holds the UART app and the shared crates. The three other firmwares depend on an esp-hal that
isn't released yet (the S31's Ethernet driver, the PHY, the register crate), so each one pins it to a commit and is
**its own workspace**, with its own `Cargo.lock` and `target/`. `.cargo/config.toml` at the root still applies
to them (default target, runner).

| Folder | What | Build, run | Lint, format |
|---|---|---|---|
| repository root: `firmware/uart/`, `firmware/shared/{core,ferrite,presets-tool}` | the UART app and the hardware-free crates | `cargo run --release --bin node-a` (in `firmware/uart/`) · `cargo test-host` | `cargo clippy --release -- -D warnings` · `cargo fmt --all` |
| `firmware/ethernet/` | the Ethernet app (`--features ferrite` runs it on the Ferrite RTOS) | `cargo run --release --bin node-a` | same, in that folder |
| `firmware/adc-stream/` | the ADC app ([page](adc-stream.md)); reuses the Ethernet bring-up by path | `cargo build --release` · `dev.py flash a -m --app adc` | same, in that folder |
| `firmware/ferrite-esp32s31/` | the Ferrite port for the S31: context switch, tick, `demo` and `stress` examples ([page](ferrite.md)) | `cargo run --release --example demo` | same, in that folder |

`python3 scripts/dev.py format` formats the root workspace, the Ethernet and ADC apps and the Python; CI also checks
the Ferrite port (`cd firmware/ferrite-esp32s31 && cargo fmt`).

## The Python tools

All of them run from the `.venv` that `python3 scripts/dev.py setup` creates (`scripts/requirements.txt`: rich,
pyserial, zensical, black, numpy, plotly). `dev.py` relaunches itself there, so the system Python stays out of it.

| Tool | What it does | Run |
|---|---|---|
| [`scripts/dev.py`](https://github.com/Lpwlk/esp32-rust-workshop/blob/main/scripts/dev.py) | one front end for the rest: boards by MAC address, build, flash, monitor, tests, `check`, docs, git helpers. `--app eth\|uart\|adc` picks the firmware. Logs in `scripts/logs/dev.log` | `python3 scripts/dev.py -h` |
| `scripts/boards.py`, `.boards.json` | finds the plugged boards (pyserial) and remembers which one is node A or B | `dev.py boards`, `dev.py boards assign a 6C:1A` |
| `scripts/memsize.py` | Flash / RAM table of an ELF (CI shows it in the job summary and compares with the target branch) | `dev.py size`, automatic after `build` |
| **Zensical** | builds this site from `docs/book/` and `docs/zensical.toml` | `dev.py docs serve --open`, `dev.py docs build -r` |
| **black** | formats the Python (`--line-length 100`) | `dev.py format` |
| [`tools/adc-fft/`](adc-stream.md) | the ADC app's live page (spectrum, waveform, waterfall, filter, PWM generator); **keeps your settings** between runs | `dev.py fft`, `dev.py fft --demo` |
| [`tools/node-dash/`](console.md) | a page around the consoles of both nodes (Ethernet or UART app): every CLI command as a control, charts read from what the boards print. Nothing changes in the firmware | `dev.py dash`, `dev.py dash --demo [uart]` |
| `tools/wireshark/` | Lua dissectors for what the boards put on the cable, see below | `tshark -X lua_script:tools/wireshark/workshop.lua -r capture.pcap` |
| `scripts/record_demos.py`, `render_demos.py`, `record.py`, `gifrender.py` | record the console GIFs from two real boards (`brew install agg gifsicle`), or re-render saved recordings without a board | `python3 scripts/record_demos.py boot` |

### Wireshark

`tools/wireshark/workshop.lua` decodes `wsnode` (the node and address space frames, UDP and TCP 7000), `wsgen`
(the traffic generator, UDP 7001) and `wsadc` (the ADC stream: samples, `LOG1`/`RPL1` lines, commands, UDP 7100).
Install: `ln -s "$PWD/tools/wireshark/workshop.lua" ~/.local/lib/wireshark/plugins/workshop.lua`, then restart
Wireshark. `python3 tools/wireshark/make_samples.py` writes two sample captures and
`python3 tools/wireshark/check.py` runs `tshark` on them and fails when the Lua tables no longer match the Rust
sources (a node, a setting, a status, the ADC header). Details: `tools/wireshark/README.md`.

## Workflows

Every command below is written `dev.py …` for short. The command always comes first and its options after it, `--app`
included: `dev flash all --app eth --rtt`, never `dev --app eth flash all`. The only requirement is a released
**Python 3.13 or newer** (the first run creates the project's `.venv` by itself, once).

To type `dev` instead of `python3 scripts/dev.py`, install it once, from the repository (the install is a link to the
repository, so `git pull` updates it):

```sh
pipx install --editable .                     # needs pipx; or: uv tool install --editable .
dev doctor                                    # from anywhere
```

On macOS and Linux the launcher works too, without pipx: `ln -s "$PWD/scripts/dev" ~/.local/bin/dev`. It follows the
link back to the repository, and uses the `.venv`'s Python (before it exists, the first Python 3.13 or newer it finds).
It is a shell script: on Windows use the pipx install, or `python scripts\dev.py`.

`-v` (or `-vv`) after `build`, `flash`, `watch`, `rtt`, `test`, `check` or `docs build|api` is cargo's own:
it prints the `rustc` and linker command lines, to see what a build really runs, and shows cargo's output live
instead of the bar below.

### What you see

While a command runs, a progress bar sits at the bottom of the terminal: a spinner, a pulsing bar (nobody knows how long a
build takes, so there is no percentage), the elapsed time, and the line the command is printing. When the command ends the
bar goes and a log line takes its place, so the screen above is a record of what ran: the time, the result (`DONE`,
`WARNING` or `ERROR`), what it did, how long it took, and the command itself on the next line.

```text
[14:03:21] DONE     build node-a, node-b  5.8 s  1 crate compiled
                    $ cargo build --release --bin node-a --bin node-b
[14:03:36] DONE     flash node A → 21:FE  14.2 s
                    $ espflash flash --chip esp32s31 --port /dev/cu.usbmodem11201 …
[14:03:38] ERROR    3/15 clippy, firmware  1.4 s  exit 101
                    $ cargo clippy --release -- -D warnings
```

A failing command prints its whole output, in color, under its log line (so does a command with warnings).
`scripts/logs/dev.log` keeps the same lines with the full output of every command, and `-v` shows cargo's own output
instead of the bar. `flash all` has one bar per board, side by side, each replaced by its own log line when its board is
done. The console commands (`monitor`, `flash -m`, `rtt`) and the servers (`fft`, `dash`, `docs serve`) are not wrapped:
they keep the terminal. Piped (no terminal), there is no bar: only the log lines.

The colors are a theme, chosen in the Python files, not on the command line: set `ACTIVE_THEME` in `scripts/devthemes.py`
to `ocean` (the default), `forest`, `sunset`, `dracula`, `nord` or `mono` (no colors, only bold and dim), or add a theme
there by copying one and changing its colors.

### Every day

| I want to… | Do |
|---|---|
| set up a computer | `rustup`, `cargo install espflash --locked`, `python3 scripts/dev.py setup`, `python3 scripts/dev.py doctor` (it lists what is missing, with the command) |
| tell the boards apart | `dev.py boards assign a 6C:1A` (the end of the MAC), once per board |
| flash and watch a node | `dev.py flash a -m` (Ethernet), `dev.py flash a -m --app uart`, `dev.py flash a -m --app adc` |
| flash both | `dev.py flash all` (one build for both nodes, then both boards are flashed at the same time) |
| rebuild every time I save | `dev.py watch` (both nodes), `dev.py watch a --flash` (also reflash node A after each good build) |
| restart a board | `dev.py reset a` (no build, no flashing) |
| forget the saved settings | `dev.py erase a --settings` (only the `nvs` partition; `dev.py erase a` wipes the whole flash) |
| look at the console again | `dev.py monitor a` (backtraces decoded); one program per serial port |
| debug with a probe | `dev.py rtt a` (RTT build, espflash, then cargo-embed) |
| try a preset | `PRESET_NODE_A=fast cargo run --release --bin node-a`, or `dev.py flash a --preset fast` |
| test the logic without a board | `cargo test-host`, or `dev.py test host` (`-u` regenerates the reference pages) |
| test on a board | `dev.py test target a` (then it reflashes the firmware) |
| see a live ADC spectrum | `dev.py fft` (or `--demo`) |
| watch both nodes on a page | `dev.py dash` (or `--demo`) |
| change the ADC page's settings back | the *Reset all* button, or `dev.py fft --reset-settings` |

### Before you push

```sh
python3 scripts/dev.py format     # cargo fmt (root, Ethernet, ADC) + black
python3 scripts/dev.py check      # what CI runs, in its order, stopping at the first failure
```

`check` ends with the time of each step. While you iterate, `check --fast` leaves out the three firmware builds (the slow
fat-LTO part; clippy still type-checks the same code) and the site, and only a link error escapes it.

Builds and flashes print how long they took. A command that ends by itself after 30 s or more (build, flash, check, test)
also raises a desktop notification; `DEV_NOTIFY=0` turns that off.

`check` runs: host tests and the freshness of the reference pages · clippy on the core · every example · the
presets · build and clippy of the UART, Ethernet and ADC firmwares · the PWM arithmetic tests · the self tests of
`adc_fft.py` and `node_dash.py` · the site. Not in it, because they need a board: `test target`, the
system test (`flash all`).

### When you change…

See the checklist in [Formatting and generated docs](automation.md#checklist-when-you-change). For the ADC page
(`tools/adc-fft/`): `python3 tools/adc-fft/adc_fft.py --selftest` is the test, and a new setting that should
survive a restart goes either into the analyzer's settings (`Analyzer.s`, saved by `/api/config`) or into the
page's `ui` object (saved by `/api/prefs`).

### In CI (`.github/workflows/ci.yml`)

| Job | Checks |
|---|---|
| **format** | `cargo fmt --check` in the root, `firmware/ethernet`, `firmware/adc-stream` and `firmware/ferrite-esp32s31` |
| **host-tests** | `cargo test-host --locked`, the examples, clippy (core, Ferrite, presets), `cargo presets check` |
| **ferrite-port** | the S31 port and its examples build and lint |
| **firmware-eth** | the Ethernet app, for default / none / all / `ferrite` features, UDP and TCP presets |
| **firmware-adc** | the ADC app builds and lints; the PWM arithmetic tests run |
| **adc-fft**, **node-dash** | the `--selftest` of each Python tool |
| **firmware** | the UART app for four feature sets, the Flash / RAM table, the ELF files kept 30 days (push to `main`) |
| **site** → **deploy** | Zensical and rustdoc, published on GitHub Pages from `main` |

`release.yml` runs on a `v*` tag: see [Formatting and generated docs](automation.md#releases). The on-target
tests never run in CI (no board there).

### Documentation

`dev.py docs serve --open` previews the site with live reload. The reference pages (`docs/book/reference/`) are
**generated from the code** and CI fails if they are stale: `UPDATE_DOCS=1 cargo test-host`, or
`dev.py docs build -r`, regenerates them. Hand-written pages are in `docs/book/`, the menu is `nav` in
`docs/zensical.toml`.
