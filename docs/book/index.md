---
template: home.html
title: Home
hide:
  - navigation
  - toc
---

## What's inside { .ws-section }

<div class="grid cards ws-pairs" markdown>

-   :lucide-network:{ .lg .middle } __Two boards, two links__

    ---

    The same node over **Gigabit Ethernet** (the RJ45 ports cabled together, UDP or TCP, no switch) or over a
    **UART** (three wires): checksummed frames, lost-frame detection, a CLI, LEDs, buttons and saved settings.
    One shared core; only the link changes.

    [:lucide-arrow-right: Ethernet app](ethernet.md) · [:lucide-arrow-right: UART app](firmware.md)

-   :lucide-audio-waveform:{ .lg .middle } __The ADC on your screen__

    ---

    A third firmware: one board samples an analog pin at up to **83 kS/s** with DMA and streams it to your
    computer: a live spectrum, waveform, harmonics and waterfall, a digital filter, and a PWM generator with a
    potentiometer to feed it a signal you know.

    [:lucide-arrow-right: The ADC app](adc-stream.md)

-   :lucide-layout-dashboard:{ .lg .middle } __Dashboards in the browser__

    ---

    One page for the ADC, and one for the two nodes, where every CLI command is a control and the charts come
    from the consoles. Cards you can drag and resize, settings kept in the repository, and both pages run
    **without a board**.

    [:lucide-arrow-right: Node dashboard](console.md#the-node-dashboard) · [:lucide-arrow-right: Layout](tools.md#arranging-the-two-dashboards)

-   :lucide-timer:{ .lg .middle } __Ferrite, an RTOS of its own__

    ---

    A small preemptive kernel in Rust: priorities, round robin, semaphores, queues, a mutex with priority
    inheritance. Tested on the computer, and its ESP32-S31 port runs on a board. **Experimental**: the apps
    don't use it by default.

    [:lucide-arrow-right: Ferrite](ferrite.md)

</div>

## The dashboards { .ws-section }

![The ADC page: the spectrum of a 1 kHz and a 3.3 kHz tone with mains hum, the waveform's controls and the strongest lines](assets/screens/adc-dashboard-dark.webp#only-dark){ .shot }
![The ADC page: the spectrum of a 1 kHz and a 3.3 kHz tone with mains hum, the waveform's controls and the strongest lines](assets/screens/adc-dashboard-light.webp#only-light){ .shot }

<p class="asciicast-caption"><code>dev.py fft --demo</code>: the ADC page on its simulated board (two tones and mains hum). <a href="adc-stream/">What the page does</a></p>

![The node dashboard: frames per second, round-trip times, the interval between data frames and the link events of both nodes](assets/screens/node-dashboard-dark.webp#only-dark){ .shot }
![The node dashboard: frames per second, round-trip times, the interval between data frames and the link events of both nodes](assets/screens/node-dashboard-light.webp#only-light){ .shot }

<p class="asciicast-caption"><code>dev.py dash --demo</code>: both nodes on one time axis; the figures are the simulation's, not a board's. <a href="console/#the-node-dashboard">What the page does</a></p>

## Try it without a board { .ws-section }

You need [Python 3.13 or newer](https://www.python.org/downloads/) (and Rust for the last line).

```sh
git clone https://github.com/Lpwlk/esp32-rust-workshop
cd esp32-rust-workshop
python3 scripts/dev.py setup          # once: a .venv with the tools' packages
python3 scripts/dev.py fft --demo     # the ADC page, simulated board     http://127.0.0.1:8050
python3 scripts/dev.py dash --demo    # the node dashboard, two nodes     http://127.0.0.1:8060
cargo test-host                       # the firmware's logic and the RTOS kernel, tested here
```

Each dashboard runs until ++ctrl+c++: use one terminal for each.

## Quick start with boards { .ws-section }

=== "Board to board"

    ```sh
    rustup target add riscv32imafc-unknown-none-elf
    cargo install espflash --locked
    cd firmware/ethernet                # or firmware/uart for the UART link
    cargo run --release --bin node-a    # board A
    cargo run --release --bin node-b    # board B, in another terminal
    ```

    Wire the boards first ([Wiring](wiring.md)), then type `help` in the console.

=== "ADC to your computer"

    ```sh
    python3 scripts/dev.py boards assign a 6C:1A   # once: the end of the board's MAC address
    python3 scripts/dev.py flash a -m --app adc    # build, flash, open the console
    python3 scripts/dev.py fft                     # in another terminal: the page
    ```

    Cable the board's RJ45 to the computer and give that Ethernet port the address `192.168.77.1`,
    netmask `255.255.255.0` ([the ADC app](adc-stream.md#run-it)).

=== "Everything, from one tool"

    ```sh
    python3 scripts/dev.py doctor          # what is missing, with the command to install it
    python3 scripts/dev.py boards          # the plugged boards: MAC, port, node
    python3 scripts/dev.py flash all       # both nodes: one build, both boards at once
    python3 scripts/dev.py check           # what CI runs, on your computer
    python3 scripts/dev.py docs serve --open
    ```

    `--app eth` (default), `--app uart` or `--app adc` picks the firmware
    ([Tools and workflows](tools.md)).

## See it run { .ws-section }

<div class="asciicast" data-cast="assets/demo/boot.cast" data-poster="npt:0:12"></div>
<p class="asciicast-caption">Node A after a reset: the boot card, then frames exchanged with node B · recorded on the boards, <a href="assets/demo/boot.cast">.cast</a></p>

<div class="asciicast" data-cast="assets/demo/dev-monitor.cast" data-poster="npt:0:11"></div>
<p class="asciicast-caption"><code>dev.py monitor a</code> on the ADC board: the boot card, the link, then a page asks for 75 kS/s · <a href="assets/demo/dev-monitor.cast">.cast</a></p>

Real sessions, colors included: the [console tour](console.md) shows the CLI, the link tests and RTT.

## Around the code { .ws-section }

<div class="grid cards ws-pairs" markdown>

-   :lucide-wrench:{ .lg .middle } __One command for the chores__

    ---

    `dev.py` tells the boards apart by MAC address, builds, flashes, monitors and rebuilds on save, with the
    Flash and RAM each firmware takes.

    [:lucide-arrow-right: Tools and workflows](tools.md)

-   :lucide-flask-conical:{ .lg .middle } __Tested on your computer__

    ---

    The hardware-free logic and the RTOS kernel pass more than 250 unit, property and documentation tests with
    no board; the Python tools test themselves against simulated boards.

    [:lucide-arrow-right: Testing](testing.md)

-   :lucide-file-check:{ .lg .middle } __Docs that can't drift__

    ---

    Reference pages generated from the code and checked by CI, examples that compile, console recordings you can
    restyle without a board.

    [:lucide-arrow-right: Formatting, docs, recordings](automation.md)

-   :lucide-radar:{ .lg .middle } __Wireshark dissectors__

    ---

    A Lua plugin that decodes what the boards put on the cable: node frames, the address space, the traffic
    generator and the ADC stream.

    [:lucide-arrow-right: Wireshark](tools.md#wireshark)

</div>

## Inside a node { .ws-section }

What the Ethernet and UART apps share:

<div class="grid cards ws-pairs" markdown>

-   :lucide-terminal:{ .lg .middle } __A real CLI on the USB console__

    ---

    `help`, `config`, `set`, `reset`… with history (↑ ↓), in-line editing (← →) and values as periods or
    frequencies (`period 2.5Hz`).

    [:lucide-arrow-right: CLI commands](reference/cli.md)

-   :lucide-gauge:{ .lg .middle } __Link tests__

    ---

    Round-trip times (`tx on-rx`) and bursts with their throughput; a traffic generator on Ethernet, a speed
    change on both nodes at once (`baud`, with automatic fallback) on UART.

    [:lucide-arrow-right: How they work](architecture.md#link-tests)

-   :lucide-shield-check:{ .lg .middle } __Configuration checked at build time__

    ---

    One `const` per node; a wrong pin, a reserved GPIO or an out-of-range period fails the build. Change
    settings live and `save` them to flash: the same rules apply.

    [:lucide-arrow-right: Configuration](configuration.md)

-   :lucide-lightbulb:{ .lg .middle } __Buttons, LEDs and the peer's__

    ---

    Debounced interrupts, RGB fade patterns, breadboard LEDs that follow the link. The other board's LEDs,
    buttons and settings are an address space you browse, read and write.

    [:lucide-arrow-right: Protocols](protocols.md)

</div>

## Wiring at a glance { .ws-section }

**Ethernet** between the boards: a cable between the two RJ45 ports (a straight cat5e is enough). The PHY uses GPIO5-19.

**UART**: three wires.

| Board A | | Board B | Each board |
|---|:-:|---|---|
| `2` GPIO2 (TX) | → | `3` GPIO3 (RX) | `4` GPIO4 ↔ `G`: jumper button |
| `3` GPIO3 (RX) | ← | `2` GPIO2 (TX) | GPIO20 / GPIO21: breadboard LEDs |
| `G` GND | — | `G` GND | USB Serial/JTAG: flash, console, probe |

**ADC app**, one board: the signal on `43` (0 to 3.3 V). To make one on the board, wire `20` (`D0`) to `43`; a
potentiometer's middle leg on `44`, its ends on `3V3` and `G`.

Header J2 labels are GPIO numbers. Drawings, resistor values and pins to avoid: [Wiring](wiring.md).

## Go further { .ws-section }

<div class="grid cards" markdown>

-   :lucide-rocket:{ .lg .middle } [__Getting started__](getting-started.md)

    Toolchain, flashing, console, configuring a node.

-   :lucide-terminal-square:{ .lg .middle } [__The console__](console.md)

    Boot card, frame lines, CLI, link tests and RTT, in recordings.

-   :lucide-code:{ .lg .middle } [__Examples__](examples.md)

    Runnable Rust: frames, settings, debouncing, the CLI. Compiled by CI.

-   :lucide-cpu:{ .lg .middle } [__Firmware walkthrough__](firmware.md)

    Boot, the main loop, the UART interrupt, shared state, flash, RMT, raw registers, line by line.

-   :lucide-blocks:{ .lg .middle } [__Architecture__](architecture.md)

    The crates, the main loop, frame reception, link tests.

-   :lucide-sliders-horizontal:{ .lg .middle } [__Presets__](presets.md)

    Whole node configurations kept as files, picked at build time.

-   :lucide-server:{ .lg .middle } [__Remote debugging__](remote.md)

    The boards on a Raspberry Pi, used from your computer over SSH.

-   :lucide-book-open:{ .lg .middle } [__Rust & Cargo guide__](guide-rust.md)

    Cargo, then Cargo for microcontrollers, plus copy-ready commands.

-   :lucide-code-xml:{ .lg .middle } [__API (rustdoc)__](api.md)

    Every type and function, with examples that are tests.

</div>

!!! note "Status of the ESP32-S31 support"
    esp-hal marks the ESP32-S31 as "in early stages of development". Two gaps are
    handled here: esp-hal 1.2.2 doesn't start the true random number generator
    (see [Architecture](architecture.md#random-keys)), and probe-rs 0.32 builds
    images these chips refuse (see the [guide](guide-rust.md)). The Ethernet app uses
    esp-hal's unreleased `main` branch, where the S31's Ethernet driver lives.
