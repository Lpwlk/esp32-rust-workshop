# Testing

Four levels, from the fastest to the closest to the hardware:

| Level | Command | Runs on | What it proves | Time |
|---|---|---|---|---|
| Host tests | `cargo test-host` | your computer | the logic is right (protocol, config, CLI, debouncing…) | ~1 s |
| Lints and build | `python3 scripts/dev.py check` | your computer | what CI checks: clippy, firmware build, docs site | ~1 min |
| On-target tests | `python3 scripts/dev.py test target a` | one board | the RISC-V core gives the same results; the hardware behaves as assumed | ~30 s |
| System test | `python3 scripts/dev.py flash all` | two boards | the whole thing: link, CLI, LEDs, bursts | by hand |

The split follows the code: everything that doesn't touch a peripheral lives in
`firmware/shared/core` and is tested on the host; only the hardware glue (UART,
GPIO, RMT, USB, interrupts) is left for the boards. See
[Architecture](architecture.md).

## Host tests

```sh
cargo test-host                  # or: python3 scripts/dev.py test host
```

`cargo test-host` is an alias (`.cargo/config.toml`) for
`cargo test -p workshop-core --target host-tuple`. The workspace builds for the
ESP32-S31 by default; `host-tuple` means "this computer", whatever it is (Mac,
Linux, a Raspberry Pi…).

It runs three test binaries:

| Binary | Tests | What |
|---|---|---|
| `workshop_core` (unit + property) | 101 | the `#[cfg(test)]` modules of each source file |
| `docs` (`tests/docs.rs`) | 1 | the generated reference pages match the code |
| doctests | 16 | every code example of the API documentation, compiled and run |

### Unit tests, by module

| Module | Tests | Highlights |
|---|---|---|
| `config` | 17 | the `console` routes (never silent without RTT); every documented range is the one enforced; every header pin accepted or refused for the right reason; every key round-trips through `set`; `reset` restores build values; values fit their buffers |
| `persist` | 8 | saved settings come back exactly; only differences are stored; every setting changed still fits; erased flash, any flipped bit, a bad length; another node's record; unknown keys and refused values skipped; CRC-32 check value |
| `cli` | 14 | every command and alias parses; **every example of the help table is a valid command accepted by the config**; error messages; rates with `k`/`M` suffixes; the `burst` questions (defaults, retries, cancel) |
| `protocol` | 11 | known frames encode to known lines; the longest possible frame fits in 96 bytes; malformed lines and bad fields are refused even with a valid checksum; lowercase checksums are refused |
| `editor` | 7 | cursor moves and edits in the middle; history keeps the line being typed; escape sequences never end up in the line; the line never overflows |
| `fx` | 6 | colors equal the shared test vectors; each key color is reached exactly; brightness 0 is black; **no visible jump** between two refreshes, even where the loop wraps |
| `button` | 6 | debounce, lockout, bounces that come back to the same level |
| `baud` | 6 | handshake then confirmation; no ACK keeps the speed; a silent link reverts to the boot speed; crossed requests |
| `burst` | 5 | paced schedule without drift, decimal rates, losses counted, stale bursts, throughput and line share |
| `wire` | 4 | byte times, end-of-line corrections on reception, end of a sent frame, the shared TX/RX `DAT` clock (negative deltas included) |
| `units`, `seq`, `leds`, `line`, `rtt`, `docgen` | 17 | periods and frequencies with units, lost-frame counting, LED modes, line splitting, round-trip times, a reason for every reserved pin |

### Property tests

