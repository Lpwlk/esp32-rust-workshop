#!/usr/bin/env python3
"""Checks workshop.lua: against the Rust sources (tables, ports) and against tshark on the sample captures.

    python3 tools/wireshark/check.py              # needs tshark (Wireshark) on the PATH or in /Applications

Exit status 0 when everything holds. Nothing here needs a board.
"""

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
LUA = HERE / "workshop.lua"

failures = []


def check(ok, what):
    print(("  ok   " if ok else "  FAIL ") + what)
    if not ok:
        failures.append(what)


def lua_block(name):
    """The text of `local NAME = { ... }` (up to its closing brace at column 0)."""
    text = LUA.read_text()
    match = re.search(rf"^local {name} = \{{\n(.*?)^\}}", text, re.S | re.M)
    assert match, f"{name} not found in workshop.lua"
    return match.group(1)


def source_tables():
    space = (ROOT / "firmware/shared/core/src/space.rs").read_text()
    config = (ROOT / "firmware/shared/core/src/config.rs").read_text()
    consts = {"ROOT": 0, "NONE": 65535}
    consts["CONFIG"] = int(re.search(r"pub const CONFIG: NodeId = (\d+);", space).group(1))
    ident = lambda token: consts[token] if token in consts else int(token)
    nodes = [
        (ident(i), ident(p), name)
        for i, p, name in re.findall(
            r'^\s+(?:object|variable)\((\w+), (\w+), "([^"]*)"', space, re.M
        )
    ]
    keys_block = re.search(
        r"pub const KEYS: \[\(&str, Key\); \d+\] = \[(.*?)\];", config, re.S
    ).group(1)
    keys = re.findall(r'\("([^"]+)", Key::\w+\)', keys_block)
    status_block = re.search(r"pub enum Status \{(.*?)\n\}", space, re.S).group(1)
    status = {int(n): name for name, n in re.findall(r"(\w+) = (\d+),", status_block)}
    return nodes, keys, status


def check_tables():
    print("tables against the Rust sources")
    nodes, keys, status = source_tables()
    lua_nodes = [
        (int(i), int(p), n)
        for i, p, n in re.findall(
            r'id = (\d+), parent = (\d+), name = "([^"]*)"', lua_block("NODES")
        )
    ]
    check(lua_nodes == nodes, f"NODES: {len(lua_nodes)} in Lua, {len(nodes)} in space.rs")
    lua_keys = re.findall(r'"([^"]+)"', lua_block("CONFIG_KEYS"))
    check(lua_keys == keys, f"CONFIG_KEYS: {len(lua_keys)} in Lua, {len(keys)} in config.rs")
    lua_status = {
        int(n): name for n, name in re.findall(r'\[(\d+)\] = "(\w+)"', lua_block("STATUS"))
    }
    check(lua_status == status, f"STATUS: {len(lua_status)} in Lua, {len(status)} in space.rs")

    text = LUA.read_text()
    adc = (ROOT / "firmware/adc-stream/src/stream.rs").read_text()
    check(
        f"ADC_HEADER = {re.search(r'pub const HEADER: usize = (\d+);', adc).group(1)}" in text,
        "ADC header length",
    )
    check(
        f"udp_table:add({re.search(r'pub const PORT: u16 = (\d+);', adc).group(1)}, adc)" in text,
        "ADC port",
    )
    pot = (ROOT / "firmware/adc-stream/src/pot.rs").read_text()
    check(
        f"POT_DATAGRAM = {re.search(r'pub const DATAGRAM: usize = (\d+);', pot).group(1)}" in text
        and b"POT1" in pot.encode(),
        "POT1 datagram length",
    )
    base = (ROOT / "firmware/ethernet/presets/base.toml").read_text()
    port = int(re.search(r"^port\s*=\s*(\d+)", base, re.M).group(1))
    check(
        f"udp_table:add({port}, node)" in text and f"tcp_table:add({port}, node)" in text,
        f"node port {port}",
    )
    check(f"udp_table:add({port + 1}, gen)" in text, f"traffic generator port {port + 1}")
    pktgen = (ROOT / "firmware/shared/core/src/pktgen.rs").read_text()
    check('"WGEN"' in text and '*b"WGEN"' in pktgen, "WGEN magic")


def find_tshark():
    for candidate in (shutil.which("tshark"), "/Applications/Wireshark.app/Contents/MacOS/tshark"):
        if candidate and Path(candidate).exists():
            return candidate
    sys.exit("tshark not found: install Wireshark")


def run(tshark, pcap, *args):
    cmd = [tshark, "-2", "-X", f"lua_script:{LUA}", "-r", str(pcap), *args]
    done = subprocess.run(cmd, capture_output=True, text=True)
    if done.stderr.strip():
        print(done.stderr.strip())
    if done.returncode != 0 or "Lua" in done.stderr or "tshark:" in done.stderr:
        check(False, f"tshark ran cleanly ({' '.join(args[:2])})")
    return done.stdout


def fields(tshark, pcap, display_filter, *names):
    args = ["-Y", display_filter, "-T", "fields", "-E", "separator=|"]
    for name in names:
        args += ["-e", name]
    return [line.split("|") for line in run(tshark, pcap, *args).splitlines()]


