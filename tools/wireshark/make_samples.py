#!/usr/bin/env python3
"""Writes two small captures that exercise workshop.lua, without needing a board.

    python3 tools/wireshark/make_samples.py            # -> tools/wireshark/samples/*.pcap

    samples/udp.pcap   node A <-> node B over UDP 7000 (node frames, the address space, a lost frame, a bad
                       checksum, an echo, a burst), the traffic generator on 7001, the ADC stream on 7100
    samples/tcp.pcap   the same node frames over one TCP connection, with a line cut in two segments

Only the standard library: the pcap is written by hand (Ethernet, IPv4, UDP/TCP with their checksums).
"""

import math
import struct
import sys
from pathlib import Path

MAC_A, MAC_B = bytes.fromhex("020000000011"), bytes.fromhex("020000000012")
MAC_BOARD, MAC_PC = bytes.fromhex("020000000020"), bytes.fromhex("3c22fb000001")
IP_A, IP_B, IP_BOARD, IP_PC = "192.168.77.11", "192.168.77.12", "192.168.77.20", "192.168.77.1"


def ip_bytes(text):
    return bytes(int(p) for p in text.split("."))


def checksum16(data):
    if len(data) % 2:
        data += b"\0"
    total = sum(struct.unpack(f"!{len(data) // 2}H", data))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return ~total & 0xFFFF


def ipv4(src, dst, proto, payload, ident):
    header = struct.pack(
        "!BBHHHBBH4s4s",
        0x45,
        0,
        20 + len(payload),
        ident,
        0x4000,
        64,
        proto,
        0,
        ip_bytes(src),
        ip_bytes(dst),
    )
    header = header[:10] + struct.pack("!H", checksum16(header)) + header[12:]
    return header + payload


def pseudo(src, dst, proto, length):
    return ip_bytes(src) + ip_bytes(dst) + struct.pack("!BBH", 0, proto, length)


def udp(src_ip, dst_ip, sport, dport, payload):
    seg = struct.pack("!HHHH", sport, dport, 8 + len(payload), 0) + payload
    cs = checksum16(pseudo(src_ip, dst_ip, 17, len(seg)) + seg) or 0xFFFF
    return seg[:6] + struct.pack("!H", cs) + seg[8:]


def tcp(src_ip, dst_ip, sport, dport, seq, ack, flags, payload=b""):
    seg = struct.pack("!HHIIBBHHH", sport, dport, seq, ack, 5 << 4, flags, 64240, 0, 0) + payload
    cs = checksum16(pseudo(src_ip, dst_ip, 6, len(seg)) + seg)
    return seg[:16] + struct.pack("!H", cs) + seg[18:]


class Capture:
    def __init__(self):
        self.packets = []
        self.ident = 0

    def add(self, t, src_mac, dst_mac, src_ip, dst_ip, proto, l4):
        self.ident += 1
        frame = dst_mac + src_mac + b"\x08\x00" + ipv4(src_ip, dst_ip, proto, l4, self.ident)
        self.packets.append((t, frame))

    def udp(self, t, src, dst, sport, dport, payload):
        (smac, sip), (dmac, dip) = src, dst
        self.add(t, smac, dmac, sip, dip, 17, udp(sip, dip, sport, dport, payload))

    def tcp(self, t, src, dst, sport, dport, seq, ack, flags, payload=b""):
        (smac, sip), (dmac, dip) = src, dst
        self.add(t, smac, dmac, sip, dip, 6, tcp(sip, dip, sport, dport, seq, ack, flags, payload))

    def write(self, path):
        with open(path, "wb") as out:
            out.write(struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, 1))
            for t, frame in self.packets:
                sec, usec = int(t), round((t % 1) * 1e6)
                out.write(struct.pack("<IIII", sec, usec, len(frame), len(frame)) + frame)


A, B = (MAC_A, IP_A), (MAC_B, IP_B)
BOARD, PC = (MAC_BOARD, IP_BOARD), (MAC_PC, IP_PC)
T0 = 1_700_000_000.0


class Node:
    """A node that numbers its frames the way the firmware does: one counter for all but BST."""

    def __init__(self, name):
        self.name = name
        self.seq = 0

    def line(self, tag, *fields, t_ms=0, seq=None, bad_checksum=False):
        if seq is None:
            seq = self.seq
            self.seq += 1
        body = ",".join([tag, self.name, str(seq), str(t_ms), *map(str, fields)])
        cs = 0
        for byte in body.encode():
            cs ^= byte
        if bad_checksum:
            cs ^= 0x04
        return f"${body}*{cs:02X}\n".encode()

    def skip(self, n=1):
        """Frames that were sent and never arrived."""
        self.seq += n


def burst_fill(index):
    return f"{((index + 1) * 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF:016X}"