[proptest](https://docs.rs/proptest) runs each property on hundreds of random
inputs, and shrinks a failure to the smallest input that still fails:

| Property | Module |
|---|---|
| any frame survives encode → parse unchanged | `protocol` |
| **any single corrupted byte is detected** | `protocol` |
| the parser never panics, whatever arrives on the wire | `protocol` |
| the same lines come out however the byte stream is chunked | `line` |
| lost frames are counted exactly, including across the `u32` wrap-around | `seq` |
| a restart from 0 is detected | `seq` |

When a property fails, proptest saves the failing case in
`firmware/shared/core/proptest-regressions/` and replays it first on every run:
**commit that file**. It already holds one real find: a corrupted byte that only
changed the case of a checksum letter (`*6A` → `*6a`) used to be accepted.

### Test vectors

`workshop_core::test_vectors` holds expected results shared by the host tests and
the on-target tests:

- `FRAMES`: frames and their exact line on the wire. The checksums were computed
  by an independent script, not by the code under test.
- `COLORS`: RGB colors at chosen fade positions (between keys, near the wrap,
  several brightness levels), generated on the host.

The on-target tests check that the ESP32-S31 (RISC-V, hardware single-precision
float) gives exactly the same bytes.

### Running part of the tests

```sh
cargo test-host -- protocol::          # one module
cargo test-host -- burst --nocapture   # see println! output
cargo test-host --doc                   # doctests only
cargo test-host --test docs             # the docs freshness check only
```

### Where to put a new test

- Logic that doesn't need a peripheral: put it in `workshop-core`, next to the
  code, in its `#[cfg(test)] mod tests`.
- A rule that must hold for any input (parsing, encoding): add a property in the
  module's `proptest!` block.
- An example worth showing in the API docs: write it as a doctest (`///` with a
  code block); it's then documentation *and* test.
- Hardware behavior: `firmware/uart/tests/hil.rs` (below).

## On-target tests

```sh
python3 scripts/dev.py test target a      # tests on node A's board, then reflash it
```

`firmware/uart/tests/hil.rs` uses [embedded-test](https://docs.rs/embedded-test): each `#[test]`
runs on the board, which is reset between tests, and results come back through
probe-rs (semihosting). No wiring is needed.

| Test | Checks |
|---|---|
| `frames_match_the_host_vectors` | encode/parse on the RISC-V core = `FRAMES` |
| `colors_match_the_host_vectors` | float color math on the RISC-V core = `COLORS` |
| `default_config_is_valid` | `NodeConfig::DEFAULT.validate()` on target |
| `trng_is_not_frozen_once_enabled` | 16 random values, at least 15 distinct (the esp-hal 1.2.2 gap) |
| `uart_loopback_at_5_mbaud` | a frame sent at 5 Mbaud comes back through the real receive path (interrupt + ring buffer), using the UART's internal loopback |
| `default_gpios_drive_and_read_back` | LED and button pins can be driven and read at the pad |

!!! warning "Status on the ESP32-S31 (probe-rs 0.32)"
    The tests compile, but running them is **not reliable yet**:

    - probe-rs writes the image with a minimum chip revision of v3.4, which our
      v0.0 boards refuse. `dev.py test target a` therefore **reflashes the node's
      firmware with espflash afterwards**. The raw `cargo test-target` doesn't, and
      leaves the board boot-looping until you run `cargo run`.
    - probe-rs lost the USB connection when starting the first test, even with
      esp-generate's minimal example: a probe-rs / ESP32-S31 issue, not a test issue.

## System test (two boards)

```sh
python3 scripts/dev.py flash all
python3 scripts/dev.py monitor a        # and `monitor b` in another terminal
```

Wire the boards first ([Wiring](wiring.md)). What to look at:

| What | Good sign |
|---|---|
| Stats line (every `status_period_ms`) | `lost 0 · bad 0 ── ● link UP` |
| `Δ` on each `DAT` line | the time since the previous `DAT`, **sent or received** (`Δ   904.288 ms ← RX`, right-aligned so the decimal points line up), measured on the wire; with both nodes at 2.5 s, the TX→RX and RX→TX deltas add up to 2500 ms |
| Unplug and replug the button jumper | one `EVT` per edge, `bounces filtered` when it bounced |
| `verbose wire` | each frame on the wire, its checksum, the RX buffer peak |
| `burst 1000 max` on A | B shows `1000/1000 frames · 0 lost · 0 bad` and the throughput |
| `baud 1M` | `switch confirmed` on both sides, the link stays UP |
| `tx on-rx` on B | A shows `rtt` on each `ECH` and min/avg/max in its stats line |

## Continuous integration

`.github/workflows/ci.yml` runs on every push and pull request:

1. **format**: `cargo fmt --all -- --check`;
2. **host tests**: `cargo test-host` (unit, property, doc tests, docs freshness),
   the [examples](examples.md) (each one runs, with its `assert!`s), then clippy on
   `workshop-core` with `-D warnings`;
3. **firmware**: `cargo build --release` and clippy with `-D warnings`, for each
   Cargo feature combination (default, none, all, `rtt` alone), plus the Flash / RAM
   table in the job summary;
4. **site**: the documentation, once formatting and host tests pass (see
   [Formatting and generated docs](automation.md)).

`python3 scripts/dev.py check` runs the same steps locally, in the same order. The
on-target tests aren't in CI: GitHub's runners have no boards. A self-hosted runner
with the boards plugged in (a Raspberry Pi, for example) could run them.
