<!-- Generated from the source by `workshop_core::docgen`. Do not edit: run `UPDATE_DOCS=1 cargo test-host`. -->

# Frame protocol

UART1, 5000000 baud, 8N1. One frame = one ASCII line, at most 96 bytes with its `\n`.
The checksum `CS` is the XOR of every byte between `$` and `*`, as 2 hex digits (NMEA style).
The links, the sequence diagrams and the address space frames (`BRW` ... `VAL`) are in [Protocols](../protocols.md) and [Address space](space.md).

| Frame | Format | Sent |
|---|---|---|
| `DAT` | `$DAT,<node>,<seq>,<t_ms>,<btn>,<events>,<period_us>*CS` | every `data_period_ms` |
| `EVT` | `$EVT,<node>,<seq>,<t_ms>,<RISE\|FALL>,<key>*CS` | on each debounced button edge |
| `BAU` | `$BAU,<node>,<seq>,<t_ms>,<baud>,<REQ\|ACK>*CS` | CLI `baud`: speed change handshake |
| `ECH` | `$ECH,<node>,<seq>,<t_ms>,<echo_seq>*CS` | in `tx_mode on-rx`, for each received frame |
| `BST` | `$BST,<node>,<burst>,<t_ms>,<index>,<count>,<fill>*CS` | CLI `burst` |

| Field | Meaning |
|---|---|
| `node` | sender's name, 1-12 printable ASCII characters without `,` `*` `$` |
| `seq` | sequence number, shared by DAT and EVT, wraps after 4294967295: gaps = lost frames |
| `t_ms` | sender's uptime when the frame was built, ms |
| `btn` | `1` if the sender's button is pressed, else `0` |
| `events` | EVT frames sent since boot |
| `period_us` | sender's DAT period in µs (12.5 ms = 12500): the receiver uses it for its link timeout |
| `RISE / FALL` | jumper removed (released) / plugged to GND (pressed) |
| `key` | 64-bit random number from the sender's TRNG, 16 hex digits |
| `baud / REQ / ACK` | requested speed; the peer answers `ACK` then switches, the requester switches on the `ACK` |
| `echo_seq` | `seq` of the frame answered: its sender gets the round-trip time |
| `burst` | in `BST`, replaces `seq`: burst number |
| `index / count` | position in the burst (from 0) and its length |
| `fill` | value derived from `index`, checked on reception on top of the checksum |

## Examples

- DAT from `node-A`, seq 7:

    ```text
    $DAT,node-A,7,12500,1,2,2500000*08
    ```

- DAT from `node-B`, seq 0:

    ```text
    $DAT,node-B,0,0,0,0,10000*0F
    ```

- EVT from `node-A`, seq 42:

    ```text
    $EVT,node-A,42,123456,FALL,0123456789ABCDEF*07
    ```

- EVT from `ABCDEFGHIJKL`, seq 4294967295:

    ```text
    $EVT,ABCDEFGHIJKL,4294967295,4294967295,RISE,FFFFFFFFFFFFFFFF*6A
    ```

- BAU from `node-A`, seq 8:

    ```text
    $BAU,node-A,8,13000,1000000,REQ*6B
    ```

- BAU from `node-B`, seq 3:

    ```text
    $BAU,node-B,3,13004,1000000,ACK*68
    ```

- ECH from `node-B`, seq 4:

    ```text
    $ECH,node-B,4,13100,8*1E
    ```

- BST from `node-A`, seq 2:

    ```text
    $BST,node-A,2,14000,0,1000,9E3779B97F4A7C15*66
    ```

- BST from `node-A`, seq 2:

    ```text
    $BST,node-A,2,14002,999,1000,08B37C993AF4B208*61
    ```

