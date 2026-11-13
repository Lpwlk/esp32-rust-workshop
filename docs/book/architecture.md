# Architecture

## The crates and the two apps

One shared core, two firmwares, and a host tool. The firmwares only differ by their **link**.

| Crate | Folder | Contents | Tested |
|---|---|---|---|
| `workshop-core` | `firmware/shared/core` | everything that doesn't touch a peripheral: the frame protocol and the address space (UA-lite), line splitting, lost-frame detection, configuration (UART *or* Ethernet link) and its validation, CLI editor/parser/help table, button debouncing, LED logic, RGB color math, saved-settings format, documentation generator | on the host (`cargo test-host`): unit tests, property tests, doc examples |
| `workshop-presets` | `firmware/shared/presets-tool` | host only: reads the TOML presets, applies them through the same checks as the CLI, and writes the Rust each firmware includes | on the host |
| `esp32-rust-workshop` | `firmware/uart` | the UART app: UART1 + interrupt, GPIO + interrupt, RMT (RGB LED), USB Serial/JTAG (CLI), TRNG, console output, a blocking main loop | on the board (`cargo test-target`) |
| `esp32-rust-workshop-eth` | `firmware/ethernet` | the Ethernet app: PHY + EMAC + embassy-net (UDP or TCP), the same glue, an async main loop, a console task | built and linted by CI, run on two boards |

Each firmware re-exports the core modules (`crate::config`, `crate::protocol`, `crate::space`...), so the node
configs and the rest of the code don't need to know about the split. The Ethernet app is its own Cargo
workspace, because it needs an esp-hal that isn't released yet (see [Ethernet app](ethernet.md)).

```mermaid
flowchart LR
    subgraph core [workshop-core · host-tested]
        direction TB
        protocol
        space["space (UA-lite)"]
        line
        seq
        config["config (Link::Uart | Link::Net)"]
        cli
        button
        leds
        fx
        docgen
    end
    presets["presets/*.toml<br/>(workshop-presets, at build time)"] --> core
    subgraph uart [firmware/uart · blocking loop]
        direction TB
        link[link: UART1 + IRQ] --> main
        btn[button: GPIO IRQ] --> main
        usb[cli: USB Serial/JTAG] --> main
        main[main loop] --> rgb[led: RMT] & ext[ext_leds: GPIO] & term[term: console]
    end
    subgraph eth [firmware/ethernet · async]
        direction TB
        net[net: PHY + EMAC + embassy-net] --> linkq[link: UDP / TCP tasks]
        linkq <--> mainE[main loop]
        btnE[button: GPIO IRQ] --> mainE
        mainE --> consoleq[console: pipe + task]
    end
    core --> uart
    core --> eth
```

## The main loop

