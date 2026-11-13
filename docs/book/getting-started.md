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

Pick the link: the **UART app** (three wires, 5 Mbaud) or the **Ethernet app** (the RJ45 ports, Gigabit, UDP or TCP). Both
are the same node with the same console; you can run either on the same two boards.

=== "UART"

    TX of each board to RX of the other, and the grounds together:

    | Board A | | Board B |
    |---|:-:|---|
    | `2` GPIO2 (TX) | → | `3` GPIO3 (RX) |
    | `3` GPIO3 (RX) | ← | `2` GPIO2 (TX) |
    | `G` GND | — | `G` GND |

=== "Ethernet"

    A cable between the two **RJ45** ports. A straight cat5e cable is enough (the PHYs detect the cable type), and
    there is no switch and no DHCP: the boards have static addresses, `192.168.77.11` (node A) and `192.168.77.12`
    (node B), set in their [presets](presets.md). The PHY uses GPIO5-19, so keep those free.

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

    The Ethernet app is its own Cargo workspace: run the same commands from `firmware/ethernet/`, and
    set `ESPFLASH_PORT=/dev/cu.usbmodem…` to skip the question when two boards are plugged in.

    ```sh
    cd firmware/ethernet && cargo run --release --bin node-a
    ```

=== "Ethernet with dev.py"

    The same script works for both apps; `--app eth` picks the Ethernet one, `--preset` a preset:

    ```sh
    python3 scripts/dev.py --app eth flash a -m
    python3 scripts/dev.py --app eth flash all
    python3 scripts/dev.py --app eth --preset node-a-tcp flash a   # TCP instead of UDP
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

You should see the boot card, then **one live line**, redrawn in place, with the last frame sent or received and the
counters (`tx 7 rx 7 lost 0`). `verbose 1` shows every frame on its own line and, every few seconds, the status line:

```text
─   20s ─ tx     7 · rx     7 · lost  0 · bad  0 ─ ● UP   5M
```

On Ethernet the console first says `cable connected: 1 Gbit/s · full duplex`, and `link` shows the PHY, the addresses
and the counters.
!!! warning "Link stays DOWN?"
    - TX and RX crossed? GPIO2 of one board goes to GPIO3 of the other.
    - Grounds connected?
    - Both nodes flashed with the same `shared::BAUD_RATE`?
    - `verbose wire` shows every byte received: `bad` frames point to wiring, nothing at
      all to a disconnected wire.
    - On Ethernet: `link` says `cable ● down`? Check the cable and that both boards run the Ethernet app. Both nodes
      must agree on `net.transport` and `net.port`, and each one's `net.peer` is the other's `net.address`.

## 4. Use the CLI

Type in the console; the prompt `cli ›` stays on the last line.

```text
help                  every command
pattern ocean         RGB LED pattern
period 1Hz            one DAT frame per second
verbose [0|1|2]       live line, every frame, raw frames on the wire
save                  keep the changes after a reboot
```

The [console tour](console.md) shows each part of the output, the keyboard
shortcuts and the link tests; the [CLI reference](reference/cli.md) lists every
command.

## Configure a node

Each node is a **preset**, a small TOML file that lists what makes it different:

```toml title="firmware/uart/presets/node-a.toml"
extends = "base"                # (1)!
name    = "node-A"              # (2)!

data_period_ms = 1000           # (3)!
led1           = "peer"         # (4)!
```

1. Everything not written here comes from `firmware/uart/presets/base.toml`, which spells out every
   setting with its meaning, its range and its default.
2. Sent in every frame; 1 to 12 printable characters, no `,` `*` `$`.
3. Periods are milliseconds: `400`, `12.5`… Units work too (`"2.5Hz"`, `"250ms"`).
4. The LED on GPIO20 lights while the **peer's** button is pressed.

`cargo run --release --bin node-a` turns the preset into the node's `NodeConfig` and **checks it
at compile time** (an invalid value is a build error naming the file and the key). Another preset:
`PRESET_NODE_A=my-preset cargo run --release --bin node-a`. The format is described in
[`docs/book/presets.md`](https://github.com/Lpwlk/esp32-rust-workshop/blob/main/docs/book/presets.md);
the Ethernet app uses the same files: [Ethernet app](ethernet.md).

Settings both nodes must share (the baud rate) are in `config::shared`. Cargo
features (`persist`, `watchdog`, `rtt`), saved settings and where to change what:
[Configuration](configuration.md). Code you can run on your computer:
[Examples](examples.md).
