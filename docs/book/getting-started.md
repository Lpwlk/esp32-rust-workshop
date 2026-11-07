# Getting started

From nothing to two boards talking, in four steps.

<div class="asciicast" data-cast="../assets/demo/boot.cast" data-poster="npt:0:12"></div>
<p class="asciicast-caption">Node A after a reset: the boot card, then frames exchanged with node B · recorded on the boards, <a href="../assets/demo/boot.cast">.cast</a></p>

## 1. Install the tools

The project builds on **stable Rust** (1.95 or newer) for the ESP32-S31's RISC-V
target. `rust-toolchain.toml` pins the channel, the target and the components, so
`rustup` installs them on the first `cargo` command.

=== "macOS / Linux"

    ```sh
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # if Rust isn't installed
    cargo install espflash --locked            # flash + serial monitor (required)
    cargo install probe-rs-tools --locked      # RTT and debugging (optional)
    cargo install cargo-binutils --locked      # cargo size (optional)
    ```

=== "Windows"

    ```powershell
    winget install Rustlang.Rustup             # if Rust isn't installed
    cargo install espflash --locked
    cargo install probe-rs-tools --locked
    ```

=== "Project scripts"

    `scripts/dev.py` wraps the daily tasks (boards, flashing, tests, docs). It needs
    Python 3.12 to 3.14 and sets up its own `.venv`:

    ```sh
    python3 scripts/dev.py setup     # once: .venv + scripts/requirements.txt
    python3 scripts/dev.py doctor    # tools, Rust target, boards: what's missing
    python3 scripts/dev.py -h        # every command
    ```

!!! tip "Check everything at once"
    `python3 scripts/dev.py doctor` lists what's installed, what's missing and
    which boards are plugged in, with the command to fix each problem.

## 2. Wire the boards

TX of each board to RX of the other, and the grounds together:

| Board A | | Board B |
|---|:-:|---|
| `2` GPIO2 (TX) | → | `3` GPIO3 (RX) |
| `3` GPIO3 (RX) | ← | `2` GPIO2 (TX) |
| `G` GND | — | `G` GND |

The button is optional: a jumper from `4` to `G` on either board, or simply the
board's **BOOT** button. Details, LEDs and pins to avoid: [Wiring](wiring.md).

## 3. Flash and open the console

Plug each board by its **USB Serial/JTAG** USB-C port.

=== "dev.py"

    Each board is found by its MAC address. Tell the script once which one is which:

    ```sh
    python3 scripts/dev.py boards                  # MAC, port and probe of each board
    python3 scripts/dev.py boards assign a 6C:1A   # this board is node A
    python3 scripts/dev.py boards assign b 21:FE
    ```

    Then:

    ```sh
    python3 scripts/dev.py flash a -m    # build, flash node A, open its console
    python3 scripts/dev.py flash b -m    # node B, in another terminal
    python3 scripts/dev.py flash all     # both, no console
    python3 scripts/dev.py monitor a     # console only, backtraces decoded
    ```

    The assignment is kept in `.boards.json` (not committed: it describes your desk).
    After each build, the script prints the flash and RAM used.

=== "cargo run"

    `cargo run` builds, flashes with espflash and opens the console (the `runner`
    of `.cargo/config.toml`):

    ```sh
    cargo run --release --bin node-a
    cargo run --release --bin node-b -- --port /dev/cu.usbmodem11301   # two boards: pick the port
    ```

=== "espflash"

    ```sh
    cargo build --release
    espflash flash --chip esp32s31 --monitor --port /dev/cu.usbmodem11201 \
        target/riscv32imafc-unknown-none-elf/release/node-a
    ```

=== "A release"

    No Rust needed: download `node-a.elf` and `node-b.elf` from the
    [latest release](https://github.com/Lpwlk/esp32-rust-workshop/releases), then:

    ```sh
    espflash flash --chip esp32s31 --monitor node-a.elf
    ```

You should see the boot card, then one line per frame and, every few seconds:

```text
────   20s ── tx 7 · rx 7 · lost 0 · bad 0 ── ● link UP   ────
```

!!! warning "Link stays DOWN?"
    - TX and RX crossed? GPIO2 of one board goes to GPIO3 of the other.
    - Grounds connected?
    - Both nodes flashed with the same `shared::BAUD_RATE`?
    - `verbose` shows every byte received: `bad` frames point to wiring, nothing at
      all to a disconnected wire.

## 4. Use the CLI

Type in the console; the prompt `cli ›` stays on the last line.

```text
help                  every command
pattern ocean         RGB LED pattern
period 1Hz            one DAT frame per second
verbose               raw frames on the wire
save                  keep the changes after a reboot
```

The [console tour](console.md) shows each part of the output, the keyboard
shortcuts and the link tests; the [CLI reference](reference/cli.md) lists every
command.

## Configure a node

Each binary is its configuration plus one macro:

```rust title="src/bin/node_a.rs"
use esp32_rust_workshop::config::{LedMode, NodeConfig};

const CONFIG: NodeConfig = NodeConfig {
    name: "node-A",                 // (1)!
    data_period_ms: 1000.0,         // (2)!
    led1: LedMode::PeerButton,      // (3)!
    ..NodeConfig::DEFAULT           // (4)!
};

esp32_rust_workshop::node_firmware!(CONFIG); // (5)!
```

1. Sent in every frame; 1 to 12 printable characters, no `,` `*` `$`.
2. Periods are `f32` milliseconds: `400.0`, `12.5`… The CLI also accepts
   frequencies (`period 2.5Hz`).
3. The LED on GPIO20 lights while the **peer's** button is pressed.
4. Every field not named here keeps its default: a node file only lists what makes
   that node different.
5. Expands to the whole firmware: `const _: () = CONFIG.check();` (an invalid value
   is a **build error**), the app descriptor the bootloader needs, the panic
   handler and `main`.

Settings both nodes must share (the baud rate) are in `config::shared`. Cargo
features (`persist`, `watchdog`, `rtt`), saved settings and where to change what:
[Configuration](configuration.md). Code you can run on your computer:
[Examples](examples.md).
