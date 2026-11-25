# Wireshark dissectors

A Lua plugin that decodes what the boards put on the Ethernet cable.

| Protocol | Port | What it shows |
|---|---|---|
| `wsnode` | UDP and TCP 7000 (`net.port`) | node frames (`DAT EVT ECH BST BAU`) and address space frames (`BRW BRR RED WRT SUB VAL`): fields, checksum, sequence gaps, ids resolved to paths (`/leds/1`, `/config/led1`), status names, request ⇄ answer links, echo round-trip time |
| `wsgen` | UDP 7001 (`net.port + 1`) | traffic generator datagrams: stream, sequence, loss, order, RFC 3550 jitter |
| `wsadc` | UDP 7100 | ADC stream: `ADC1` samples (rate, loss, min/max/mean), `LOG1`/`RPL1` log lines, `start`/`stop`/`info` commands |

Over TCP the lines are reassembled, so a line cut between two segments is decoded once and one segment may show
several frames. Ports can be changed in *Edit ▸ Preferences ▸ Protocols* (a board whose `net.port` is not 7000 is
also found by its `$TYPE,...*CS` shape).

## Install

```sh
mkdir -p ~/.local/lib/wireshark/plugins
ln -s "$PWD/tools/wireshark/workshop.lua" ~/.local/lib/wireshark/plugins/workshop.lua
```

(`~/.local/lib/wireshark/plugins` is the "Personal Lua Plugins" folder of *Help ▸ About Wireshark ▸ Folders*.)
Restart Wireshark, or *Analyze ▸ Reload Lua Plugins* (Ctrl+Shift+L). Without installing: `tshark -X lua_script:tools/wireshark/workshop.lua -r capture.pcap`.

Capture on the computer's interface wired to the boards (or a switch mirror port): the boards talk to each other, so
a capture from the computer alone only sees what is sent to it (the ADC stream, broadcast ARP...).

## Useful filters

```text
wsnode                                  every node / address space frame
wsnode.tag == "WRT" || wsnode.status > 0     writes, and every refused request
wsnode.lost || wsgen.lost || wsadc.lost      losses, as the nodes count them
wsnode.checksum.good == 0                    frames a node would refuse
wsnode.rtt > 1                               echoes slower than 1 ms
wsnode.path contains "/leds"                 what touched the LEDs
wsgen.jitter_us > 100
wsadc.dropped > 0                            the ADC board lost samples
```

## Samples and checks

```sh
python3 tools/wireshark/make_samples.py   # writes samples/udp.pcap and samples/tcp.pcap
python3 tools/wireshark/check.py          # tshark on both, and the Lua tables against the Rust sources
```

`check.py` fails when `NODES`, `CONFIG_KEYS`, `STATUS`, the ADC header length or a port in `workshop.lua` no longer
matches `space.rs`, `config.rs`, `stream.rs` or `base.toml`: update the table when you add a node or a setting.

A frame the firmware would refuse (bad checksum) is shown but does not move the sequence check, as on the nodes: the
next frame then shows the gap.
