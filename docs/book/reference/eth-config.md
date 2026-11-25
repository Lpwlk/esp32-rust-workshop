<!-- Generated from the source by `workshop_core::docgen`. Do not edit: run `UPDATE_DOCS=1 cargo test-host`. -->

# Configuration (Ethernet app)

Build values come from `NodeConfig::DEFAULT_NET`, overridden per node by a preset (`firmware/ethernet/presets/*.toml`).
Invalid values fail the build; at runtime `set` applies the same checks.

## Runtime settings (`set <key> <value>`)

| Key | Meaning | Accepted | Default |
|---|---|---|---|
| `data_period_ms` | period of the DAT frame sent to the peer (tx_mode periodic) | 10-3600000 ms, or a frequency: 2.5Hz | `2500` |
| `status_period_ms` | period of the stats line in the console | 100-3600000 ms, or a frequency | `10000` |
| `link_timeout` | link DOWN after this many peer periods without a frame | 2-100 | `3` |
| `debounce_ms` | button: line must be stable this long before an edge counts, ms | 1-1000 | `30` |
| `lockout_ms` | button: changes ignored this long after an accepted edge, ms | 0-5000 | `1000` |
| `rgb.pattern` | RGB LED fade pattern | name or number, see `list` | `viridis` |
| `rgb.freq_hz` | RGB LED refresh rate | 1-200 Hz, or a period: 20ms | `50` |
| `rgb.cycle_ms` | duration of one fade loop (lower = faster) | 200-60000 ms, or a frequency | `6000` |
| `rgb.brightness` | RGB LED brightness, % | 0-100 | `40` |
| `led1` | breadboard LED mode | off on blink [period|freq] button peer link rx tx | `peer` |
| `led2` | breadboard LED mode | off on blink [period|freq] button peer link rx tx | `rx` |
| `verbose` | console detail: nothing, a live line, every frame, or the details behind each | 0 silent, 1 live, 2 frames, 3 wire (or next) | `live` |
| `tx_mode` | when this node sends frames on its own | periodic [period|freq], on-rx, quiet | `periodic` |
| `burst.count` | frames per burst (default of `burst`) | 1-100000 | `1000` |
| `burst.rate_hz` | burst rate, frames/s (default of `burst`) | 0.1-20000 Hz, or max (= 0, back to back) | `0` |
| `console` | where the console output goes (rtt, both: build with --features rtt) | auto serial rtt both | `auto` |
| `trace` | network trace (Ethernet firmware): control frames, or every frame decoded layer by layer | 0 off, 1 events, 2 frames (or next) | `off` |
| `trace_bytes` | network trace: raw bytes of each frame shown as a hex dump | 0-256 (0 = no dump) | `32` |
| `trace_view` | network trace: how a frame is laid out, on one line, layer by layer, or the node's frame first | compact, layers, inside-out | `layers` |
| `trace_dash` | the traffic dashboard above the prompt (graphs of what is sent and received), with or without the trace | 0 off, 1 mini, 2 normal, 3 full (or next) | `off` |
| `trace_dash_view` | what the dashboard draws (bars per direction, a line chart of the bit rate, the chart and the protocol ranking) | bars, chart, mix (or next) | `bars` |
| `trace_window_s` | time the dashboard's graphs cover | 2-3600 s (30, 30s, 2min) | `30` |
| `trace_short_ip` | network trace, compact view: write this subnet's addresses short (.12) | on, off | `off` |

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
| `net.port` | port of the frames (UDP, or TCP server), same on both nodes; `gen` uses the next one | 1024-65534 | `7000` |
| `net.tcp_role` | TCP only: who opens the connection | server, client | `client` |
| `net.max_speed` | fastest link speed allowed, Mbit/s (1000 needs the MPLL at 500 MHz) | 100, 1000 | `1000` |

## Colors (`[colors]` in a preset)

Build time only: the console names a role, the build chooses its color. A value is the SGR parameters of an ANSI
escape code, numbers separated by `;` (`"31"` red, `"1;33"` bold yellow, `"38;5;208"` 256-color,
`"38;2;255;128;0"` RGB), or `""` for no color.

| Key | What it colors | Default |
|---|---|---|
| `colors.time` | the time stamp at the start of a line | `90` |
| `colors.dim` | labels and secondary text | `90` |
| `colors.sep` | the `│` between the columns of a line | `90` |
| `colors.tx` | the `TX ▶` arrow, and what was sent | `36` |
| `colors.rx` | the `RX ◀` arrow, and what was received | `32` |
| `colors.node` | node names, and the peer's | `33` |
| `colors.ok` | good news: link up, checks passed | `32` |
| `colors.warn` | things to look at: lost frames, control frames | `33` |
| `colors.error` | errors, bad checksums, link down | `31` |
| `colors.accent` | frames of the cards, names of settings and commands | `36` |
| `colors.info` | ordinary data frames in the trace | `34` |
| `colors.prompt` | the `cli ›` prompt and the echo of what you typed | `35` |
| `colors.link` | link events and the negotiated speed | `35` |
| `colors.dat` | the `DAT` badge (periodic data) | `30;47` |
| `colors.evt` | the `EVT` badge (button events) | `30;43` |
| `colors.lnk` | the `ECH`/`BAU` badge (link control) | `30;45` |
| `colors.eth` | trace: the Ethernet layer | `34` |
| `colors.ip` | trace: the IPv4 and ARP layers | `35` |
| `colors.transport` | trace: the UDP, TCP and ICMP layer | `36` |
| `colors.app` | trace: the node's own frames inside the packet | `33` |
| `colors.raw` | trace: the hex dump | `90` |
| `colors.mix_udp` | dashboard protocol ranking: the UDP bar (a background) | `30;44` |
| `colors.mix_tcp` | dashboard protocol ranking: the TCP bar | `30;42` |
| `colors.mix_arp` | dashboard protocol ranking: the ARP bar | `30;43` |
| `colors.mix_icmp` | dashboard protocol ranking: the ICMP bar | `30;45` |
| `colors.mix_ip` | dashboard protocol ranking: other IPv4 protocols | `30;46` |
| `colors.mix_other` | dashboard protocol ranking: everything else | `30;100` |