def node_conversation(send, faults):
    """Calls `send(t, from, to, line)` for the exchange of two nodes; used over UDP and over TCP.
    `faults`: a frame is lost and one is corrupted (UDP only: TCP loses nothing)."""
    a, b = Node("node-A"), Node("node-B")
    t = 0.0

    def a_to_b(line, dt=0.001):
        nonlocal t
        t += dt
        send(t, A, B, line)

    def b_to_a(line, dt=0.001):
        nonlocal t
        t += dt
        send(t, B, A, line)

    a_to_b(a.line("DAT", 0, 0, 10000, t_ms=0))  # A seq 0
    b_to_a(b.line("DAT", 0, 0, 10000, t_ms=2))  # B seq 0
    a_to_b(a.line("DAT", 0, 0, 10000, t_ms=10), 0.008)  # A seq 1
    a_to_b(a.line("EVT", "FALL", "0123456789ABCDEF", t_ms=15), 0.005)  # A seq 2
    if faults:
        a.skip()  # A seq 3 is lost
    a_to_b(a.line("DAT", 1, 1, 10000, t_ms=30), 0.015)  # A seq 4: one frame lost
    # B echoes (as in tx_mode on-rx): ECH carries the seq of the frame it answers.
    echo_of = a.seq
    a_to_b(a.line("DAT", 1, 1, 10000, t_ms=40), 0.010)  # A seq 5
    b_to_a(b.line("ECH", echo_of, t_ms=41), 0.00031)
    a_to_b(a.line("DAT", 1, 1, 10000, t_ms=50, bad_checksum=faults), 0.010)  # corrupted on the wire

    # Browse /leds, read, write (accepted, then refused), watch.
    req = a.seq
    a_to_b(a.line("BRW", 4, 0, t_ms=100), 0.05)
    b_to_a(b.line("BRR", req, 5, "1", "VTW", 1, t_ms=100), 0.0004)
    req = a.seq
    a_to_b(a.line("BRW", 4, 1, t_ms=101), 0.003)
    b_to_a(b.line("BRR", req, 6, "2", "VTW", 0, t_ms=101), 0.0004)
    req = a.seq
    a_to_b(a.line("BRW", 4, 2, t_ms=102), 0.003)
    b_to_a(b.line("BRR", req, 65535, "", "---", 0, t_ms=102), 0.0004)
    req = a.seq
    a_to_b(a.line("RED", 8, t_ms=110), 0.01)
    b_to_a(b.line("VAL", req, 8, 0, 0, t_ms=110), 0.0004)
    req = a.seq
    a_to_b(a.line("WRT", 5, "blink 200", t_ms=120), 0.01)
    b_to_a(b.line("VAL", req, 5, 0, "blink 200 ms", t_ms=120), 0.0004)
    req = a.seq
    a_to_b(a.line("WRT", 5, "disco", t_ms=130), 0.01)
    b_to_a(b.line("VAL", req, 5, 3, "blink 200 ms", t_ms=130), 0.0004)
    req = a.seq
    a_to_b(a.line("RED", 109, t_ms=135), 0.005)  # /config/led1: 100 + its index in KEYS
    b_to_a(b.line("VAL", req, 109, 0, "blink 200", t_ms=135), 0.0004)
    sub = a.seq
    a_to_b(a.line("SUB", 8, 100, t_ms=140), 0.01)
    b_to_a(b.line("VAL", sub, 8, 0, 0, t_ms=140), 0.0004)
    b_to_a(b.line("VAL", sub, 8, 0, 1, t_ms=260), 0.12)  # notification: the button moved
    a_to_b(a.line("SUB", 65535, 0, t_ms=300), 0.04)  # unwatch all
    b_to_a(b.line("VAL", sub + 1, 65535, 0, "", t_ms=300), 0.0004)

    # A burst: BST frames number the burst, not the sequence.
    for i in range(3):
        a_to_b(a.line("BST", i, 3, burst_fill(i), t_ms=400 + i, seq=7), 0.001 if i else 0.1)
    a_to_b(a.line("BAU", 1000000, "REQ", t_ms=410), 0.01)
    return t


