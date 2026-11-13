# Presets

A **preset** is a named, documented configuration of a node, in TOML. Both apps use them:

| Folder | App | Built by | Default presets |
|---|---|---|---|
| [`uart/`](uart) | the UART firmware (repository root) | `cargo run --release --bin node-a` | `node-a`, `node-b` |
| [`eth/`](eth) | the Ethernet firmware (`eth-lab/`) | `cd eth-lab && cargo run --release --bin node-a` | `node-a`, `node-b`, `node-a-tcp`, `node-b-tcp` |

The build script reads the preset, checks it with the same code as the CLI's `set` command, and
turns it into a `const NodeConfig` that the compiler checks once more. **A mistake stops the build**
and names the file and the key, nothing reaches the board half-configured.

## Where to look

- **`base.toml`** of each folder is the reference: every setting, with its meaning, its range and its
  default, written out. A test keeps it equal to the defaults of the firmware, so it can't lie.
- `node-a.toml` / `node-b.toml` only list what differs from `base`: the node's name (and, for
  Ethernet, its address and its peer's).
- The same settings are described, generated from the code, in
  [`docs/book/reference/config.md`](../docs/book/reference/config.md) (UART) and
  [`eth-config.md`](../docs/book/reference/eth-config.md) (Ethernet).

## Writing a preset

```toml
# presets/eth/bench.toml: node-A with a fast, quiet link
extends = "node-a"            # start from another preset of the same folder (chains are fine)

data_period_ms = 100          # plain settings first...
tx_mode        = "periodic"
led2           = "blink 2.5Hz"

[rgb]                         # ...then the [sections]: pins, net, rgb, burst
pattern = "ocean"
```

- **Keys** are the ones of `set <key> <value>` in the CLI, plus the build-time ones: `name`, `[pins]`
  and, on Ethernet, `[net]`. Sections are `pins`, `net`, `rgb` and `burst`.
- **Values** are what the CLI accepts: numbers, booleans, and strings with units
  (`"2.5Hz"`, `"250ms"`, `"blink 200"`).
- **Order doesn't matter**, with one rule: an explicit `data_period_ms` wins over a period written in
  `tx_mode = "periodic 2.5Hz"`.
- **A key written after a `[section]` header belongs to that section.** Put plain settings
  *above* the first section. If you forget, the error says so (`pins.led1: expected a GPIO number`
  and a hint).
- The file name is the preset's name: letters, digits, `-` and `_`.

## Building a preset

| What | Command |
|---|---|
| the default preset of each node | `cargo run --release --bin node-a` |
| another preset, one node | `PRESET_NODE_A=bench cargo run --release --bin node-a` |
| the same preset for both | `PRESET=bench cargo build --release` |
| through the helper script | `python3 scripts/dev.py --app eth --preset bench flash a` |

Changing a preset file or one of these variables rebuilds the firmware. Build presets of the Ethernet
app from `eth-lab/` (or with `dev.py --app eth`).

## Looking at presets

```sh
cargo presets list              # every preset of both apps, and what it extends
cargo presets show eth node-b   # every setting it results in, build-time ones first
cargo presets check             # load and validate all of them (CI runs this)
```

## What a preset can't change

- The **UART baud rate** (5 Mbaud) is the constant `shared::BAUD_RATE` in
  `crates/workshop-core/src/config.rs`: both boards must agree on it and it isn't a per-node setting.
- Cargo **features** (`persist`, `watchdog`, `rtt`) choose what is compiled in:
  `cargo run --release --features rtt`.
- The **RGB LED** is wired to GPIO60 on the board, and on Ethernet nodes **GPIO5-19** belong to the
  PHY; the pin checks refuse them.
