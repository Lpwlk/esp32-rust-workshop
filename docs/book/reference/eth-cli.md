<!-- Generated from the source by `workshop_core::docgen`. Do not edit: run `UPDATE_DOCS=1 cargo test-host`. -->

# CLI commands (Ethernet app)

Type these in the USB console (`cd firmware/ethernet && cargo run --release --bin node-a`).
Changes are applied immediately and lost on reboot.

## Settings

| Command | Aliases | Summary |
|---|---|---|
| `config` | `cfg`, `get` | all settings: current value, build value, read-only ones |
| `set <key> <value>` | `s` | change a setting, applied immediately |
| `reset [key]` | `r` | back to the build value (one key, or all) |

### `config`

```text
Lists every setting of this node. Values changed at runtime are shown
in yellow next to their build value (from the node's preset).
Read-only settings (name, pins, link) need a rebuild to change.
```

Examples:

```text
config
```

### `set`

```text
Keys are the field names of NodeConfig (see `config` for the list,
`help <key>` for one key). The value goes through the same checks as
the build config: if it's refused, nothing changes.
Changes are lost on reboot (back to the build values).
```

Examples:

```text
set data_period_ms 1000
set data_period_ms 2.5Hz
set rgb.pattern ocean
set led1 blink 200
```

### `reset`

```text
Without a key, restores every setting changed with `set`.
```

Examples:

```text
reset rgb.brightness
reset
```

## Shortcuts

| Command | Aliases | Summary |
|---|---|---|
| `verbose [0|1|2|3|silent|live|frames|wire]` | `v` | how much the console prints (= set verbose) |
| `pattern <name|1-5>` | `p` | RGB LED pattern (= set rgb.pattern) |
| `next` | `n` | next RGB LED pattern |
| `freq <Hz|period>` | `f` | RGB LED refresh rate (= set rgb.freq_hz) |
| `cycle <ms|freq>` | `c` | duration of one fade loop (= set rgb.cycle_ms) |
| `bright <0-100>` | `b` | RGB LED brightness, % (= set rgb.brightness) |
| `led <1|2> <mode>` | `l` | breadboard LED mode (= set led1 / set led2) |

### `verbose`

```text
0 silent  only what you ask for (replies, burst results) and what is wrong
          (bad frames, frames not sent): no live line, no frames, no link events
1 live    one line, updated in place, with the last frame sent or received and
          the counters. Lost frames, link events, bursts and errors still scroll.
2 frames  every frame on its own line, and the periodic stats line
3 wire    frames plus what is behind each: the time since the peer's previous frame,
          ignored button bounces and counters (UART: also the raw line, checksum
          and size; on Ethernet the raw bytes are `trace`'s)
`verbose` alone goes to the next level: live, frames, wire, silent, and round again.
```

Examples:

```text
verbose
verbose frames
verbose silent
verbose 3
```

### `pattern`

```text
Name or number from `list`, which shows a color preview of each.
```

Examples:

```text
pattern viridis
p 3
```

### `next`

```text
Cycles through the patterns in `list` order.
```

Examples:

```text
next
```

### `freq`

```text
How often the LED color is recomputed: smoothness, not speed.
1-200 Hz, decimals allowed (37.5), or a period (20ms = 50 Hz).
To make the fade faster, lower `cycle` instead.
```

Examples:

```text
freq 100
freq 37.5
freq 20ms
```

### `cycle`

```text
Lower = faster fade. 200-60000 ms, decimals allowed (2500.5),
with a unit (2.5s), or as a frequency (0.5Hz = one loop every 2 s).
```

Examples:

```text
cycle 2000
cycle 2.5s
cycle 0.5Hz
```

### `bright`

```text
Gamma-corrected: 50 % looks about half as bright as 100 %.
```

Examples:

```text
bright 70
```

### `led`

```text
off, on        always off / on
blink [p|f]    blinks: full period (200, 0.5s) or frequency (2Hz), default 500 ms
button         on while this node's button is pressed
peer           on while the peer's button is pressed
link           on while the link is up
rx, tx         short flash on each received / sent frame
```

Examples:

```text
led 1 peer
led 2 blink 200
led 2 blink 2.5Hz
```

## Link tests

