---
template: home.html
title: Home
hide:
  - navigation
  - toc
---

## What's inside { .ws-section }

<div class="grid cards" markdown>

-   :lucide-network:{ .lg .middle } __Gigabit Ethernet link__

    ---

    The board's RJ45 port, cabled straight to the other board: static IP addresses, no switch, no DHCP.
    One frame per UDP datagram, or one TCP connection; lost frames are detected, and back-to-back bursts
    arrive intact. Four frame types (`DAT` `EVT` `ECH` `BST`) with an XOR checksum.

    [:lucide-arrow-right: The Ethernet app](ethernet.md)

-   :lucide-cable:{ .lg .middle } __UART link__

    ---

    The same node over three wires, as an alternative: the same frames (plus `BAU`, to change the speed
    on both boards at once), reception by interrupt into a ring buffer so nothing is lost while the console prints.

    [:lucide-arrow-right: Frame protocol](reference/protocol.md)

-   :lucide-terminal:{ .lg .middle } __A real CLI on the USB console__

    ---

    `help`, `config`, `set`, `reset`… with history (↑ ↓), in-line editing (← →)
    and values as periods or frequencies (`period 2.5Hz`).

    [:lucide-arrow-right: CLI commands](reference/cli.md)

-   :lucide-gauge:{ .lg .middle } __Link tests__

    ---

    Measure the round-trip time (`tx on-rx`), fire bursts and read the throughput; on UART, change the
    speed on both nodes at once (`baud`, with automatic fallback).

    [:lucide-arrow-right: How they work](architecture.md#link-tests)

-   :lucide-lightbulb:{ .lg .middle } __Button and LEDs__

    ---

    A jumper "button" and the BOOT button, on interrupts, with debounce and lockout, five RGB fade patterns (viridis,
    sunset…), two breadboard LEDs that follow the link or the peer's button.
    Read and write the peer's LEDs, buttons and settings as an address space ([UA-lite](protocols.md)).

    [:lucide-arrow-right: RGB patterns](reference/patterns.md)

-   :lucide-shield-check:{ .lg .middle } __Configuration checked at build time__

    ---

    One `const` per node; a wrong pin, a reserved GPIO or an out-of-range period
    fails the build. Change settings live, `save` them to flash; the same rules
    apply everywhere. Cargo features: `persist`, `watchdog`, `rtt`.

    [:lucide-arrow-right: Configuration](configuration.md)

-   :lucide-audio-waveform:{ .lg .middle } __The ADC on your screen__

    ---

    One board samples an analog pin at up to 83 kS/s with the ADC's DMA and streams it over Ethernet: a live
    spectrum, waveform and waterfall in your browser, a digital filter, a real PWM generator to feed it a known
    signal, and settings that survive a restart.

    [:lucide-arrow-right: The ADC app](adc-stream.md)

-   :lucide-flask-conical:{ .lg .middle } __Tested on your computer__

    ---

    The hardware-free logic lives in `workshop-core`: ~150 unit, property and doc
    tests. The reference pages are generated from the code and checked by CI.

    [:lucide-arrow-right: Testing](testing.md)

</div>

## See it run { .ws-section }

<div class="asciicast" data-cast="assets/demo/boot.cast" data-poster="npt:0:12"></div>
<p class="asciicast-caption">Node A after a reset: the boot card, then frames exchanged with node B · recorded on the boards, <a href="assets/demo/boot.cast">.cast</a></p>

Real sessions, colors included: the [console tour](console.md) shows the CLI, the
link tests and RTT.

## Quick start { .ws-section }

=== "Flash the boards"

    ```sh
    rustup target add riscv32imafc-unknown-none-elf
    cargo install espflash --locked
    cd firmware/ethernet                # or firmware/uart for the UART link
    cargo run --release --bin node-a    # board A
    cargo run --release --bin node-b    # board B, in another terminal
    ```

    Then type `help` in the console. Wire the boards first: [Wiring](wiring.md).

=== "Run the tests"

    ```sh
    cargo test-host                      # workshop-core, on this computer
    cargo clippy --release -- -D warnings
    ```

=== "Preview this site"

    ```sh
    python3 scripts/dev.py setup         # once: .venv with Zensical
    python3 scripts/dev.py docs serve --open
    ```

## Wiring at a glance { .ws-section }

**Ethernet**: a cable between the two RJ45 ports (a straight cat5e is enough). The PHY uses GPIO5-19.

**UART**: three wires.

| Board A | | Board B | Each board |
|---|:-:|---|---|
| `2` GPIO2 (TX) | → | `3` GPIO3 (RX) | `4` GPIO4 ↔ `G`: jumper button |
| `3` GPIO3 (RX) | ← | `2` GPIO2 (TX) | GPIO20 / GPIO21: breadboard LEDs |
| `G` GND | — | `G` GND | USB Serial/JTAG: flash, console, probe |

Header J2 labels are GPIO numbers. Full details, resistor values and pins to avoid:
[Wiring](wiring.md).

## Go further { .ws-section }

<div class="grid cards" markdown>

-   :lucide-rocket:{ .lg .middle } [__Getting started__](getting-started.md)

    Toolchain, flashing, console, configuring a node.

-   :lucide-terminal-square:{ .lg .middle } [__The console__](console.md)

    Boot card, frame lines, CLI, link tests and RTT, in recordings.

-   :lucide-code:{ .lg .middle } [__Examples__](examples.md)

    Runnable Rust: frames, settings, debouncing, the CLI. Compiled by CI.

-   :lucide-sliders-horizontal:{ .lg .middle } [__Configuration__](configuration.md)

    Cargo features, node config, saved settings, and where to change what.

-   :lucide-cpu:{ .lg .middle } [__Firmware walkthrough__](firmware.md)

    Boot, the main loop, the UART interrupt, shared state, flash, RMT,
    raw registers: the embedded code explained, line by line.

-   :lucide-blocks:{ .lg .middle } [__Architecture__](architecture.md)

    The crates, the main loop, frame reception, link tests.

-   :lucide-audio-waveform:{ .lg .middle } [__The ADC app__](adc-stream.md)

    The ADC streamed to a live page, the PWM generator and the page's saved settings.

-   :lucide-wrench:{ .lg .middle } [__Tools and workflows__](tools.md)

    Every Rust and Python tool, the four Cargo workspaces, what to run before a push, CI.

-   :lucide-timer:{ .lg .middle } [__Ferrite RTOS__](ferrite.md)

    The project's own preemptive RTOS in Rust: a host-tested kernel and its ESP32-S31 port.

-   :lucide-book-open:{ .lg .middle } [__Rust & Cargo guide__](guide-rust.md)

    Cargo, then Cargo for microcontrollers, plus copy-ready commands.

-   :lucide-code-xml:{ .lg .middle } [__API (rustdoc)__](api.md)

    Every type and function, with examples that are tests.

</div>

!!! note "Status of the ESP32-S31 support"
    esp-hal marks the ESP32-S31 as "in early stages of development". Two gaps are
    handled here: esp-hal 1.2.2 doesn't start the true random number generator
    (see [Architecture](architecture.md#random-keys)), and probe-rs 0.32 builds
    images these chips refuse (see the [guide](guide-rust.md)).
