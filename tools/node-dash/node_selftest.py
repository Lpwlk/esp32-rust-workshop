"""`node_dash.py --selftest`: the whole chain on two simulated nodes, no board and no browser.

The parser is tried on the exact bytes the firmware writes (the prompt block, the live line, the dashboard),
then the web side is driven over HTTP the way the page does it: event stream, commands, settings, tests.
"""

from __future__ import annotations

import http.client
import json
import threading
import time

import node_dash as nd
import node_sim


class Page:
    """What an open page would hold: the event stream, merged."""

    def __init__(self, port: int) -> None:
        self.conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        self.conn.request("GET", "/events")
        self.resp = self.conn.getresponse()
        self.frame: dict = {}
        self.series: dict = {k: {n: [] for n in nd.Board("x", None, None).series} for k in nd.NODES}
        self.logs: dict = {k: [] for k in nd.NODES}
        self.lock = threading.RLock()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self) -> None:
        event = None
        try:
            while True:
                line = self.resp.readline()
                if not line:
                    return
                if line.startswith(b"event: "):
                    event = line[7:].strip()
                elif line.startswith(b"data: "):
                    data = json.loads(line[6:])
                    with self.lock:
                        if event == b"log":
                            self.logs[data["board"]].extend(data["rows"])
                        else:
                            self.frame = data
                            for k, b in data["boards"].items():
                                for n, s in b["series"].items():
                                    self.series[k][n].extend(s["items"])
                    event = None
        except (OSError, ValueError):
            return

    def wait(self, what, timeout: float = 8.0):
        """The first non-false value of `what(page)`, polled."""
        end = time.time() + timeout
        while time.time() < end:
            with self.lock:
                value = what(self)
            if value:
                return value
            time.sleep(0.05)
        return None

    def board(self, k: str) -> dict:
        return self.frame.get("boards", {}).get(k, {})

    def text(self, k: str) -> list[str]:
        with self.lock:
            return [nd.plain(e["runs"]) for e in self.logs[k]]