| Command | Aliases | Summary |
|---|---|---|
| `link` | `net`, `lk` | the Ethernet link: cable, speed, addresses, transport, counters |
| `trace [0|1|2|off|events|frames|compact|layers|inside-out]` | `t`, `tr` | decode the network frames on the port (= set trace, or trace_view) |
| `dash [bars|chart|mix | off|mini|normal|full]` | `ds` | the traffic dashboard above the prompt (= set trace_dash[_view]) |
| `gen [start|stop|pause|resume|rate|size|shape|for|count] ...` | `g` | traffic generator: UDP datagrams to the peer, finely controlled |
| `window <2-3600 seconds>` | `win` | the time the dashboard's graphs cover (= set trace_window_s) |
| `dump <0-256>` | `d` | raw bytes of each traced frame as a hex dump (= set trace_bytes) |
| `burst [count [rate|max]]` | `bs` | send a burst of frames and measure the link |
| `tx <periodic [period]|on-rx|quiet>` |  | when this node transmits (= set tx_mode) |
| `period <period|freq>` | `pd` | DAT period (= set data_period_ms) |

### `link`

```text
Shows what the PHY negotiated (speed, duplex), this node's address and the
peer's, UDP or TCP and the port, and the frame counters. The speed and the
addresses are fixed at build time (presets/*.toml); `baud` is the UART equivalent.
```

Examples:

```text
link
```

### `trace`

```text
0 off     nothing (the driver copies no frame; `dash` still counts them)
1 events  one line per control frame: ARP, ICMP, TCP SYN/FIN/RST, the undecodable
2 frames  every frame decoded, checksums checked, and `dump` bytes as a hex dump
`trace` alone goes to the next level (frames goes back to off).
Independent of `verbose`. How a frame is laid out (= set trace_view):
compact     one line per frame: who to whom, flags, the node's frame
layers      a header line, then Ethernet, IPv4, UDP/TCP, the node's frame, raw bytes
inside-out  the node's frame first, then the layers back out to the cable
```

Examples:

```text
trace
trace events
trace frames
trace compact
trace inside-out
t 0
```

### `dash`

```text
Graphs of what is received and sent, at the bottom of the console above the
prompt, redrawn in place. It counts the frames itself, with `trace` on or off.
Off at boot: `dash normal` shows it. A view, and a size:
bars   vertical bars per direction (▁ to █, a column per slice of time), with a
       scale, a time axis and its rate (default)
chart  the classic network graph: rx and tx as two lines on the same axes, bit rate
       over the window, with a time axis, packets per second, peak, link use
mix    the chart, and under it the protocols that went by as horizontal bars
off | mini | normal | full   how many lines (0 off, 1 mini, 2 normal, 3 full)
`dash` alone goes to the next view; `dash next` to the next size.
`window` sets the time the graphs cover.
```

Examples:

```text
dash
dash chart
dash mix
dash full
dash off
```

### `gen`

```text
gen start [rate] [size] [shape] [time]   in any order; the rest stays as it was
gen rate R | size S | shape P | for T | count N | stop | pause | resume | status
R rate: 5000 (datagrams/s), 5kpps, or on the wire 2Mbit, 500kbit, 1Gbit.
  With a shape it is the FLOOR.
S size of the payload: 512B, 64-1400 (random), imix (7 small, 4 medium, 1 large)
T time: 30s, 2min, forever, auto. Default auto: as long as the shape (one cycle),
  10 s at least.
P shape (it has a colon): the second value is the PEAK, the last is one CYCLE,
  not the length of the stream:
  sine:3Mbit:20s  cosine:...  triangle:...  saw:...  square:...
  ramp:3Mbit:30s  steps:3Mbit:5:30s  pulse:200ms:800ms  random
A wave repeats for as long as the stream runs: add `forever` (or a time).
```

Examples:

```text
gen start 2Mbit 512B 30s
gen start 200kbit triangle:2Mbit:10s forever
gen start 200kbit sine:2Mbit:20s forever
gen start 5000 imix
gen rate 50Mbit
gen stop
```

### `window`

```text
Each column of a graph is a slice of it: 30 s over 40 columns is 750 ms a column.
Seconds, or with a unit: 30, 30s, 2min. Changing it starts the graphs over.
```

Examples:

```text
window 30
window 2min
win 10s
```

### `dump`

```text
0 shows no dump. The headers of any frame fit in 134 bytes;
a frame is cut after 256.
```

Examples:

```text
dump 64
d 0
dump 256
```

### `burst`

```text
Without arguments, asks the length then the speed (Enter = the default
in brackets, from `burst.count` / `burst.rate_hz`; q cancels).
rate = frames per second, decimals allowed (37.5), or a period between
frames (20ms); max (or 0) = back to back at line speed.
Frames are counted, not printed. At the end this node shows what it sent,
the peer what it received: lost, corrupted, throughput, % of the line.
```

