---
hide:
  - toc
---

# The console

Everything the firmware does shows up on its console: over the **USB serial**
port (`espflash monitor`) by default, or through the debug probe (**RTT**). Same
text, same colors, for the **UART app** and the **Ethernet app**: they share the
console, the CLI and the settings, and only what concerns the link differs
(`baud` on UART, `link` on Ethernet). The recordings on this page are real sessions on two
ESP32-S31-Function-CoreBoard-1 boards (node A shown, node B running alongside),
captured with `scripts/record.py`. They are text, not videos: pause them, seek
with the progress bar, or select and copy a line. The README shows the Ethernet
app in GIFs (`scripts/record_demos.py`).

## Boot

<div class="asciicast" data-cast="../assets/demo/boot.cast" data-poster="npt:0:12"></div>
<p class="asciicast-caption">Node A after a reset: the boot card, then frames exchanged with node B · recorded on the boards, <a href="../assets/demo/boot.cast">.cast</a></p>

After the ROM and the ESP-IDF bootloader, the firmware prints its **boot card**:
everything that defines this node, in one screen.

| Section | What it tells you |
|---|---|
| **link** | what both nodes must agree on: UART and baud rate, or Ethernet (transport, port, the PHY and its speed), and the frame format |
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
    12.156 RX ◀  DAT  #00003 node-B   │ btn ○ released  event     0 │ Δ  2142.844 ms ← TX
    12.513 TX ▶  DAT  #00004          │ btn ○ released  event     0 │ Δ   357.206 ms ← RX
    14.002 TX ▶  EVT  #00005          │ ▼ press    key 7A3F09C2E61B44D0 │ BOOT
─   20s ─ tx     7 · rx     7 · lost  0 · bad  0 ─ ● UP   5M
```

| Column | Meaning |
|---|---|
| `│` | separates the fixed columns (time, direction, type, number, sender) from what the frame says, and that from the measurements (`Δ`, `rtt`, button): a column that is always the same width gets a bar, so the eye can skip it |
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
    baud 1M                   UART: both nodes switch together (handshake, auto fallback)
    link                      Ethernet: cable, negotiated speed, addresses, transport, counters
    tx on-rx                  answer every frame: the peer shows the round-trip time
    burst                     asks the length and the rate, then measures
    burst 1000 max            same, in one line
    ```

