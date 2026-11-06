# Getting started

## Toolchain

The project builds on **stable Rust** (1.95 or newer) for the RISC-V target of the
ESP32-S31. `rust-toolchain.toml` selects the channel and the target, so `rustup`
installs them on the first build.

```sh
rustup update stable
cargo install espflash --locked     # flash + serial monitor
cargo install probe-rs-tools --locked   # on-target tests and debugging (optional)
```

## Project scripts: `scripts/dev.py`

One script for the daily tasks: boards, firmware, tests, docs. It needs Python
3.12-3.14 (not 3.15, whose alpha breaks pip and Zensical's dependencies), sets up
a `.venv` and runs everything inside it:

```sh
python3 scripts/dev.py setup     # once: .venv + scripts/requirements.txt
python3 scripts/dev.py doctor    # tools, Rust target, boards: what's missing
python3 scripts/dev.py -h        # every command
```

## Build, flash, monitor

Plug each board by its **USB Serial/JTAG** USB-C port. Each one is found by its MAC
address, which is both its serial port and its debug probe. Tell the script once
which board is which node:

```sh
python3 scripts/dev.py boards              # MAC, port and probe of each board
python3 scripts/dev.py boards assign a 6C:1A
python3 scripts/dev.py boards assign b 21:FE
```

Then:

```sh
python3 scripts/dev.py flash a -m    # build, flash node A, open its console
python3 scripts/dev.py flash b -m    # node B (other terminal)
python3 scripts/dev.py flash all     # both, no console
python3 scripts/dev.py monitor a     # console only, backtraces decoded
```

The assignment is kept in `.boards.json` (not committed: it describes your desk).
With a single board plugged in, no assignment is needed. `--board <MAC end>`
overrides it for one command.

??? note "Without the script: `cargo run`"
    `cargo run` builds, flashes with `espflash` and opens the console:

    ```sh
    cargo run --release --bin node-a
    espflash list-ports                                   # two boards: choose the port
    cargo run --release --bin node-a -- --port /dev/cu.usbmodem11301
    ```

The console shows a boot card (link, node config, build, chip, LEDs), then one line
per frame sent (`TX ▶`) or received (`RX ◀`), in fixed columns, and a stats line
with the link state:

```text
    12.156 RX ◀  DAT  #00003 node-B   btn ○ released  ev     0  Δ  2142.844 ms ← TX
    12.513 TX ▶  DAT  #00004          btn ○ released  ev     0  Δ   357.206 ms ← RX
────   20s ── tx 7 · rx 7 · lost 0 · bad 0 ── ● link UP   ────
```

`Δ` is the time since the previous `DAT`, sent or received, measured on the wire
([Timing on the wire](architecture.md#timing-on-the-wire)). The sender's uptime and
the raw frame are shown with `verbose`.

### Serial console or RTT

Output goes to the USB serial console by default. To read it through the debug
probe (RTT) instead, build with `--features rtt` (`python3 scripts/dev.py rtt a`
does it all) and choose the route with the `console` setting: `auto` (default),
`serial`, `rtt` or `both`, per node in `src/bin/node_*.rs` or live with
`set console rtt`. Details and when to use which:
[Rust & Cargo guide, Logs and console](guide-rust.md#logs-and-console).

## Using the CLI

Type in the console: the prompt `cli ›` stays on the last line.

| Keys | Action |
|---|---|
| ← → | move in the line; typing inserts at the cursor |
| ↑ ↓ | previous / next command (↓ at the bottom: back to what you were typing) |
| Home / End, Ctrl+A / Ctrl+E | start / end of the line |
| Backspace / Delete | erase before / under the cursor |
| Ctrl+U / Ctrl+K | clear the line / erase to its end |
| Ctrl+C | quit `espflash monitor` |

```text
help                  list of commands
help set              details on one command
info                  the boot card again (current settings)
config                every setting, current and build value
set data_period_ms 1000
pattern ocean
led 1 blink 200
verbose               raw frames, checksums, byte counters
baud 1M               switch both nodes to 1 Mbaud (handshake)
tx on-rx              reply to each peer frame: the peer shows the round-trip time
burst                 asks the burst length and speed, then measures the link
burst 1000 max        same, in one line
```

See the [CLI reference](reference/cli.md) and the
[settings reference](reference/config.md).

## Configuring a node

Each binary is just its configuration, in `src/bin/node_a.rs` and
`src/bin/node_b.rs`:

```rust
const CONFIG: NodeConfig = NodeConfig {
    name: "node-A",
    data_period_ms: 1000.0,
    led1: LedMode::PeerButton,
    ..NodeConfig::DEFAULT
};
esp32_rust_workshop::node_firmware!(CONFIG);
```

Invalid values **fail the build** with a message (`config: a pin is reserved...`).
Settings that both nodes must share, like the baud rate, are in `config::shared`.

Changes made live with `set` can be kept across reboots with `save` (and erased
with `forget`). Build features, saved settings, shared settings and where to change
what: [Configuration](configuration.md).