def run() -> int:
    failures: list[str] = []

    def check(ok: bool, what: str) -> None:
        print(("ok    " if ok else "FAIL  ") + what)
        if not ok:
            failures.append(what)

    # 1. the terminal, on what the firmware writes
    s = nd.Screen()
    stream = (
        "\x1b[?2026h\x1b[?25l\r\x1b[2K"  # prompt_clear
        "\x1b[90m    12.513\x1b[0m \x1b[36mTX ▶\x1b[0m \x1b[30;47m DAT \x1b[0m \x1b[90m#00004\x1b[0m          \x1b[90m│\x1b[0m btn ○ released\r\n"
        "\x1b[?7l\x1b[90m    12.513\x1b[0m \x1b[36mTX ▶\x1b[0m  DAT  #00004  │ tx 4 rx 3\x1b[0m lost 1\x1b[?7h\n"  # the live line
        "\x1b[?7l  dash row\x1b[?7h\n"
        "\x1b[1m\x1b[35mcli ›\x1b[0m hel\x1b[3D\x1b[?25h\x1b[?2026l"
    )
    data = stream.encode()
    for i in range(0, len(data), 7):  # chunks that cut the multi-byte characters
        s.feed(data[i : i + 7])
    rows = s.take()
    check(
        len(rows) == 1 and nd.plain(rows[0]).startswith("    12.513 TX ▶  DAT"),
        "a printed line is one log row",
    )
    check(rows[0][0][1] == 8, "its SGR colour is kept (90 = bright black = 8)")
    check(
        s.live is not None and "tx 4 rx 3" in nd.plain(s.live),
        "the row drawn with auto-wrap off is the live line, not a log row",
    )
    check(nd.plain(s.dash[0]) == "  dash row", "the next such rows are the dashboard")
    check(s.prompt == "cli › hel", f"the prompt row is the prompt ({s.prompt!r})")
    s.feed(
        b"\x1b[?2026h\x1b[?25l\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2Kone\r\ntwo\rTWO\r\n\x1b[?25h\x1b[?2026l"
    )
    check(
        [nd.plain(r) for r in s.take()] == ["one", "TWO"],
        "erasing the block does not eat log rows; \\r overwrites",
    )
    check(s.live is None and s.dash == [], "a redraw without block rows empties the live line")

    # the UART firmware draws the same block without `ESC[?2026h/l`: it ends where the next erase begins
    u = nd.Screen()
    live1 = "\x1b[?7l\x1b[90m    5.000\x1b[0m \x1b[32mRX ◀\x1b[0m DAT  #00001 node-B │ x │ tx 1 rx 1\x1b[0m lost 0\x1b[?7h\n"
    live2 = (
        live1.replace("5.000", "7.500")
        .replace("#00001", "#00002")
        .replace("tx 1 rx 1", "tx 2 rx 2")
    )
    prompt = "\x1b[1m\x1b[35mcli ›\x1b[0m "
    cycle1 = "\r\x1b[2K" + "boot line\r\n" + live1 + prompt
    cycle2 = "\r\x1b[2K\x1b[1A\r\x1b[2K" + "second line\r\n" + live2 + prompt
    data = (cycle1 + cycle2).encode()
    u.feed(cycle1.encode()[:23])
    u.feed(cycle1.encode()[23:])
    check(
        [nd.plain(r) for r in u.take()] == ["boot line"] and u.live is not None,
        "UART stream: the live line is seen right away (no sync marker)",
    )
    check("tx 1 rx 1" in nd.plain(u.live) and u.prompt == "cli ›", f"and the prompt ({u.prompt!r})")
    u.feed(cycle2.encode())
    check(
        [nd.plain(r) for r in u.take()] == ["second line"] and "tx 2 rx 2" in nd.plain(u.live),
        "the next redraw replaces it, its rows are not logged twice",
    )
    u.feed(("\r\x1b[2K\x1b[1A\r\x1b[2K" + "no live now\r\n" + prompt + "he").encode())
    check(u.live is None and u.prompt == "cli › he", "a redraw without a live line empties it")
    del data

    # 2. the numbers read from the lines
    m = nd.FRAME_RE.match(
        "   14.656 RX ◀  DAT  #00004 node-B   │ btn ● pressed   event     4 │ Δ  2142.821 ms ← TX"
    )
    check(bool(m) and m[3] == "RX" and m[5] == "00004", "frame line")
    m = nd.STATUS_RE.match(
        "─  485s ─ tx   242 · rx   242 · lost  0 · bad  0 ─ ● UP   1G   ─ rtt  4.08 /  8.55 / 1005.62 ms"
    )
    check(bool(m) and m[6] == "UP" and m[7] == "1G" and m[10] == "1005.62", "stats line")
    check(
        nd.parse_bps("1.25 MB/s") == 1.25e6
        and nd.parse_dur_ms("1 min 5 s") == 65000
        and nd.parse_bitrate("5.00 Mbit/s") == 5e6,
        "units",
    )

    # 3. the whole chain, Ethernet app
    world = node_sim.World("eth", loss=0.01)
    boards = {k: nd.Board(k, world.factory(k), f"simulated {k}") for k in nd.NODES}
    server = nd.Server(boards, "127.0.0.1", 0, fps=20)
    server.start()

    def http_(method: str, path: str, body=None, ctype="application/json"):
        c = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
        c.request(
            method,
            path,
            json.dumps(body) if body is not None else None,
            {"Content-Type": ctype} if body is not None else {},
        )
        r = c.getresponse()
        return r.status, r.read()

    def send(board: str, text: str, quiet: bool = False) -> int:
        return http_("POST", "/api/cmd", {"board": board, "text": text, "quiet": quiet})[0]

    status, body = http_("GET", "/")
    check(status == 200 and b"<title>Node dashboard</title>" in body, "GET / serves the page")
    check(http_("GET", "/nope")[0] == 404, "unknown path is a 404")
    page = Page(server.port)
    got = page.wait(lambda p: all(p.board(k).get("settings") for k in nd.NODES))
    check(bool(got), "on connect the page asks `config`: settings of both nodes")
    a, b = page.board("a"), page.board("b")
    check(
        a.get("app") == "eth" and a.get("name") == "node-A" and b.get("name") == "node-B",
        f"app and names read from `config` ({a.get('app')}, {a.get('name')})",
    )
    check(
        a["settings"]["verbose"][0] == "live" and a["settings"]["net.transport"][2] is True,
        "settings: value, build value, read-only",
    )
    check(
        not any("rgb.brightness" in t for t in page.text("a")),
        "the page's own `config` is kept out of the log",
    )
    check(not any(t.strip() == "› config" for t in page.text("a")), "and so is its echo")

    check(
        page.wait(lambda p: p.board("a").get("counters", {}).get("tx")) is not None,
        "live line: counters read",
    )
    check(
        page.wait(lambda p: p.board("a").get("live")) is not None,
        "the live line is exposed to the page",
    )
    check(
        not any(nd.FRAME_RE.match(t) for t in page.text("a")),
        "at verbose live, frames stay on the live line, not in the log",
    )

    check(send("ab", "verbose frames") == 200, "POST /api/cmd verbose frames to both")
    check(
        bool(
            page.wait(
                lambda p: p.board("a")["settings"]["verbose"][0] == "frames"
                and p.board("b")["settings"]["verbose"][0] == "frames"
            )
        ),
        "the settings follow the board (refreshed by itself)",
    )
    check(
        bool(
            page.wait(
                lambda p: any(
                    nd.FRAME_RE.match(t) for t in [nd.plain(e["runs"]) for e in p.logs["a"]]
                )
            )
        ),
        "verbose frames: frame lines scroll in the log",
    )
    check(
        bool(page.wait(lambda p: len(p.series["a"]["delta"]) >= 3)), "intervals between DAT frames"
    )
    check(
        bool(page.wait(lambda p: len(p.series["a"]["rate"]) >= 2)),
        "frames per second from the counters",
    )
    rate = page.series["a"]["rate"][-1]
    check(
        1.0 < rate[1] < 6.0 and 1.0 < rate[2] < 6.0,
        f"2 DAT frames/s each way at 500 ms ({rate[1]:.1f} tx, {rate[2]:.1f} rx)",
    )

    send("ab", "tx on-rx")
    send("a", "tx periodic 200ms")
    check(
        bool(page.wait(lambda p: len(p.series["a"]["rtt"]) >= 3)),
        "round-trip times, once the peer echoes (tx on-rx)",
    )
    rtt = page.series["a"]["rtt"][-1][1]
    check(100 < rtt < 20000, f"round trip {rtt:.0f} µs")
    check(
        page.wait(lambda p: p.board("a")["settings"]["data_period_ms"][0] == "200"),
        "tx periodic 200ms sets the period",
    )

    send("a", "pattern ocean")
    send("a", "bright 70")
    send("a", "led 2 blink 300")
    check(
        bool(
            page.wait(
                lambda p: p.board("a")["settings"]["rgb.pattern"]
                == ["ocean", "viridis", False, True]
            )
        ),
        "pattern ocean: new value, build value, marked changed",
    )
    check(
        page.board("a")["settings"]["rgb.brightness"][0] == "70"
        and page.board("a")["settings"]["led2"][0] == "blink 300",
        "brightness and LED 2",
    )
    check(page.board("b")["settings"]["rgb.pattern"][0] == "viridis", "the other node is untouched")
    send("a", "bright 500")
    check(
        bool(page.wait(lambda p: any("brightness" in t and "✗" in t for t in p.text("a")))),
        "a refused value comes back as an error line",
    )
    check(page.board("a")["settings"]["rgb.brightness"][0] == "70", "and nothing changed")
    send("a", "reset")
    check(
        bool(page.wait(lambda p: p.board("a")["settings"]["rgb.pattern"][3] is False)),
        "reset: back to the build values",
    )

    send("a", "burst 300 max")
    check(
        bool(
            page.wait(
                lambda p: len(p.series["a"]["tests"]) >= 1 and len(p.series["b"]["tests"]) >= 1
            )
        ),
        "a burst: result on the sender and on the receiver",
    )
    ta, tb = page.series["a"]["tests"][-1], page.series["b"]["tests"][-1]
    check(
        ta["dir"] == "tx" and ta["frames"] == 300 and ta["bps"] > 0,
        f"sender: {ta['frames']} frames, {ta['bps']:.0f} B/s",
    )
    check(
        tb["dir"] == "rx" and tb["frames"] + tb["lost"] == 300 and tb["who"] == "node-A",
        f"receiver: {tb['frames']} of 300, {tb['lost']} lost",
    )

    send("a", "burst")
    check(
        bool(page.wait(lambda p: p.board("a")["prompt"].startswith("burst length"))),
        "the burst wizard's question is the prompt",
    )
    send("a", "")
    check(
        bool(page.wait(lambda p: p.board("a")["prompt"].startswith("burst rate"))),
        "an empty line takes the default and goes to the next question",
    )
    send("a", "q")
    check(
        bool(page.wait(lambda p: p.board("a")["prompt"].startswith("cli"))),
        "q cancels, the prompt is back",
    )

    send("a", "burst 60 30")
    check(
        bool(page.wait(lambda p: (p.board("a").get("burst_run") or {}).get("count") == 60)),
        "a burst under way is exposed: count and rate",
    )
    check(page.board("a")["burst_run"]["rate_hz"] == 30.0, "with its rate, for a progress bar")
    check(
        bool(page.wait(lambda p: p.board("a").get("burst_run") is None, 10)),
        "and gone when the burst is sent",
    )
    check(
        time.time() - page.board("a")["peer_last"] < 3,
        "the time of the last frame from the peer, for the link timeout bar",
    )

    send("a", "gen start 1Mbit 256B 2s")
    check(
        bool(
            page.wait(
                lambda p: p.board("a").get("gen")
                and p.board("a")["gen"]["state"] in ("running", "idle", "done")
                or any("gen" in t for t in p.text("a"))
            )
        ),
        "gen start",
    )
    check(
        bool(page.wait(lambda p: (p.board("a").get("gen") or {}).get("total_ms") == 2000)),
        "the generator's length is read from its status",
    )
    check(
        bool(
            page.wait(
                lambda p: ((p.board("a").get("gen") or {}).get("stream") or {}).get("sent", 0) > 0
            )
        ),
        "and its stream's progress is polled",
    )
    check(
        sum("stream #" in t for t in page.text("a")) <= 1
        and not any(t.strip() == "› gen status" for t in page.text("a")),
        "the polling stays out of the log (only the answer to the typed `gen start` is there)",
    )

    check(
        bool(
            page.wait(
                lambda p: any(t["kind"] == "gen" for t in p.series["a"]["tests"])
                and any(t["kind"] == "gen" for t in p.series["b"]["tests"]),
                12,
            )
        ),
        "a generator run: both sides' summaries",
    )

    send("a", "browse / 2")
    check(
        bool(page.wait(lambda p: len(p.board("a")["space"]["tree"]) > 10)),
        "browse: the peer's tree",
    )
    tree = {n["path"]: n for n in page.board("a")["space"]["tree"]}
    check(
        tree.get("/leds/1", {}).get("rw") is True
        and tree.get("/stats/rx", {}).get("rw") is False
        and tree.get("/leds", {}).get("obj") is True,
        "paths, types and access",
    )
    send("a", "write /leds/1 blink 200")
    send("a", "read /stats/lost")
    send("a", "write /stats/rx 3")
    check(
        bool(
            page.wait(
                lambda p: p.board("a")["space"]["values"].get("/leds/1", {}).get("value")
                == "blink 200"
            )
        ),
        "write: the value now in force",
    )
    check(
        bool(
            page.wait(
                lambda p: p.board("a")["space"]["values"].get("/stats/lost", {}).get("status")
                == "Good"
            )
        ),
        "read",
    )
    check(
        bool(
            page.wait(
                lambda p: p.board("a")["space"]["values"].get("/stats/rx", {}).get("status")
                == "BadNotWritable"
            )
        ),
        "a refused write: the status",
    )

    # the peer's config, read and written over the link by the board itself
    def peer(board: str, action: str, keys=None) -> tuple[int, dict]:
        status, body = http_("POST", "/api/peer", {"board": board, "action": action, "keys": keys})
        return status, json.loads(body)

    def idle(p) -> bool:
        return p.board("a")["peer"]["state"] == "idle"

    send("b", "bright 90")
    send("b", "pattern sunset")
    check(
        bool(page.wait(lambda p: p.board("b")["settings"]["rgb.pattern"][0] == "sunset")),
        "node B given another pattern and brightness",
    )
    check(peer("a", "pull", ["rgb.pattern"])[0] == 400, "pull before reading the peer is refused")
    check(
        peer("a", "dance")[0] == 400
        and peer("a", "push", [])[0] == 400
        and peer("c", "read")[0] == 400,
        "unknown action, no keys, unknown board",
    )
    n = page.board("a")["peer"]["n"]
    check(peer("a", "read")[0] == 200, "POST /api/peer read")
    check(
        bool(page.wait(lambda p: idle(p) and p.board("a")["peer"]["n"] > n)),
        "the peer's settings are read, one `read /config/<key>` each",
    )
    rows = {r["key"]: r for r in page.board("a")["peer"]["rows"]}
    check(
        len(rows) == 23 and all(r["status"] == "ok" for r in rows.values()),
        f"23 settings answered ({len(rows)})",
    )
    check(
        rows["rgb.brightness"]["peer"] == "90"
        and rows["rgb.pattern"]["peer"] == "sunset"
        and rows["verbose"]["peer"] == "frames",
        "with the peer's own values",
    )
    check(
        not any("/config/" in t for t in page.text("a")),
        "the sequence stays out of the console log",
    )
    check(
        not page.board("a")["space"]["values"]
        or not any(k.startswith("/config/") for k in page.board("a")["space"]["values"]),
        "and out of the address space table",
    )

    check(
        peer("a", "pull", ["rgb.pattern", "rgb.brightness"])[0] == 200,
        "pull two settings to node A",
    )
    check(
        bool(
            page.wait(lambda p: idle(p) and p.board("a")["settings"]["rgb.pattern"][0] == "sunset")
        ),
        "node A now has the peer's pattern",
    )
    check(page.board("a")["settings"]["rgb.brightness"][0] == "90", "and brightness")
    check(
        {r["key"]: r["status"] for r in page.board("a")["peer"]["rows"]}["rgb.pattern"] == "pulled",
        "the row says pulled",
    )

    send("a", "bright 55")
    send("a", "led 2 blink 300")
    check(
        bool(page.wait(lambda p: p.board("a")["settings"]["rgb.brightness"][0] == "55")),
        "node A changes two settings",
    )
    check(peer("a", "push", ["rgb.brightness", "led2"])[0] == 200, "push them to the peer")
    check(
        bool(
            page.wait(lambda p: idle(p) and p.board("b")["settings"]["rgb.brightness"][0] == "55")
        ),
        "node B's own console now says 55 (asked again by itself)",
    )
    check(
        page.board("b")["settings"]["led2"][0] == "blink 300", "and the LED mode, with its period"
    )
    check(
        page.board("b")["settings"]["rgb.pattern"][0] == "sunset",
        "settings that were not pushed are not touched",
    )
    check(
        {r["key"]: r["status"] for r in page.board("a")["peer"]["rows"]}["led2"] == "pushed",
        "the row says pushed",
    )
    send("a", "reset")
    send("b", "reset")

    # the dashboard does not need the trace
    check(
        page.board("a")["settings"]["trace"][0] == "off"
        and page.board("a")["settings"]["trace_dash"][0] == "off",
        "trace off, dashboard off at boot",
    )
    check(not page.board("a")["dash"], "no dashboard rows then")
    send("a", "dash chart")
    check(
        bool(page.wait(lambda p: any("dashboard is off" in t for t in p.text("a")))),
        "choosing a view while it is off says why nothing shows",
    )
    send("a", "dash normal")
    check(
        bool(page.wait(lambda p: p.board("a")["dash"])),
        "dash normal: the traffic dashboard rows are exposed, with the trace off",
    )
    check(page.board("a")["settings"]["trace"][0] == "off", "the trace stayed off")
    send("a", "trace frames")
    send("a", "dash off")
    check(
        bool(
            page.wait(
                lambda p: p.board("a")["settings"]["trace_dash"][0] == "off"
                and not p.board("a")["dash"]
            )
        ),
        "dash off removes it, with the trace on",
    )
    send("a", "trace off")

    send("a", "reboot")
    check(bool(page.wait(lambda p: p.board("a")["boots"] >= 1)), "reboot: the boot card is noticed")
    check(
        bool(page.wait(lambda p: p.board("a")["card"].get("MAC", "").startswith("1C:29"))),
        "and read (MAC)",
    )
    check(
        page.wait(lambda p: p.board("a")["settings"]["data_period_ms"][0] == "500"),
        "after a reboot the settings are asked again: build values",
    )

    # refusals
    check(send("a", "x" * 200) == 400, "a line longer than the board's editor takes is refused")
    check(send("a", "led\x03 1 on") == 400, "control characters are refused")
    check(send("c", "info") == 400, "unknown board")
    check(send("a", "reboot", quiet=True) == 400, "`quiet` is for `config` only")
    check(
        http_("POST", "/api/cmd", {"board": "a", "text": "info"}, "text/plain")[0] == 400,
        "a POST that isn't JSON is refused",
    )
    check(
        http_("GET", "/api/export?board=a&kind=frames")[1].startswith(
            b"host_time,board_ms,dir,kind"
        ),
        "frames CSV",
    )
    check(b" board " in http_("GET", "/api/export?board=a&kind=log")[1], "log export")

    page.resp.close()
    server.close()
    world.close()

    # 4. the UART app
    world = node_sim.World("uart")
    boards = {k: nd.Board(k, world.factory(k), f"simulated {k}") for k in nd.NODES}
    server = nd.Server(boards, "127.0.0.1", 0, fps=20)
    server.start()
    page = Page(server.port)
    check(
        bool(page.wait(lambda p: p.board("a").get("app") == "uart")),
        "UART app recognised (shared.baud_rate in `config`)",
    )
    check(
        bool(page.wait(lambda p: p.board("a")["settings"].get("verbose", [""])[0] == "live")),
        "UART: settings read, verbose live",
    )
    check(
        bool(page.wait(lambda p: p.board("a")["live"] and p.board("a")["counters"]["rx"])),
        "UART at verbose live: the live line and its counters",
    )
    check(
        bool(page.wait(lambda p: p.board("a")["prompt"] == "cli ›")),
        "UART: the prompt is read without synchronized output",
    )
    check(
        not any(nd.FRAME_RE.match(t) for t in page.text("a")),
        "UART: frames stay on the live line, not in the log",
    )
    send("a", "burst")
    check(
        bool(page.wait(lambda p: p.board("a")["prompt"].startswith("burst length"))),
        "UART: the burst wizard's question is the prompt",
    )
    send("a", "q")
    check(bool(page.wait(lambda p: p.board("a")["prompt"] == "cli ›")), "UART: back to the prompt")
    send("ab", "verbose frames")
    check(
        bool(
            page.wait(lambda p: page.board("a")["last_frame"] and page.board("a")["counters"]["rx"])
        ),
        "UART frames read",
    )
    check(
        bool(page.wait(lambda p: not p.board("a")["live"])), "verbose frames: no live line any more"
    )
    check(
        not any(k.startswith("trace") for k in []) and page.board("a")["app"] == "uart", "UART app"
    )
    send("a", "baud 921600")
    check(
        bool(page.wait(lambda p: any("921600" in t or "921.6" in t for t in p.text("b")))),
        "baud: both nodes follow",
    )
    page.resp.close()
    server.close()
    world.close()

    # 5. no board
    lone = nd.Board("a", None, None, "no board assigned")
    server = nd.Server(
        {"a": lone, "b": nd.Board("b", None, None, "no board assigned")}, "127.0.0.1", 0, fps=10
    )
    server.start()
    page = Page(server.port)
    check(
        bool(page.wait(lambda p: p.board("a").get("status") == "unassigned")),
        "without a board the page says so",
    )
    check(send("a", "info") == 400, "and a command is refused, not lost")
    page.resp.close()
    server.close()

    print("selftest:", "FAILED" if failures else "passed")
    return 1 if failures else 0
