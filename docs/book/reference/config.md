<!-- Generated from the source by `workshop_core::docgen`. Do not edit: run `UPDATE_DOCS=1 cargo test-host`. -->

# Configuration

Build values come from `NodeConfig::DEFAULT`, overridden per node by a preset (`firmware/uart/presets/*.toml`).
Invalid values fail the build; at runtime `set` applies the same checks.

## Runtime settings (`set <key> <value>`)

| Key | Meaning | Accepted | Default |
|---|---|---|---|
| `data_period_ms` | period of the DAT frame sent to the peer (tx_mode periodic) | 10-3600000 ms, or a frequency: 2.5Hz | `2500` |
| `status_period_ms` | period of the stats line in the console | 100-3600000 ms, or a frequency | `10000` |
| `link_timeout_periods` | link DOWN after this many peer periods without a frame | 2-100 | `3` |
| `debounce_ms` | button: line must be stable this long before an edge counts, ms | 1-1000 | `30` |
| `lockout_ms` | button: changes ignored this long after an accepted edge, ms | 0-5000 | `150` |
| `rgb.pattern` | RGB LED fade pattern | name or number, see `list` | `viridis` |
| `rgb.freq_hz` | RGB LED refresh rate | 1-200 Hz, or a period: 20ms | `50` |
| `rgb.cycle_ms` | duration of one fade loop (lower = faster) | 200-60000 ms, or a frequency | `6000` |
| `rgb.brightness` | RGB LED brightness, % | 0-100 | `40` |
| `led1` | breadboard LED mode | off on blink [period|freq] button peer link rx tx | `peer` |
| `led2` | breadboard LED mode | off on blink [period|freq] button peer link rx tx | `rx` |
| `verbose` | console detail: a live line, every frame, or the raw wire | 0 live, 1 frames, 2 wire (or next) | `live` |
| `tx_mode` | when this node sends frames on its own | periodic [period|freq], on-rx, quiet | `periodic` |
| `burst.count` | frames per burst (default of `burst`) | 1-100000 | `1000` |
| `burst.rate_hz` | burst rate, frames/s (default of `burst`) | 0.1-20000 Hz, or max (= 0, back to back) | `0` |
| `console` | where the console output goes (rtt, both: build with --features rtt) | auto serial rtt both | `auto` |

## Fixed at build time

- `name`
- `pins.uart_tx`
- `pins.uart_rx`
- `pins.button`
- `pins.led1`
- `pins.led2`
- `shared.baud_rate`

`shared.baud_rate` = 5000000 baud, 8N1: must be identical on both nodes.