def check_captures(tshark):
    sys.path.insert(0, str(HERE))
    import make_samples

    with tempfile.TemporaryDirectory() as tmp:
        make_samples.main(tmp)
        udp, tcp = Path(tmp) / "udp.pcap", Path(tmp) / "tcp.pcap"

        print("UDP capture")
        count = lambda flt, pcap=udp: len(fields(tshark, pcap, flt, "frame.number"))
        check(count("wsnode") == 31, "31 node frames (heuristic and port)")
        check(count("wsgen") == 5, "5 generator datagrams")
        check(count("wsadc") == 13, "13 ADC datagrams (the POT1 one included)")
        check(count("_ws.malformed || _ws.expert.severity == error") == 0, "nothing malformed")

        lost = fields(tshark, udp, "wsnode.lost", "frame.number", "wsnode.lost")
        # Frame 9 follows the corrupted frame 8, which a node refuses: its sequence number is not trusted, so the gap shows.
        check(
            lost == [["5", "1"], ["9", "1"]],
            f"lost frames: 5 (really lost), 9 (after the corrupted one) ({lost})",
        )
        bad = fields(
            tshark, udp, "wsnode.checksum.good == 0", "frame.number", "wsnode.checksum.calculated"
        )
        check(len(bad) == 1 and bad[0][0] == "8", f"one bad checksum, at frame 8 ({bad})")

        echo = fields(tshark, udp, "wsnode.rtt", "frame.number", "wsnode.echo_of", "wsnode.rtt")
        check(
            len(echo) == 1 and echo[0][:2] == ["7", "6"] and abs(float(echo[0][2]) - 0.31) < 0.01,
            f"echo links frame 7 to 6, rtt 0.31 ms ({echo})",
        )
        links = fields(tshark, udp, "wsnode.request_in", "frame.number", "wsnode.request_in")
        check(
            ["10", "9"] in links and ["16", "15"] in links and ["25", "23"] in links,
            "answers point at their request",
        )
        answers = fields(tshark, udp, "wsnode.response_in", "frame.number", "wsnode.response_in")
        check(["9", "10"] in answers and ["23", "24"] in answers, "requests point at their answer")

        paths = fields(tshark, udp, "wsnode.path", "frame.number", "wsnode.path")
        check(
            ["17", "/leds/1"] in paths and ["21", "/config/led1"] in paths,
            f"paths of ids ({paths[:3]}...)",
        )
        refused = fields(tshark, udp, "wsnode.status > 0", "frame.number", "wsnode.status")
        check(refused == [["20", "3"]], f"the refused write is frame 20, status 3 ({refused})")
        kinds = fields(tshark, udp, "wsnode.kind.text", "wsnode.kind.text")
        check(
            ["Variable, text, read-write"] in kinds and ["no node (end of the branch)"] in kinds,
            "kinds spelled out",
        )
        burst = fields(tshark, udp, "wsnode.burst", "wsnode.burst")
        check(len(burst) == 3, "3 burst frames, not counted in the sequence")

        gen = fields(tshark, udp, "wsgen", "wsgen.lost", "wsgen.jitter_us", "wsgen.stream")
        check(sum(1 for g in gen if g[0] == "1") == 1, "one datagram lost in the stream")
        check(all(g[2] == "1" for g in gen), "stream number")
        jitter = [float(g[1]) for g in gen if g[1]]
        check(len(jitter) == 4 and max(jitter) > 0, f"jitter computed ({jitter})")

        adc = fields(
            tshark,
            udp,
            "wsadc.rate_hz",
            "wsadc.seq",
            "wsadc.lost",
            "wsadc.rate_hz",
            "wsadc.count",
            "wsadc.mean",
        )
        check(
            len(adc) == 5 and adc[3][:2] == ["4", "1"] and adc[0][3] == "16",
            f"ADC1: a gap before #4 ({adc[3]})",
        )
        check(
            fields(tshark, udp, "wsadc.ok.rate_mhz", "wsadc.ok.rate_mhz", "wsadc.ok.pin")[0]
            == ["20000000", "43"],
            "the answer to start is read",
        )
        check(
            fields(
                tshark,
                udp,
                "wsadc.pot.mode",
                "wsadc.pot.mode",
                "wsadc.pot.position",
                "wsadc.pot.code",
                "wsadc.pot.value",
            )
            == [["1", "5000", "3295", "707.25"]],
            "the POT1 datagram is read",
        )
        check(
            fields(tshark, udp, "wsadc.ok.pot_pin", "wsadc.ok.pot_pin")[0] == ["44"],
            "the potentiometer's pin in the answer to start",
        )
        check(
            fields(tshark, udp, "wsadc.start.rate", "wsadc.start.rate", "wsadc.start.n")[0]
            == ["20000", "16"],
            "the start command is read",
        )
        log = fields(tshark, udp, "wsadc.level", "wsadc.level", "wsadc.text")
        check(
            ["2", "ring 90 % full"] in log and any("·" in l[1] for l in log),
            f"log levels and UTF-8 text ({log})",
        )

        print("TCP capture")
        check(count("wsnode", tcp) == 11, "11 segments with frames")
        lines = run(tshark, tcp, "-T", "fields", "-e", "wsnode.tag", "-Y", "wsnode")
        tags = [t for line in lines.splitlines() for t in line.split(",")]
        check(
            len(tags) == 31, f"{len(tags)} frames decoded, as many as over UDP (31 incl. none lost)"
        )
        check(fields(tshark, tcp, "wsnode.lost", "frame.number") == [], "no loss over TCP")
        check(
            fields(tshark, tcp, "wsnode.checksum.good == 0", "frame.number") == [],
            "no bad checksum over TCP",
        )
        warnings = fields(tshark, tcp, '_ws.expert.severity >= "Warning"', "_ws.expert.message")
        check(
            warnings == [["BadTypeMismatch"]],
            f"the only warning is the refused write; the cut line is reassembled ({warnings})",
        )


def main():
    check_tables()
    check_captures(find_tshark())
    print()
    if failures:
        print(f"{len(failures)} check(s) failed")
        return 1
    print("all good")
    return 0


if __name__ == "__main__":
    sys.exit(main())
