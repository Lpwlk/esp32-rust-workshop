<!-- Generated from the source by `workshop_core::docgen`. Do not edit: run `UPDATE_DOCS=1 cargo test-host`. -->

# Address space (UA-lite)

A node describes its LEDs, buttons, statistics and settings as a tree; its peer can browse it, read and write
values and subscribe to changes. The frames are the same checksummed lines as the others, so it works over the UART,
UDP and TCP. The explanation is in [Protocols](../protocols.md).

## Frames

| Frame | Format | Meaning |
|---|---|---|
| `BRW` | `$BRW,<node>,<seq>,<t_ms>,<id>,<index>*CS` | browse: child number `index` (from 0) of node `id` |
| `BRR` | `$BRR,<node>,<seq>,<t_ms>,<req>,<id>,<name>,<kind>,<more>*CS` | one child; `id` 65535 = there is none; `more` = 1 if another follows |
| `RED` | `$RED,<node>,<seq>,<t_ms>,<id>*CS` | read a variable |
| `WRT` | `$WRT,<node>,<seq>,<t_ms>,<id>,<value>*CS` | write a variable |
| `SUB` | `$SUB,<node>,<seq>,<t_ms>,<id>,<interval_ms>*CS` | report a variable when it changes, checked every `interval_ms`; 0 cancels (with id 65535: every subscription) |
| `VAL` | `$VAL,<node>,<seq>,<t_ms>,<req>,<id>,<status>,<value>*CS` | a value or why there is none: the answer to RED, WRT and SUB, then every change |

`req` is the `seq` of the request an answer belongs to (a notification carries the `seq` of its `SUB`).
`kind` is three letters: class (`O`bject, `V`ariable), type (`B`ool, `U`32, `T`ext, `-` for an object), access (`R`ead, `W`rite).
Values are text of at most 24 characters without `,` `*` `$`; a bool is `0` or `1`.

## Status codes

| Code | Name | Meaning |
|---|---|---|
| 0 | `Good` | done |
| 1 | `BadNodeIdUnknown` | no such node |
| 2 | `BadNotWritable` | the node can't be written |
| 3 | `BadTypeMismatch` | an object has no value, or the value isn't of the node's type |
| 4 | `BadOutOfRange` | of the right type, refused by the node's own checks (a range, a name...) |
| 5 | `BadTooManySubscriptions` | all subscription slots are taken |

## The address space

What every node has. Ids below 100 are fixed; `/config` has one variable per runtime setting, from id 100.

| Path | Id | Kind | Type | Access | What |
|---|---|---|---|---|---|
| `/` | 0 | object |  | read | the root of the address space |
| `/node` | 1 | object |  | read | this node |
| `/node/name` | 2 | variable | text | read | the node's name, as sent in every frame (fixed at build time) |
| `/node/uptime` | 3 | variable | u32 | read | time since boot, ms |
| `/leds` | 4 | object |  | read | the two breadboard LEDs |
| `/leds/1` | 5 | variable | text | read-write | mode of LED 1: off, on, blink [ms], button, peer, link, rx, tx |
| `/leds/2` | 6 | variable | text | read-write | mode of LED 2, same values as LED 1 |
| `/buttons` | 7 | object |  | read | the two buttons |
| `/buttons/jumper` | 8 | variable | bool | read | 1 while the jumper button is pressed (debounced) |
| `/buttons/boot` | 9 | variable | bool | read | 1 while the board's BOOT button is pressed (debounced) |
| `/rgb` | 10 | object |  | read | the on-board RGB LED |
| `/rgb/pattern` | 11 | variable | text | read-write | fade pattern: viridis, sunset, ocean, aurora, sakura |
| `/rgb/brightness` | 12 | variable | u32 | read-write | 0-100 % |
| `/stats` | 13 | object |  | read | frame counters since boot |
| `/stats/tx` | 14 | variable | u32 | read | frames sent |
| `/stats/rx` | 15 | variable | u32 | read | frames received |
| `/stats/lost` | 16 | variable | u32 | read | frames lost (gaps in the peer's sequence numbers) |
| `/config` | 17 | object |  | read | the runtime settings: one text variable each, as `set` takes them |
| `/config/data_period_ms` | 100 | variable | text | read-write | period of the DAT frame sent to the peer (tx_mode periodic) |
| `/config/status_period_ms` | 101 | variable | text | read-write | period of the stats line in the console |
| `/config/link_timeout_periods` | 102 | variable | text | read-write | link DOWN after this many peer periods without a frame |
| `/config/debounce_ms` | 103 | variable | text | read-write | button: line must be stable this long before an edge counts, ms |
| `/config/lockout_ms` | 104 | variable | text | read-write | button: changes ignored this long after an accepted edge, ms |
| `/config/rgb.pattern` | 105 | variable | text | read-write | RGB LED fade pattern |
| `/config/rgb.freq_hz` | 106 | variable | text | read-write | RGB LED refresh rate |
| `/config/rgb.cycle_ms` | 107 | variable | text | read-write | duration of one fade loop (lower = faster) |
| `/config/rgb.brightness` | 108 | variable | text | read-write | RGB LED brightness, % |
| `/config/led1` | 109 | variable | text | read-write | breadboard LED mode |
| `/config/led2` | 110 | variable | text | read-write | breadboard LED mode |
| `/config/verbose` | 111 | variable | text | read-write | console detail: a live line, every frame, or the raw wire |
| `/config/tx_mode` | 112 | variable | text | read-write | when this node sends frames on its own |
| `/config/burst.count` | 113 | variable | text | read-write | frames per burst (default of `burst`) |
| `/config/burst.rate_hz` | 114 | variable | text | read-write | burst rate, frames/s (default of `burst`) |
| `/config/console` | 115 | variable | text | read-write | where the console output goes (rtt, both: build with --features rtt) |
