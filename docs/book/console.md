---
hide:
  - toc
---

# The console

Everything the firmware does shows up on its console: over the **USB serial**
port (`espflash monitor`) by default, or through the debug probe (**RTT**). Same
text, same colors. The recordings on this page are real sessions on two
ESP32-S31-Function-CoreBoard-1 boards (node A shown, node B running alongside),
captured with `scripts/record.py`. They are text, not videos: pause them, seek
with the progress bar, or select and copy a line.

## Boot

<div class="asciicast" data-cast="../assets/demo/boot.cast" data-poster="npt:0:12"></div>
<p class="asciicast-caption">Node A after a reset: the boot card, then frames exchanged with node B · recorded on the boards, <a href="../assets/demo/boot.cast">.cast</a></p>

After the ROM and the ESP-IDF bootloader, the firmware prints its **boot card**:
everything that defines this node, in one screen.

| Section | What it tells you |
|---|---|
| **link** | what both nodes must agree on: UART, baud rate, frame format |
| **node** | this board's config: name, pins, periods, button filter |
| **build** | firmware version, git commit (`+dirty` if built from uncommitted changes), date, profile, rustc, crate versions |
| **chip** | silicon revision, CPU clock, MAC, reset reason, three TRNG samples |
| **rgb led**, **breadboard leds** | current pattern (with a color preview) and LED modes |
| **system** | Cargo features compiled in, settings loaded from flash, watchdog, console route |

!!! tip "Seen it scroll by too fast?"
    Type `info`: the card again, with the **current** settings (those changed with
    `set` included).

## Frame lines

One line per frame, sent (`TX ▶`) or received (`RX ◀`), in fixed columns:

```text
    12.156 RX ◀  DAT  #00003 node-B   btn ○ released  event     0  Δ  2142.844 ms ← TX
    12.513 TX ▶  DAT  #00004          btn ○ released  event     0  Δ   357.206 ms ← RX
    14.002 TX ▶  EVT  #00005          ▼ press    key 7A3F09C2E61B44D0  BOOT
────   20s ── tx 7 · rx 7 · lost 0 · bad 0 ── ● link UP   ────
```

| Column | Meaning |
|---|---|
| `12.156` | this node's uptime, s |
| `TX ▶` / `RX ◀` | sent / received |
| `DAT` `EVT` `BAU` `ECH` | frame type ([protocol](reference/protocol.md)) |
| `#00003` | sequence number: gaps are counted as `lost` |
| `node-B` | sender (empty for our own frames) |
| `btn` `event` | the sender's button state and how many `EVT` it has sent |
| `▼ press` · `key` · `BOOT` | an `EVT`: which edge, its random 64-bit key, which button (`jumper` or `BOOT`) |
| `Δ 357.206 ms ← RX` | time since the previous `DAT`, **sent or received**, measured on the wire (µs resolution): here 357 ms after the last frame received |

The stats line comes every `status_period_ms`: frames sent and received, lost
(sequence gaps), bad (checksum or format), and the link state.