This is the **UART** app's loop. The Ethernet app has the same steps in the same order, but its loop shares an
executor with the network tasks and gets frames from a queue instead of an interrupt (see
[Ethernet app](ethernet.md#how-it-works)).

The firmware is a single loop, with two interrupts feeding it:

1. render the RGB LED frame when due, update the breadboard LEDs;
2. read CLI key presses, apply complete commands;
3. turn a debounced button edge into an `EVT` frame;
4. send the periodic `DAT` frame;
5. read bytes buffered by the UART interrupt, split them into lines, decode frames;
6. print the stats line when due (from `verbose frames`; at the default `live` level the last frame and the
   counters share one line that is redrawn in place, see [the console](console.md#how-much-the-console-prints-verbose)).

## Receiving at 5 Mbaud

At 5 Mbaud the 128-byte UART RX FIFO fills in about 256 µs, faster than the loop
can be relied on to poll it: printing a line to the USB console can take longer.
So:

- an interrupt (FIFO half full, or line idle for 10 symbols) drains the FIFO into
  a 2 KB ring buffer, and the loop reads from it;
- `esp-println` is used **without** its `critical-section` feature, which would mask
  interrupts during every print;
- nothing is printed inside a critical section.

`verbose wire` shows the ring buffer peak (`RX buffer peak 33/2048`).

## Timing on the wire

Each `DAT` line shows `Δ`, the time since the previous `DAT` frame **in either
direction**, and `rtt` values are computed the same way. Both use one reference:
the moment the frame's `\n` finished on the wire, not when the main loop got to
it (printing to the USB console can delay that by milliseconds).

- **Received**: the UART interrupt notes when it read each `\n`, then corrects it:
  minus one byte time (2 µs at 5 Mbaud) per byte that came after it in the same
  batch, and minus the idle timeout when the interrupt fired because the line went
  quiet. What remains is the interrupt latency, a few µs.
- **Sent**: the time the frame was queued in the (empty) TX FIFO, plus its duration
  on the wire (`length × 10 bits / baud rate`).

Measured on the boards: received deltas vary by about ±0.15 ms, sent ones by about
±0.8 ms (a DAT goes out when the main loop checks its timer). The link is
full-duplex, so a delta can be slightly negative: a received frame can end while
one of ours is being sent. The math is in `workshop_core::wire`, tested on the host.

## Button debouncing

Two buttons, the jumper (`pins.button`) and BOOT (GPIO61), share esp-hal's single
GPIO interrupt handler: it checks each pin's interrupt flag and only records that
its line moved, and when. The loop then asks one
`workshop_core::button::Debouncer` per button:

- **debounce** (30 ms): the line must be quiet that long before its level is read;
- **lockout** (150 ms): after an accepted edge, further changes are ignored. A
  jumper sliding into a breadboard can lose contact for longer than the debounce
  time; the level is read again when the lockout ends, so a real change is never
  missed.

## Random keys

`EVT` keys come from the ESP32-S31 true random number generator. esp-hal 1.2.2
reads it from the right register on this chip but never starts it, and the
ESP-IDF bootloader turns it off before jumping to the app: every key would be the
same. `firmware/uart/src/trng.rs` starts it at boot, like `trng_ll_enable()` in ESP-IDF v6.1.

## Link tests

Three features exercise the link; their logic is in `workshop-core` (`baud`,
`rtt`, `burst`), tested on the host. On Ethernet there is no speed to change
(`baud` and its `BAU` frames are UART only; `link` shows what the PHY negotiated), the round-trip time and the
bursts work the same, and a burst's share is computed against the negotiated link speed.

**Speed change (`baud`).** Both nodes must use the same baud rate, so a change is a
handshake of `BAU` frames: the requester sends `REQ` at the current speed, the
peer answers `ACK`, waits until it has left its UART, and switches; the requester
switches on the `ACK`. The first valid frame at the new speed confirms the switch.
If none arrives in time, each node goes back to the boot speed on its own: both
know it, so they meet again there.

```mermaid
sequenceDiagram
    participant A as node-A
    participant B as node-B
    A->>B: BAU REQ 1M (at 5 Mbaud)
    B->>A: BAU ACK 1M (at 5 Mbaud)
    Note over B: TX FIFO empty, switch to 1 Mbaud
    Note over A: switch to 1 Mbaud
    A->>B: DAT (at 1 Mbaud): confirmed
```

**Round-trip time (`tx on-rx`).** A node in `on-rx` mode stops sending `DAT` and
answers each frame from the peer with an `ECH` carrying that frame's `seq`. The
peer remembers when it sent its last 16 frames, and shows the difference: both
UART transfers plus the reaction time of the other node's main loop.

**Bursts (`burst`).** `BST` frames are scheduled by `BurstTx` (frame *i* is due at
*start + i / rate*, so a late loop catches up without drifting; back-to-back
bursts send for at most 2 ms per loop iteration so the rest keeps running). The
receiver counts them with `BurstRx` instead of printing them (the USB console
couldn't keep up), and prints one summary: received, lost, corrupted, throughput
and share of the line capacity (8N1: 10 bits per byte).

## Configuration

`NodeConfig` holds everything a node may choose on its own, and a `link` section says how it reaches its peer: `Link::Uart`
(two pins, the baud rate is a shared constant) or `Link::Net` (addresses, port, transport). It is written as a
[preset](presets.md), a TOML file the build script checks and turns into a `const NodeConfig`. That `const` is
validated by a `const fn`, so an invalid value fails the build; the CLI `set` command, the address space's `WRT`
frames and the settings saved in flash (`save`) go through the same function at runtime. Cargo features (`persist`,
`watchdog`, `rtt`) choose what is compiled in. The whole map is on the [Configuration](configuration.md) page; every
key is in the [settings reference](reference/config.md).