Examples:

```text
burst
burst 1000 max
burst 200 50
burst 100 2.5Hz
```

### `tx`

```text
periodic   a DAT frame every data_period_ms (default); `tx periodic 2.5Hz`
           or `tx periodic 400ms` also sets the period
on-rx      no DAT: an ECH reply to each frame from the peer; the peer then
           shows the round-trip time (rtt) of its frames
quiet      nothing on its own (button events and link handshakes only)
```

Examples:

```text
tx on-rx
tx periodic
tx periodic 2.5Hz
tx periodic 12.5ms
```

### `period`

```text
A period (12.5 = 12.5 ms, 250ms, 2.5s) or a frequency (4Hz = 250 ms).
Sent in each DAT frame, so the peer adapts its link timeout.
```

Examples:

```text
period 1s
period 12.5
period 4Hz
```

## Address space (the peer)

| Command | Aliases | Summary |
|---|---|---|
| `browse [path] [depth]` | `br`, `tree` | list what the peer has, as a tree: its address space |
| `read <path>` | `rd` | read one of the peer's variables |
| `write <path> <value>` | `wr` | write one of the peer's variables |
| `watch <path> [interval]` | `wa` | the peer reports a variable whenever it changes |
| `unwatch [path]` | `uw` | stop watching one variable, or all of them |

### `browse`

```text
Asks the peer to describe itself, one node per frame: its LEDs, buttons,
statistics and settings as a tree. `browse` alone lists the root `/`;
`browse /leds` lists one branch. `depth` (1 to 4, default 1) is how many
levels to go down: `browse / 2` shows the whole tree (`-d 2` and `--depth 2`
work too). Each line shows the node, and for a variable its type and whether
it can be written (`rw`) or only read (`ro`).
```

Examples:

```text
browse
browse /leds
browse / 2
browse /config -d 1
```

### `read`

```text
The answer is the value, and a status: Good, or why not (BadNodeIdUnknown,
BadTypeMismatch for an object...). Paths come from `browse`.
```

Examples:

```text
read /buttons/jumper
read /stats/lost
read /node/uptime
```

### `write`

```text
The peer checks the value like its own `set` does, and answers with the value now
in force: what you wrote, or what stayed if it was refused (BadOutOfRange,
BadTypeMismatch, BadNotWritable for a read-only variable).
```

Examples:

```text
write /leds/1 on
write /leds/2 blink 200
write /rgb/brightness 80
write /config/verbose frames
```

### `watch`

```text
The peer checks the variable every `interval` (default 250ms; 4Hz, 1s work too) and
sends it only when it differs from the last one reported. A few variables can be
watched at once; `unwatch` stops one, or all.
```

Examples:

```text
watch /buttons/jumper
watch /stats/rx 1s
watch /node/uptime 500ms
```

### `unwatch`

Examples:

```text
unwatch /buttons/jumper
unwatch
```

## Information

| Command | Aliases | Summary |
|---|---|---|
| `info` | `about`, `boot`, `i` | the boot card again: link, node, build, chip, LEDs |
| `list` | `ls` | RGB patterns with a color preview |
| `status` | `st` | current state of the RGB LED and breadboard LEDs |
| `help [command|key]` | `h`, `?` | this list, or details on one command or setting |

### `info`

```text
Shows the current settings (those changed with `set` included).
```

Examples:

```text
info
```

### `list`

```text
The current pattern is marked with ▶.
```

Examples:

```text
list
```

### `status`

Examples:

```text
status
```

### `help`

Examples:

```text
help set
help data_period_ms
```

## Saved settings and restart

| Command | Aliases | Summary |
|---|---|---|
| `save` | `w` | keep the current settings across reboots (in flash) |
| `forget` | `fg` | erase the saved settings (next boot: build config) |
| `reboot` | `restart`, `rb` | restart the chip (software reset) |

### `save`

```text
Stores the settings that differ from the build config in flash; they are
applied at every boot, on top of the build config. Reflashing the firmware
keeps them (espflash doesn't touch that area); `forget` erases them.
Needs the `persist` Cargo feature (on by default).
```

Examples:

```text
save
```

### `forget`

```text
The current settings stay as they are until the next reboot;
`reset` then `reboot` to go back now.
```

Examples:

```text
forget
```

### `reboot`

```text
Settings not saved with `save` are lost.
The boot card then shows a software reset (`CoreSw`).
```

Examples:

```text
reboot
```