??? info "How is `Δ` measured so precisely?"
    The UART interrupt timestamps each received `\n`, then corrects for the bytes
    read after it and for the idle timeout that triggered it; a sent frame's end is
    its queuing time plus its length on the wire. Both directions share one clock,
    so with both nodes at 2.5 s, TX→RX and RX→TX add up to 2500 ms. Details:
    [Timing on the wire](architecture.md#timing-on-the-wire).

## The CLI

<div class="asciicast" data-cast="../assets/demo/cli.cast" data-poster="npt:0:7"></div>
<p class="asciicast-caption"><code>help</code>, <code>help period</code>, <code>pattern ocean</code>, <code>period 4Hz</code> (Δ drops to 250 ms), <code>verbose</code> (raw frames on the wire) · recorded on the boards, <a href="../assets/demo/cli.cast">.cast</a></p>

The prompt `cli ›` stays on the last line while frames scroll above it. It edits
like a shell:

| Keys | Action |
|---|---|
| ++arrow-left++ ++arrow-right++ | move in the line; typing inserts at the cursor |
| ++arrow-up++ ++arrow-down++ | history (↓ at the bottom: back to what you were typing) |
| ++home++ ++end++, ++ctrl+a++ ++ctrl+e++ | start / end of the line |
| ++backspace++ ++del++ | erase before / under the cursor |
| ++ctrl+u++ ++ctrl+k++ | clear the line / erase to its end |
| ++ctrl+r++ / ++ctrl+c++ | reset the chip / quit (`espflash monitor`) |

=== "Settings"

    ```text
    config                    every setting: live value and build value
    set data_period_ms 1000   or a frequency: set data_period_ms 4Hz
    period 2.5Hz              shortcut for the same setting
    reset data_period_ms      back to the build value (reset alone: everything)
    save                      keep the changes across reboots (flash)
    forget                    erase what was saved
    ```

=== "LEDs"

    ```text
    list                      RGB patterns, with a color preview
    pattern ocean             or its number: pattern 3
    bright 70                 RGB brightness, %
    led 1 blink 2.5Hz         breadboard LED 1
    led 2 peer                LED 2 follows the peer's button
    ```

=== "Link tests"

    ```text
    baud 1M                   both nodes switch together (handshake, auto fallback)
    tx on-rx                  answer every frame: the peer shows the round-trip time
    burst                     asks the length and the rate, then measures
    burst 1000 max            same, in one line
    ```

=== "Help"

    ```text
    help                      every command, grouped
    help set                  one command: usage, details, examples
    help rgb.cycle_ms         one setting: meaning, accepted values, default
    verbose                   raw frames, checksums, byte counters
    ```

Every command, with its examples (each one is checked by a unit test):
[CLI reference](reference/cli.md).

## Link tests

<div class="asciicast" data-cast="../assets/demo/link.cast" data-poster="npt:0:10"></div>
<p class="asciicast-caption"><code>baud 1M</code> confirmed by both nodes, <code>burst 500 max</code>, then back to 5 Mbaud · recorded on the boards, <a href="../assets/demo/link.cast">.cast</a></p>

- **`baud 1M`**: node A asks (`BAU REQ`), node B answers (`ACK`) and both
  switch; the first `DAT` received at the new speed confirms it. Without an
  answer, both fall back to the boot speed.
- **`burst 500 max`**: 500 frames back to back. The receiving side counts
  them, the losses and the bad checksums, and both sides print the throughput and
  the share of the line used.

!!! example "Try it"
    On node B, `tx on-rx`: node A then shows `rtt` on every `ECH` reply, and
    min / avg / max round-trip times in its stats line.

## RTT, through the debug probe

<div class="asciicast" data-cast="../assets/demo/rtt.cast" data-poster="npt:0:9"></div>
<p class="asciicast-caption"><code>probe-rs attach</code>: <code>info</code> and <code>pattern aurora</code> typed at probe-rs's <code>cli></code> prompt, answered over RTT · recorded on the boards, <a href="../assets/demo/rtt.cast">.cast</a></p>

Built with `--features rtt`, the firmware also writes its console into RAM
buffers that the probe reads in the background: no serial port, and it never
blocks even when nobody reads. It works both ways: `probe-rs attach` shows a
`cli>` prompt (the name of the firmware's input channel), and what you type there
goes to the same CLI. In the recording, `info` and `pattern aurora` are typed in
probe-rs.

!!! note "The first lines arrive all at once"
    The firmware writes into a 4 KB buffer from boot, probe or not. When probe-rs
    attaches, it first prints what the buffer still holds (the boot card is long
    gone: type `info`), then the live output.

=== "dev.py"

    ```sh
    python3 scripts/dev.py rtt a      # RTT build, espflash flash, then cargo-embed
    ```

=== "probe-rs"

    ```sh
    cargo build --release --features rtt --bin node-a
    espflash flash --chip esp32s31 target/riscv32imafc-unknown-none-elf/release/node-a
    probe-rs attach --chip esp32s31 --probe <id> --no-timestamps target/riscv32imafc-unknown-none-elf/release/node-a   # cli> prompt: type commands
    ```

=== "cargo-embed"

    ```sh
    # firmware/uart/Embed.toml: RTT console, flashing disabled (see the warning below)
    cargo embed --release --features rtt --bin node-a --probe <id>
    ```

!!! warning "Flash with espflash, attach with probe-rs"
    probe-rs 0.32 writes a wrong minimum chip revision into the images it flashes
    on these boards, which then boot-loop. Flash with espflash first; use probe-rs
    and cargo-embed only to attach. [Why](guide-rust.md#flash-run-debug).

`set console serial|rtt|both|auto` chooses where the output goes at run time:
[Logs and console](guide-rust.md#logs-and-console).

## Recording a session

```sh
brew install agg gifsicle    # asciicast → GIF, then GIF compression
python3 scripts/record.py docs/book/assets/demo/cli.cast --gif --seconds 30 --start 4.5 \
    --key '0.8:\x12' --type 6:"help" --type 15:"pattern ocean" \
    -- espflash monitor --chip esp32s31 --port /dev/cu.usbmodemXXXX
```

`scripts/record.py` runs the command in a pseudo-terminal (so colors are kept),
types the `--type` commands one key at a time, and writes an asciicast that
[agg](https://github.com/asciinema/agg) renders and gifsicle compresses.

- `--key '0.8:\x12'` sends ++ctrl+r++: `espflash monitor` doesn't reset the chip
  by itself, and shows nothing until it does;
- `--start 4.5` drops the boot from the recording, `--trim REGEX` drops lines
  (the ROM and bootloader logs);
- record from a committed tree, or the boot card shows `+dirty`. The `.cast` files
  are kept next to the GIFs (`docs/book/assets/demo/`), to re-render them with
  other settings.