=== "Peer's address space"

    ```text
    browse [path] [depth]     the peer's address space as a tree: `browse / 2` is all of it
    read /buttons/jumper      one of its variables
    write /leds/1 blink 200   write one (checked like its own `set`)
    watch /buttons/jumper     the peer reports it whenever it changes
    unwatch                   stop watching
    ```

    These are requests to the other board, over the same link: [Protocols](protocols.md#the-address-space-ua-lite).

=== "Help"

    ```text
    help                      every command, grouped
    help set                  one command: usage, details, examples
    help rgb.cycle_ms         one setting: meaning, accepted values, default
    verbose [0|1|2|3]         console detail: nothing, live line, every frame, or the details behind each
    ```

`help` prints its sections (settings, shortcuts, link tests, ...) one after the other, `HELP_PAUSE_MS` apart
(`firmware/shared/core/src/cli.rs`; set it to 0 to print them as fast as the console allows), so the list can be read
as it appears; the node keeps running meanwhile (`workshop_core::cli::HelpPager`: the pause never blocks the main
loop or the network).

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

## How much the console prints: `verbose`

A frame every couple of seconds scrolls fast, so the console has three levels (`verbose`, alone it goes to the
next one; also a key of the [presets](presets.md)):

| Level | `verbose` | What you see |
|---|---|---|
| 0 | `live` (default) | **one line**, updated in place just above the prompt: the last frame sent or received, with the `tx` / `rx` / `lost` counters. Lost or bad frames, link up / down, bursts and errors still scroll as lines, and so does everything you type |
| 1 | `frames` | every frame on its own line, and the periodic stats line |
| 2 | `wire` | frames plus the raw line, checksum and size of each, the time since the peer's previous frame, ignored button bounces and the link counters |

The live line is only on the USB console: an RTT-only console (`set console rtt`) has no prompt to keep it
under, so it gets every line. `silent` (0) is for when the console must stay clean: a burst measurement, a
recording, a wizard. It still answers what you type and still reports what is wrong. `verbose` alone walks
the four levels and wraps round: `live` → `frames` → `wire` → `silent` → `live`, so one key reaches all of them.

Spellings: `verbose off` is `silent`, `verbose quiet` is `live`, `verbose on` is `wire`. The levels were numbered
0 live, 1 frames, 2 wire before `silent` took the 0: a preset or a habit that says `verbose 0` now means silent, and
`verbose = 1` means live (the names did not change, so a preset that spells them is fine).

### `verbose` and `trace`: who shows what

The two commands cut the console along different lines. `verbose` is about **the node**: its frames, its counters,
what it measures. `trace` is about **the cable**: every Ethernet frame as it crossed the port. Nothing is
printed twice:

| You want | Ethernet app | UART app |
|---|---|---|
| the frames the node sent and received | `verbose frames` | `verbose frames` |
| the time since the peer's previous frame, bounces, counters | `verbose wire` | `verbose wire` |
| the raw line, its checksum and size | `trace frames` (the `app` line and the hex dump) | `verbose wire` |
| ARP, TCP handshake, ICMP, headers, checksums | `trace events` / `trace frames` | (not on a UART link) |

On Ethernet `verbose wire` no longer prints the raw line: it is in the trace, next to the headers it travelled
in, so you read it once and with its envelope.

## The network frames: `trace` (Ethernet app)

`verbose` shows the node's own frames (`DAT`, `EVT`...). `trace` shows what crosses the RJ45 port: every
Ethernet frame, decoded, including what the node never sees as a frame: ARP, the TCP handshake, ICMP. It is
independent of `verbose`; alone it goes to the next level (`t` and `tr` are short for it, `d` for `dump`).

| Level | `trace` | What you see |
|---|---|---|
| 0 | `off` (default) | nothing. The driver doesn't even copy the frames |
| 1 | `events` | one line per **control** frame: ARP, ICMP, TCP `SYN` / `FIN` / `RST`, and anything that can't be decoded or has a bad checksum |
| 2 | `frames` | every frame: a header line, then a line per layer (Ethernet, IPv4, UDP or TCP), the node's own frames inside, and the first `dump` bytes of the raw frame |

`dump <0-256>` (the `trace_bytes` setting) chooses how many raw bytes are shown, 32 by default, 0 for none. A
frame is kept up to 256 bytes, enough for the headers of any frame (at most 134).

### The view: `trace compact`, `trace layers`, `trace inside-out`

The level says **which** frames; the view (`trace_view`) says **how** one is laid out. Pick it at run time, it
changes the next frame:

| View | What a frame looks like |
|---|---|
| `compact` | one line: time, direction, protocol, `from → to`, what it is, and the node's own frame at the end. No layers, no dump. Made for a busy link, see below |
| `layers` (default) | that line, then a line per layer **from the cable inwards** (Ethernet, IPv4, UDP or TCP), then the node's own frames, then the hex dump |
| `inside-out` | that line, then the node's own frames **first** and the layers back out to the cable, then the hex dump: what you were looking for first, its envelope after |

`trace events` always uses the one-line form, whatever the view. In every view the bars `│` separate columns that
have a fixed width (the label of a layer, the offset, the bytes, the characters), never free text:

```text
   12.351 TX ▶  UDP │ 192.168.77.11:7000 → 192.168.77.12:7000   │ 78 B  payload 36 B
           eth  │ 02:00:00:00:00:11 → 02:00:00:00:00:12  type 0x0800 (IPv4)
           ipv4 │ 192.168.77.11 → 192.168.77.12  ttl 64  proto 17 (UDP)  total 64  id 0x0000  DF  cs ok
           udp  │ 7000 → 7000  len 44 (payload 36 B)  cs ok
           app  │ DAT node-A #41 t=12351 ms  button released  events 3  period 2500.000 ms
           raw  │ 0000 │ 02 00 00 00 00 12 02 00  00 00 00 11 08 00 45 00 │ ..............E.
                │ 0010 │ 00 40 00 00 40 00 40 11  1f 45 c0 a8 4d 0b c0 a8 │ .@..@.@..E..M...
                │ … 46 B more
```

Each layer has its own color (`eth`, `ip`, `transport`, `app`, `raw`, see [Colors](#colors)). The header's
protocol is a **badge**, black text on the protocol's own pastel background, the same in the trace, in the node's
own lines and in the dashboard's protocol ranking ([Protocol colors](#protocol-colors)). A damaged frame says
`✗ damaged` in red after its header. Every time stamp, in every line, is seconds and milliseconds (`12.351`).

Checksums (IPv4 header, UDP, TCP, ICMP) are verified when the whole frame was kept: `cs ok`, `cs BAD`, or `cs -`
when the frame was longer than the 256 bytes kept. A damaged frame is marked in red.

The trace costs the node almost nothing while it is off, and little while it is on: the driver copies the start of
each frame into a small queue and the main loop prints it, a couple of frames per pass, only while the console has
room. If frames arrive faster than the console can print them (a burst, `trace frames` at full rate), the ones
that don't fit are counted and said (`trace: N frame(s) not shown`): use `trace events`, or `dump 0`, for less.

The UART firmware has no network frames: its `trace` and `trace_bytes` settings exist so presets and saved
settings read the same on both, and do nothing.

#### Reading the compact view at speed

```text
    12.351 RX ◀  TCP │ .11:49152→.12:7000     │ [SYN] 58 B
    12.351 TX ▶  ARP │ who-has .12 tell .11   │ 42 B
    12.354 TX ▶  UDP │ .11:7000→.12:7000      │ 79 B             │ DAT node-A #105 t=12351 ms  button released … │ ×6
    12.355 RX ◀  TCP │ .11:49152→.12:7000     │ [RST,ACK] 54 B
```

- **Short addresses, if you ask.** `set trace_short_ip on` drops what every address has in common with this node's
  subnet: `.12` for `192.168.77.12` on a /24 (an address from elsewhere is written whole). It is **off** by
  default: the addresses are whole, and the column is wide enough for the longest line (`who-has` with two
  addresses, a MAC) so that the bars `│` line up whatever the line says.
- **Fixed columns.** Each column has its own width, so the eye follows a column down a screen of lines. The
  endpoints are dimmed, the protocol and the node's own frame are not.
- **Runs are folded.** A run of identical frames (same direction, endpoints and TCP flags: the periodic `DAT`
  of a busy link, a burst) is one line, the last frame of the run, with `×N`. A control frame or a damaged one
  is never folded: it is what you are looking for.

When even that is too much to read, the [dashboard](#the-traffic-dashboard-dash) gives the rates.

## The traffic dashboard: `dash`

Once it is shown (it is **off** at boot: `dash normal` turns it on), a block of graphs sits at the bottom of the
console, above the prompt, redrawn in place like the live line (at most twice a second, and only when something
changed), while everything else scrolls above it. It does not depend on `trace`: it counts the frames itself, so
it works with the trace on or off, and the trace works without it. It has three **views** (vertical bars, a line chart, the chart with the protocols) and three **sizes**; `dash` alone goes to the next
view, `dash mini|normal|full|off` chooses the size.

Every rate is in **bits per second** and packets per second, counted for **every** frame while the dashboard is on,
whatever the trace level keeps: `trace events` shows only the control frames and the dashboard still shows the whole
traffic. With `dash off` nothing is counted and nothing is drawn (the driver's tap then reads two flags per
frame and stops).

### `bars`: vertical bars per direction

```text
rx  45.76 Mbit/s ┤           ▆███                          │ now 960.6 kbit/s   1001 pkt/s
               0 ┤        ▃█████████▂                      │ avg  119 B
tx   1.24 Mbit/s ┤                           ▄█████████████ │ now  1.23 Mbit/s    300 pkt/s
               0 ┤                           ████████████████│ avg  513 B
                 └-30s ───────────── -15s ───────────── now
```

One plot of rectangles per direction, a column per slice of the window (30 s over 40 columns: 750 ms), the
newest on the right, only complete slices (so the right edge doesn't flicker). The height of a column is the
bytes of the slice, in steps of an eighth of a row (`▁` to `█`; 2 rows tall in `normal`, 3 in `full`, 1 in `mini`);
an empty slice is blank, and any traffic shows at least `▁`. Each plot is scaled to **its own** peak (the scale on
its left), so a quiet direction still shows its shape, with a time axis under them and on the right what a
network engineer reads: the rate over the last second, **packets per second**, the **average frame size**, the
peak and the **use of the link** (against the speed the PHY negotiated).

### `chart`: the classic network graph

```text
               5.72 Mbit/s ┤          ╭───╮                         │ ━ rx   960.6 kbit/s   1001 pkt/s · avg  119 B
                           │         ╭╯   │                         │ ━ tx    1.23 Mbit/s    300 pkt/s · avg  513 B
                           ┤        ╭╯    ╰╮                        │ peak   5.72 Mbit/s  link rx 0.09% tx 0.12%
                           │        │      │                        │
                         0 ┤────────╯      ╰────────────────────────│
                           └-30s ───────────── -15s ───────────── now
```

Receive and send as two **lines on the same axes**: one bit-rate scale for both, so they can be compared, and
the time under them (5 rows tall in `normal`, 6 in `full`, 3 in `mini`). A line goes from the middle of a slice to
the middle of the next one with `─ │ ╭ ╮ ╯ ╰`; where the two meet in a character, send is drawn. Any traffic is above
the bottom row, so a trickle doesn't look like silence. Beside it: the rate over the last second, packets per
second, the average frame size, the peak and the use of the link.

### `mix`: and the protocols that went by

The chart, and under it the protocols that went by (both directions, since the graphs started), ranked by the
number of frames as horizontal bars. A bar is a row of spaces on an ANSI **background color**, one per protocol
(UDP, TCP, ARP, ICMP, IGMP, PTP, other IPv4, anything else), so it reads at a glance and can be changed:
`mix_udp`, `mix_tcp`, `mix_arp`, `mix_icmp`, `mix_igmp`, `mix_ptp`, `mix_ip` and `mix_other` in `[colors]` (see
[Colors](#colors)), as backgrounds, e.g. `"30;44"` is black on blue and `"30;48;5;208"` black on a 256-color orange.
They are the colors of the trace's protocol badges too.

```text
            UDP │███████████████████████████████████████ │  95.46% 55500 frames · rx 54000 · tx 1500  (since the graphs started)
            TCP │██                                      │   4.29% 2499 frames · rx 0 · tx 2499
            ARP │                                        │   0.20% 120 frames · rx 60 · tx 60
```

A thin dimmed rule (`── network · chart · normal · 30s ────`) sits above the block, so you can tell where what
scrolls ends and the dashboard begins.

| Lines taken (the rule included) | `mini` | `normal` | `full` |
|---|---|---|---|
| `bars` | 3 | 6 | 8 |
| `chart` | 4 | 7 | 8 |
| `mix` (the chart and the protocols) | 5 (one stacked bar of the shares, and the three biggest named) | 10 (the three biggest) | 14 (all six) |

`window <seconds>` (`window 2min`, 2 to 3600 s) chooses what the graphs cover, and starts them over. The settings
are `trace_dash` (size), `trace_dash_view` and `trace_window_s`, so a preset can choose them.

**Long answers wait for room.** The console's output goes through a 16 KB pipe that never blocks and drops what doesn't fit,
in the middle of whatever was being written. A `help` page with a dashboard under it is 10-25 KB (each section is
followed by the redraw of the prompt block, dashboard included), more than the USB link carries in a few milliseconds
(about 64 KB/s). So a command that answers at length (`help`, `config`, `link`, `info`, `list`, `status`) is held back
until the pipe has 8 KB free, the sections of `help` the same, and the trace and the dashboard leave that room alone:
the trace is what gives way, and says how many frames it did not show.

Erasing the prompt block and drawing it again takes a few milliseconds on the USB link, so the console wraps
every redraw in the terminal's **synchronized output** mode (`ESC[?2026h` … `l`, the cursor hidden meanwhile): the
terminal holds what it receives and paints it at once, and the block never shows empty. Terminals that know the mode
(Ghostty, kitty, WezTerm, iTerm2, recent tmux) show no flicker; the others ignore it. To check yours:
`printf '\e[?2026$p'` answers `\e[?2026;1$y` or `;2$y` when it is supported.

## Generating traffic: `gen`

`burst` measures the link with the node's own frames. `gen` **fills the network** with the load you describe, and
you steer it while it runs. It sends UDP datagrams to the peer on port `net.port + 1` (whatever carries the node's
frames, UDP or TCP), and the peer tells what arrived. Watch it with `trace` and its [dashboard](#the-traffic-dashboard-dash):
the generated frames cross the same port.

```text
gen start 2Mbit size=512 for=30s        2 Mbit/s of 512-byte datagrams, for 30 s
gen start 5000 size=imix                5000 datagrams a second, a realistic mix of sizes, until `gen stop`
gen rate 50Mbit                         change the rate of the stream that is running
gen shape pulse:200ms:800ms             200 ms of traffic, 800 ms of silence, and again
gen pause · gen resume · gen stop       hold it (its clock too), go on, end it
gen                                     what it is doing
```

| Setting | Values | Default |
|---|---|---|
| rate (`gen rate`, or the bare number of `start`) | `5000` datagrams/s, `5kpps`, or **on the wire**: `2Mbit`, `500kbit`, `1Gbit` (headers counted: a frame is 42 bytes more than its payload). With a shape: the **floor** | 1000 pps |
| size (`size=`, or `512B`, `64-1400`, `imix`) | `512` bytes of payload (16-1472), `64-1400` a random size in the range, `imix` (7 small, 4 medium, 1 large: the classic Internet mix) | 256 B |
| shape (`shape=`, or any word with a colon) | `flat`; `random` the same average rate with random gaps; `pulse:200ms:800ms` on, then off; `ramp:10Mbit:30s` from the rate to 10 Mbit/s in 30 s, then it stays; `steps:10Mbit:5:30s` a staircase of 5 levels; `sine:10Mbit:20s`, `cosine:…`, `triangle:…`, `saw:…`, `square:…` a wave between the rate and 10 Mbit/s, one cycle every 20 s | flat |
| time (`for=`, `count=`, or `30s`, `forever`) | a time (`30s`, `2min`), `forever`, a number of datagrams (`count=`), or `auto` | `auto`: as long as the shape (one cycle of a wave, the time of a ramp...), 10 s at least |

**In a shape, the rate you give first is the floor, the value in the shape is the peak, and the time is one cycle.**
The stream is a separate matter: it lasts one cycle by default, and `forever` (or `for=60s`) makes the wave repeat.

#### Recipes

```text
# Triangles that repeat: up from 200 kbit/s to 2 Mbit/s and down again, every 10 s, until `gen stop`
gen start 200kbit triangle:2Mbit:10s forever

# A sine that never reaches zero: it swings between 200 kbit/s (the floor) and 2 Mbit/s every 20 s, for ever.
# It is a raised sine: floor + (peak - floor) x (1 - cos(2 pi t / T)) / 2, so it is strictly positive as long as the floor is.
gen start 200kbit sine:2Mbit:20s forever

# The same, starting at the peak (a raised cosine)
gen start 200kbit cosine:2Mbit:20s forever

# Saw teeth every 5 s, for a minute (12 teeth)
gen start 100kbit saw:1Mbit:5s 60s

# A day's traffic in miniature: a slow sine with realistic frame sizes
gen start 500kbit imix sine:20Mbit:60s forever

# Change it while it runs: a bigger peak, a faster cycle
gen shape triangle:5Mbit:4s
```

Watch the other node with `trace events`, `dash chart` and `window 60`: a window of two or three cycles draws the shape.
The same commands with the keys spelled out: `gen start rate=200kbit shape=triangle:2Mbit:10s for=forever`.

What isn't said in `start` stays as it was, and `gen rate`, `gen size`, `gen shape`, `gen for` and `gen count` change
the settings, a stream that is running included (the next datagram already follows the new size). A rate in bits
counts what is on the wire, so mixed sizes still hit it. The schedule is exact to the microsecond (tested on the
host), except that a node that stalls does not repay the time it lost with a flood: it catches up by 5 ms at most. If the
socket is full when a datagram is due it waits, and the report says so.

When a stream ends, **both nodes report**:

```text
  12.351 TX ▶ gen #1 done │ 1919 datagrams, 1999.5 kB in 2.00 s │ 7.99 Mbit/s · 959 pkt/s · 0.7% of the link
  12.351 RX ◀ gen #1 │ 1881 received · lost 38 (1.98%) · out of order 0 · jitter 68 µs │ 7.83 Mbit/s · 940 pkt/s · 0.7% of the link · 2.00 s
```

The receiver knows how many were sent from the stream's last datagram (an end marker), so **losses at the very end
are counted** too; if the marker itself is lost it closes the stream after a second of silence and says so. The
**jitter** is the one of RFC 3550: the variation of the transit time between consecutive datagrams, so the two
boards' clocks needn't agree. Every datagram starts with a 16-byte header (`WGEN`, stream, flags, sequence number,
sender's clock) and the rest is filler.

## The node dashboard

Both nodes on one page, in your browser: the same consoles, with controls and charts around them.

```sh
python3 scripts/dev.py boards assign a 6C:1A   # once per board: which one is node A, which is B
python3 scripts/dev.py dash                    # the two boards (close their serial monitors first)
python3 scripts/dev.py dash --demo             # two simulated nodes, no hardware (--demo uart: the UART app)
```

The page is at `http://127.0.0.1:8060`. **Nothing is added to the firmware**: the tool opens each board's USB console,
shows what it prints and reads the numbers out of it (counters, rates, round-trip times, link state, bursts, settings).
What you type in a console of the page goes to the board as if typed in `dev.py monitor`.

| Section | What is there |
| --- | --- |
| **Boards** | each node's state and its console, with the colors, the live line, filters and a command line |
| **Control** | what the CLI does, as controls: console verbosity, RGB LED, breadboard LEDs, traffic period and mode, bursts, the link (baud rate, or network settings and trace), the traffic generator |
| **Measure** | both boards on one time axis: frames per second, round-trip times, the interval between data frames, link and button events, burst and generator runs |
| **Configure** | the settings with *save*, the peer's settings side by side to pull or push, and the peer's address space |

Round-trip times and frame intervals come from the frame lines, so those charts need `verbose frames` on the nodes
(the page offers a button for it), and round trips need `tx on-rx` on the peer. The cards move, fold and resize
([Arranging the two dashboards](tools.md#arranging-the-two-dashboards)), and the view is
[saved in the repository](tools.md#saved-settings-and-git).

## Colors

Every colored part of the console has a **role** (the time stamp, the `TX ▶` arrow, node names, errors, each
layer of the trace...) and the build chooses a color for each, in the `[colors]` section of a
[preset](presets.md). They are build-time settings: they don't change at run time, and a wrong value fails the
build with the key in the message.

```toml
[colors]
tx    = "1;36"        # bold cyan
node  = "38;5;208"    # a 256-color orange
error = "1;37;41"     # bold white on red
app   = ""            # no color
```

A value is the SGR parameters of an ANSI escape code, numbers separated by `;`. The list of roles and their
defaults is in the [configuration reference](reference/config.md#colors-colors-in-a-preset) (and, for the Ethernet
app, [here](reference/eth-config.md#colors-colors-in-a-preset)). Bold and faint are not roles: the console adds them itself
where they help, so a theme only has to pick hues.

### Protocol colors

Every protocol has one look wherever it appears: the header of a traced frame, the `DAT` / `EVT` / `UA` badges of
the node's own lines, and the bars of the dashboard's protocol ranking. It is **black text on a pastel
background** (a 256-color `30;48;5;N`), so the badges read the same on a dark or a light terminal:

| Badge | Color | Role |
|---|---|---|
| <span style="background:#ffffaf;color:#000;padding:0 .6em;font-family:monospace">ARP</span> | 229 · yellow | `mix_arp` |
| <span style="background:#ffafd7;color:#000;padding:0 .6em;font-family:monospace">UDP</span> | 218 · pink | `mix_udp` |
| <span style="background:#afffaf;color:#000;padding:0 .6em;font-family:monospace">TCP</span> | 157 · green | `mix_tcp` |
| <span style="background:#afd7ff;color:#000;padding:0 .6em;font-family:monospace">UA</span> | 153 · blue | `ua`: the address space protocol (UA-lite) |
| <span style="background:#d7afff;color:#000;padding:0 .6em;font-family:monospace">ICMP</span> | 183 · lavender | `mix_icmp` |
| <span style="background:#ffaf87;color:#000;padding:0 .6em;font-family:monospace">IGMP</span> | 216 · orange | `mix_igmp` |
| <span style="background:#afffff;color:#000;padding:0 .6em;font-family:monospace">PTP</span> | 159 · cyan | `mix_ptp` |
| <span style="background:#e4e4e4;color:#000;padding:0 .6em;font-family:monospace">DAT</span> | 254 · light grey | `dat`, `evt`, `lnk`: the node's own frames (`DAT`, `EVT`, `ECH`, `BAU`) |
| <span style="background:#d0d0d0;color:#000;padding:0 .6em;font-family:monospace">IPv4</span> | 252 · grey | `mix_ip`: another IPv4 protocol |
| <span style="background:#9e9e9e;color:#000;padding:0 .6em;font-family:monospace">other</span> | 247 · dark grey | `mix_other`: IPv6, LLDP... |
| <span style="background:#ff8787;color:#000;padding:0 .6em;font-family:monospace">bad</span> | 210 · red | `mix_bad`: a frame that can't be decoded |

Change one in `[colors]` like any other role, as a background (`mix_udp = "30;48;5;208"`).

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
  by itself, and shows nothing until it does (`dev.py monitor` presses it for you);
- `--start 4.5` drops the boot from the recording, `--trim REGEX` drops lines
  (the ROM and bootloader logs);
- record from a committed tree, or the boot card shows `+dirty`. The `.cast` files
  are kept next to the GIFs (`docs/book/assets/demo/`), to render them again with
  other settings.

### The look of the GIFs

A cast is the raw text of the session, so a GIF can be **restyled without recording again and without a board**:
font size, line height, theme, font, speed, pauses, window size. The look is written once, with every key explained, in
`docs/book/assets/demo/render.toml`:

```toml
[style]
font_size = 15       # pixels: a smaller font gives a smaller picture and file
theme = "monokai"
speed = 1.0

[eth-boot]           # one recording only: a table named like the cast
font_size = 12       # the long boot card, smaller
```

```sh
python3 scripts/render_demos.py --list              # the look each cast would get
python3 scripts/render_demos.py --font-size 12      # all casts, smaller, to try it
python3 scripts/render_demos.py eth-boot            # one cast, with what render.toml says
python3 scripts/record.py boot.cast --render-only --theme nord   # one file, another theme
```

The same options work when recording (`record.py ... --font-size 12`, `record_demos.py --font-size 12`). At 15 px a
100-column cast is 918 px wide, at 11 px it is 673 px; the text stays crisp. The full list of keys is in
[Console recordings](automation.md#console-recordings-the-gifs).
