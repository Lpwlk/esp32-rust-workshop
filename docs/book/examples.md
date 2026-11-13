# Examples

Short programs that use the firmware's logic directly. They run **on your
computer**: everything in `firmware/shared/core` is plain `no_std` Rust, without a
peripheral, so the same code runs on the ESP32-S31 and on the host.

!!! success "These examples can't lie"
    The code on this page is not copied: it is **included** from
    `firmware/shared/core/examples/*.rs` when the site is built
    (`pymdownx.snippets`). CI compiles and runs each example, and they are full of
    `assert!`: a change that breaks one fails the build before it can reach this
    page.

```sh
cargo run -p workshop-core --target host-tuple --example frames
cargo run -p workshop-core --target host-tuple --example settings
cargo run -p workshop-core --target host-tuple --example button
cargo run -p workshop-core --target host-tuple --example cli
```

## Frames on the wire

### Build and encode a frame

=== "Code"

    ```rust title="firmware/shared/core/examples/frames.rs"
    --8<-- "firmware/shared/core/examples/frames.rs:encode"
    ```

=== "Output"

    ```text
    on the wire: $DAT,node-A,42,12345,1,3,2500000*3F
    ```

A frame is one text line: `$TYPE,node,seq,t_ms,fields…*CS\n`, where `CS` is the XOR
of the bytes between `$` and `*`, in uppercase hex. Readable in any serial
terminal, and cheap to check on a microcontroller.

!!! note "No allocator"
    `encode` returns a `heapless::String<MAX_FRAME>`: a fixed-capacity string on the
    stack. The longest possible frame is checked by a unit test to fit.

### Parse what arrives

=== "Code"

    ```rust
    --8<-- "firmware/shared/core/examples/frames.rs:parse"
    ```

=== "Output"

    ```text
    DAT #42 from node-A, button pressed
    ```

`parse` borrows from the line (`Frame<'_>` holds a `&str` for the node name): no
copy, no allocation.

### A corrupted byte is caught

=== "Code"

    ```rust
    --8<-- "firmware/shared/core/examples/frames.rs:corrupt"
    ```

=== "Output"

    ```text
    corrupted: checksum 0x3F, expected 0x3C
    ```

??? question "Is an XOR checksum enough?"
    It detects **any single corrupted byte** (a property test checks exactly that,
    on random frames and random corruptions), and any corruption that flips an odd
    number of bits in the same bit position. It misses two errors that cancel out, which a CRC
    would catch. On a few-centimetre UART link, single errors are the realistic
    case, and the readable format is worth it.

### From a byte stream to frames

=== "Code"

    ```rust
    --8<-- "firmware/shared/core/examples/frames.rs:stream"
    ```

=== "Output"

    ```text
    EVT #7 from node-B: First
    DAT #42 from node-A: Lost(34)
    ```

This is the receive path of the firmware: the UART interrupt fills a ring buffer,
the main loop feeds the bytes to `LineBuffer`, then `parse` and `seq::check`.
A property test checks that chunking never changes the lines that come out.

## Settings

### A node's config, checked at compile time

```rust title="firmware/shared/core/examples/settings.rs"
--8<-- "firmware/shared/core/examples/settings.rs:const"
```

`check()` is a `const fn`: in a `const _: () = …` it runs **during compilation**.
Try a reserved pin and the build stops with a message:

```rust
use workshop_core::config::{NodeConfig, Pins};

const BAD: NodeConfig = NodeConfig { pins: Pins { button: 33, ..NodeConfig::DEFAULT.pins }, ..NodeConfig::DEFAULT };
const _: () = BAD.check();
```

```text
error[E0080]: evaluation panicked: config: a pin is reserved (flash, USB, console, RGB LED, on-board audio or BOOT)
```

### Change it like the CLI does

=== "Code"

    ```rust
    --8<-- "firmware/shared/core/examples/settings.rs:set"
    ```

=== "Output"

    ```text
    rgb.pattern    = ocean
    data_period_ms = 250
    ```

The CLI's `set` is this function: one validation (`NodeConfig::validate`) for the
build, the CLI and the flash.

### Save and reload

=== "Code"

    ```rust
    --8<-- "firmware/shared/core/examples/settings.rs:save"
    ```

=== "Output"

    ```text
    record: 59 bytes, 2 settings changed
    boot: 2 applied, 0 skipped
    ```

The record is text (`rgb.pattern=ocean`), so a firmware with new defaults or a
renamed setting skips what it doesn't know instead of misreading it.
[Saved settings](configuration.md#saved-settings-what-you-changed-and-keep).

## Debouncing a button

=== "Code"

    ```rust title="firmware/shared/core/examples/button.rs"
    --8<-- "firmware/shared/core/examples/button.rs:debounce"
    ```

=== "Output"

    ```text
    t=20 ms: waiting for the line to settle
    t=30 ms: waiting for the line to settle
    t=42 ms: Falling, 3 raw edges
    ```

```mermaid
sequenceDiagram
    participant Pin as GPIO (jumper / BOOT)
    participant IRQ as interrupt
    participant Loop as main loop
    Pin->>IRQ: edge (bounce)
    IRQ->>IRQ: raw_edges += 1, last_edge = now
    Pin->>IRQ: edge, edge…
    Loop->>Loop: ready()? settled(last_edge)?
    Note over Loop: quiet for 30 ms
    Loop->>Pin: read the level
    Loop->>Loop: settle(): Edge → EVT frame, lockout 150 ms
```

The interrupt never decides anything: it records *that* the line moved. The
decision is plain logic, tested on the host with simulated bounces.

## The console

### Line editing

=== "Code"

    ```rust title="firmware/shared/core/examples/cli.rs"
    --8<-- "firmware/shared/core/examples/cli.rs:editor"
    ```

=== "Output"

    ```text
    submitted: "pattern ocean"
    ```

The editor gets raw bytes, escape sequences included (`ESC [ D` is ←), and keeps
a history. It is fed the same way from the USB console and from RTT.

### Commands

=== "Code"

    ```rust
    --8<-- "firmware/shared/core/examples/cli.rs:parse"
    ```

=== "Output"

    ```text
    set rgb.pattern = ocean
    help set         → Help(Some("set"))
    baud 1M          → Baud(Some(1000000))
    burst 1000 max   → Burst { count: Some(1000), rate_hz: Some(0.0) }
    led 3 on         → error: usage: led <1|2> <off|on|blink [ms]|button|peer|link|rx|tx>
    frobnicate       → error: unknown command 'frobnicate' (try `help`)
    ```

## Write your own

1. Add `firmware/shared/core/examples/<name>.rs` with a `main()`; `assert!` what
   matters.
2. Mark the parts to show with section comments, `[start:part]` and `[end:part]`
   after the snippet marker: see the first lines of `main()` in `frames.rs`.
3. Include a part in a page with the same marker followed by
   `"firmware/shared/core/examples/<name>.rs:part"`, alone on its line inside a
   ` ```rust ` block.

`python3 scripts/dev.py check` runs every example, as CI does. Snippets are
documented in the
[PyMdown Extensions docs](https://facelessuser.github.io/pymdown-extensions/extensions/snippets/).