def make_udp():
    cap = Capture()

    def send(t, src, dst, line):
        cap.udp(T0 + t, src, dst, 7000, 7000, line)

    end = node_conversation(send, faults=True)

    # The traffic generator: stream 1 from A to B on 7001, a datagram missing, jitter, then the end mark.
    t = end + 0.5
    boot_us = 5_000_000
    for seq, gap in [(0, 0), (1, 0.0010), (2, 0.0013), (4, 0.0021), (5, 0.0008)]:
        t += gap
        last = seq == 5
        sent_us = boot_us + seq * 1000  # the sender's clock: one datagram per millisecond
        header = b"WGEN" + struct.pack("!HHII", 1, 1 if last else 0, seq, sent_us)
        filler = b"" if last else bytes((seq + i) & 0xFF for i in range(48))
        cap.udp(T0 + t, A, B, 7001, 7001, header + filler)

    # The ADC board: the computer starts a stream, the board answers, streams, logs, answers a command.
    t += 0.5
    rate_mhz, n, port = 20_000_000, 16, 50612
    cap.udp(T0 + t, PC, BOARD, port, 7100, b"start rate=20000 n=16")
    cap.udp(
        T0 + t + 0.0003,
        BOARD,
        PC,
        7100,
        port,
        f"ok rate_mhz={rate_mhz} n={n} pin=43 zero=2198 full=4393 verbose=0 pot=freq pot_pin=44 pot_lo=100 pot_hi=5000".encode(),
    )
    # The potentiometer's live position: mode freq, knob at 50 %, averaged code 3295, 707.25 Hz given to the generator.
    cap.udp(
        T0 + t + 0.0004,
        BOARD,
        PC,
        7100,
        port,
        b"POT1" + struct.pack("<BxHHxxf", 1, 5000, 3295, 707.25),
    )

    def log(magic, seq, ms, level, text):
        return magic + struct.pack("<IIB", seq, ms, level) + text.encode()

    cap.udp(
        T0 + t + 0.0005,
        BOARD,
        PC,
        7100,
        port,
        log(b"LOG1", 0, 1234, 1, "client 192.168.77.1:50612 · 20000.0 Hz"),
    )
    cap.udp(T0 + t + 0.0006, BOARD, PC, 7100, port, log(b"LOG1", 1, 1250, 2, "ring 90 % full"))
    for k, seq in enumerate([0, 1, 2, 4, 5]):
        samples = [
            round(2198 + 1500 * math.sin(2 * math.pi * (seq * n + i) / 40)) for i in range(n)
        ]
        dropped = 3 if seq == 5 else 0
        header = b"ADC1" + struct.pack("<IIIHH", seq, rate_mhz, dropped, n, 43)
        cap.udp(
            T0 + t + 0.01 + k * 0.0008,
            BOARD,
            PC,
            7100,
            port,
            header + struct.pack(f"<{n}H", *samples),
        )
    cap.udp(T0 + t + 0.05, PC, BOARD, port, 7100, b"info")
    cap.udp(
        T0 + t + 0.0502,
        BOARD,
        PC,
        7100,
        port,
        log(b"RPL1", 0, 1300, 5, "chip ESP32-S31 · build 2026-11-21"),
    )
    cap.udp(T0 + t + 0.06, PC, BOARD, port, 7100, b"stop")
    return cap


def make_tcp():
    cap = Capture()
    cap.tcp(T0, A, B, 50000, 7000, 1000, 0, 0x02)
    cap.tcp(T0 + 0.0002, B, A, 7000, 50000, 5000, 1001, 0x12)
    cap.tcp(T0 + 0.0004, A, B, 50000, 7000, 1001, 5001, 0x10)

    lines = []
    node_conversation(lambda t, src, dst, line: lines.append((t, src, line)), faults=False)

    # What each side writes, at the times it wrote it; a segment carries what was written meanwhile (a burst
    # makes several lines in one segment), and the very first one of A is cut in the middle of a line.
    segments = []
    for side in (A, B):
        mine = [(t, line) for t, src, line in lines if src is side]
        step = 3 if side is A else 4
        clock = T0 + 0.001
        for k in range(0, len(mine), step):
            data = b"".join(line for _, line in mine[k : k + step])
            clock = max(clock + 0.0008, T0 + 0.001 + mine[min(k + step, len(mine)) - 1][0] / 50)
            if side is A and k == 0:
                cut = len(data) - 12
                segments += [(clock, side, data[:cut]), (clock + 0.0004, side, data[cut:])]
                clock += 0.0004
            else:
                segments.append((clock, side, data))
    segments.sort(key=lambda s: s[0])

    sent = {id(A): 1001, id(B): 5001}
    for at, side, data in segments:
        src, dst = (A, B) if side is A else (B, A)
        sport, dport = (50000, 7000) if side is A else (7000, 50000)
        cap.tcp(at, src, dst, sport, dport, sent[id(side)], sent[id(dst)], 0x18, data)
        sent[id(side)] += len(data)
    return cap


def main(out=None):
    out = Path(out or (sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent / "samples"))
    out.mkdir(parents=True, exist_ok=True)
    make_udp().write(out / "udp.pcap")
    make_tcp().write(out / "tcp.pcap")
    print(f"wrote {out / 'udp.pcap'} and {out / 'tcp.pcap'}")


if __name__ == "__main__":
    main()
