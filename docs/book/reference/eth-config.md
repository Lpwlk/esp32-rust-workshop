<!-- Generated from the source by `workshop_core::docgen`. Do not edit: run `UPDATE_DOCS=1 cargo test-host`. -->

# Configuration (Ethernet app)

Build values come from `NodeConfig::DEFAULT_NET`, overridden per node by a preset (`firmware/ethernet/presets/*.toml`).
Invalid values fail the build; at runtime `set` applies the same checks.

## Runtime settings (`set <key> <value>`)

| Key | Meaning | Accepted | Default |
|---|---|---|---|
| `data_period_ms` | period of the DAT frame sent to the peer (tx_mode periodic) | 10-3600000 ms, or a frequency: 2.5Hz | `2500` |
| `status_period_ms` | period of the stats line in the console | 100-3600000 ms, or a frequency | `10000` |
| `link_timeout_periods` | link DOWN after this many peer periods without a frame | 2-100 | `3` |
| `debounce_ms` | button: line must be stable this long before an edge counts, ms | 1-1000 | `30` |
| `lockout_ms` | button: changes ignored this long after an accepted edge, ms | 0-5000 | `1000` |
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
- `pins.button`
- `pins.led1`
- `pins.led2`
- `net.transport`
- `net.address`
- `net.prefix_len`
- `net.peer`
- `net.port`
- `net.tcp_role`
- `net.max_speed`

## Ethernet link (`[net]` in a preset)

Identical on both nodes, except the two addresses, which are each other's.

| Key | Meaning | Accepted | Default |
|---|---|---|---|
| `net.transport` | how frames travel between the nodes | udp, tcp | `udp` |
| `net.address` | this node's static IPv4 address (no DHCP) | a.b.c.d | `192.168.77.11` |
| `net.prefix_len` | subnet size (24 = 255.255.255.0) | 8-30 | `24` |
| `net.peer` | the other node's address, same subnet | a.b.c.d | `192.168.77.12` |
| `net.port` | port of the frames (UDP, or TCP server), same on both nodes | 1024-65535 | `7000` |
| `net.tcp_role` | TCP only: who opens the connection | server, client | `client` |
| `net.max_speed` | fastest link speed allowed, Mbit/s (1000 needs the MPLL at 500 MHz) | 100, 1000 | `1000` |
