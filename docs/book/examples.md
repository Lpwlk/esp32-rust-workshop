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
cargo run -p workshop-core --target host-tuple --example space
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

## The address space (UA-lite)

Node B describes itself as a tree (its LEDs, buttons, statistics and settings); node A browses it, reads a button,
writes a LED and watches the button. B's side is the real `space::serve` function, fed with the lines A sends: what is
printed is exactly what would travel on the UART, over UDP or over TCP. The protocol is explained in
[Protocols](protocols.md#the-address-space-ua-lite).

### Browse

=== "Code"

    ```rust title="firmware/shared/core/examples/space.rs"
    --8<-- "firmware/shared/core/examples/space.rs:browse"
    ```

=== "Output"

    ```text
    A> $BRW,node-A,1,1000,4,0*33
    B> $BRR,node-B,1,1000,1,5,1,VTW,1*4C
       found 1 (id 5)
    A> $BRW,node-A,2,1000,4,1*31
    B> $BRR,node-B,2,1000,2,6,2,VTW,0*4D
       found 2 (id 6)
    ```

One child per frame, until `more` is 0. `VTW` says a **V**ariable of type **T**ext that is **W**ritable.

### Read and write

=== "Code"

    ```rust
    --8<-- "firmware/shared/core/examples/space.rs:read_write"
    ```

=== "Output"

    ```text
    A> $RED,node-A,3,2000,8*36
    B> $VAL,node-B,3,2000,3,8,0,0*22
       jumper = 0
    A> $WRT,node-A,4,3000,5,blink 200*63
    B> $VAL,node-B,4,3000,4,5,0,blink 200 ms*50
       LED 1 written: Good, now blink 200 ms
    A> $WRT,node-A,5,4000,5,disco*67
    B> $VAL,node-B,5,4000,5,5,3,blink 200 ms*54
       LED 1 written: BadTypeMismatch, still blink 200 ms
    ```

A write goes through the same checks as the CLI's `set`; the answer carries the value now in force, so a refused write
(status 3, `BadTypeMismatch`) leaves the LED alone and says so.

### Watch

=== "Code"

    ```rust
    --8<-- "firmware/shared/core/examples/space.rs:watch"
    ```

=== "Output"

    ```text
    A> $SUB,node-A,6,5000,8,100*3E
    B> $VAL,node-B,6,5000,6,8,0,0*25
    B> $VAL,node-B,99,5200,6,8,0,1*10
    ```

The answer to the `SUB` is the value now; the last line is a notification, sent only because the button changed, and
its `req` (6) is the `seq` of the `SUB`.

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
