"""Two simulated nodes of the Ethernet / UART apps, for `node_dash.py --demo` and its self test.

They write what the firmware writes to the USB console, byte for byte where it matters to a reader of the
stream: the same escape sequences around the prompt block (`term.rs`: `prompt_clear` / `prompt_draw`, the
live line and the dashboard drawn with auto-wrap off, synchronized output), the same line layouts, the same
`config` table, the same `burst` / `gen` / address space answers. Their CLI is a subset of the real one (the
commands the page uses), and the two nodes talk to each other like the boards do: data frames, round-trip
times, button events, bursts, lost frames.
"""

from __future__ import annotations

import heapq
import queue
import random
import re
import threading
import time

RST, BOLD, FAINT = "\x1b[0m", "\x1b[1m", "\x1b[2m"
TIME = DIM = SEP = "\x1b[90m"
TX, RX, NODE, OK, WARN, ERROR = (
    "\x1b[36m",
    "\x1b[32m",
    "\x1b[33m",
    "\x1b[32m",
    "\x1b[33m",
    "\x1b[31m",
)
ACCENT, PROMPT, LINK = "\x1b[36m", "\x1b[35m", "\x1b[35m"
BADGE = {
    "DAT": "\x1b[30;47m",
    "EVT": "\x1b[30;43m",
    "ECH": "\x1b[30;45m",
    "BAU": "\x1b[30;45m",
    "BST": "",
}

# (key, value in the demo build): the order of `config`. Periods are short so a demo has something to show.
KEYS = [
    ("data_period_ms", "500"), ("status_period_ms", "5000"), ("link_timeout", "3"),
    ("debounce_ms", "30"), ("lockout_ms", "1000"), ("rgb.pattern", "viridis"), ("rgb.freq_hz", "50"),
    ("rgb.cycle_ms", "6000"), ("rgb.brightness", "40"), ("led1", "peer"), ("led2", "rx"),
    ("verbose", "live"), ("tx_mode", "periodic"), ("burst.count", "1000"), ("burst.rate_hz", "0"),
    ("console", "auto"), ("trace", "off"), ("trace_bytes", "32"), ("trace_view", "layers"),
    ("trace_dash", "off"), ("trace_dash_view", "bars"), ("trace_window_s", "30"), ("trace_short_ip", "false"),
]  # fmt: skip
RANGES = {
    "data_period_ms": (10, 3.6e6), "status_period_ms": (100, 3.6e6), "link_timeout": (2, 100),
    "debounce_ms": (1, 1000), "lockout_ms": (0, 5000), "rgb.freq_hz": (1, 200), "rgb.cycle_ms": (200, 60000),
    "rgb.brightness": (0, 100), "burst.count": (1, 100000), "trace_bytes": (0, 256), "trace_window_s": (2, 3600),
}  # fmt: skip
ENUMS = {
    "rgb.pattern": ("viridis", "sunset", "ocean", "aurora", "sakura"),
    "verbose": ("silent", "live", "frames", "wire"), "tx_mode": ("periodic", "on-rx", "quiet"),
    "trace": ("off", "events", "frames"), "trace_view": ("compact", "layers", "inside-out"),
    "trace_dash": ("off", "mini", "normal", "full"), "trace_dash_view": ("bars", "chart", "mix"),
    "console": ("auto", "serial", "rtt", "both"), "trace_short_ip": ("true", "false"),
}  # fmt: skip
SHORT = {  # the CLI shortcuts that are `set`
    "pattern": "rgb.pattern", "p": "rgb.pattern", "freq": "rgb.freq_hz", "f": "rgb.freq_hz",
    "cycle": "rgb.cycle_ms", "c": "rgb.cycle_ms", "bright": "rgb.brightness", "b": "rgb.brightness",
    "verbose": "verbose", "v": "verbose", "period": "data_period_ms", "pd": "data_period_ms",
    "trace": "trace", "t": "trace", "tr": "trace", "window": "trace_window_s", "win": "trace_window_s",
    "dump": "trace_bytes", "d": "trace_bytes", "tx": "tx_mode",
}  # fmt: skip
SPACE = [  # the address space of a node: (path, kind, writable)
    ("/node", "O", False), ("/node/name", "T", False), ("/node/uptime", "U", False), ("/leds", "O", False),
    ("/leds/1", "T", True), ("/leds/2", "T", True), ("/buttons", "O", False), ("/buttons/jumper", "B", False),
    ("/buttons/boot", "B", False), ("/rgb", "O", False), ("/rgb/pattern", "T", True), ("/rgb/brightness", "U", True),
    ("/stats", "O", False), ("/stats/tx", "U", False), ("/stats/rx", "U", False), ("/stats/lost", "U", False),
    ("/config", "O", False), *[(f"/config/{k}", "T", True) for k, _ in KEYS],
]  # fmt: skip


def parse_period_ms(text: str) -> float | None:
    m = re.fullmatch(r"\s*([\d.]+)\s*(hz|ms|s|min)?\s*", text.lower())
    if not m:
        return None
    n = float(m[1])
    return {"hz": 1000 / n if n else None, "ms": n, "s": n * 1000, "min": n * 60000, None: n}[m[2]]


def num(x: float) -> str:
    return str(int(x)) if float(x).is_integer() else f"{x:g}"


class SimTransport:
    """What `Board` reads and writes: bytes in both directions."""

    def __init__(self, node: "SimNode") -> None:
        self.node = node

    def read(self) -> bytes:
        try:
            return self.node.out.get(timeout=0.1)
        except queue.Empty:
            return b""

    def write(self, data: bytes) -> None:
        self.node.inbox.put(data)

    def close(self) -> None:
        pass


class World:
    """Both nodes, and the wire between them."""

    def __init__(self, app: str = "eth", seed: int = 7, loss: float = 0.0) -> None:
        self.app = app
        self.rng = random.Random(seed)
        self.loss = loss  # chance a frame is lost on the way
        self.latency = 0.0004 if app == "eth" else 0.00006
        self.nodes = {"a": SimNode(self, "a"), "b": SimNode(self, "b")}
        self.nodes["a"].peer, self.nodes["b"].peer = self.nodes["b"], self.nodes["a"]
        self.wire: list = []  # heap of (arrival, n, node, frame)
        self._n = 0
        self.stop_flag = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def factory(self, key: str):
        return lambda: SimTransport(self.nodes[key])

    def send(self, to: "SimNode", frame: dict) -> None:
        if self.rng.random() < self.loss:
            return
        self._n += 1
        heapq.heappush(
            self.wire,
            (time.monotonic() + self.latency * (1 + self.rng.random()), self._n, to, frame),
        )

    def close(self) -> None:
        self.stop_flag.set()
        self._thread.join(timeout=1)

    def _run(self) -> None:
        while not self.stop_flag.is_set():
            now = time.monotonic()
            while self.wire and self.wire[0][0] <= now:
                _, _, node, frame = heapq.heappop(self.wire)
                node.receive(frame)
            for node in self.nodes.values():
                node.step(now)
            time.sleep(0.002)


class SimNode:
    def __init__(self, world: World, key: str) -> None:
        self.world, self.key = world, key
        self.app = world.app
        self.name = f"node-{key.upper()}"
        self.peer: SimNode
        self.out: queue.Queue = queue.Queue()
        self.inbox: queue.Queue = queue.Queue()
        self.settings = dict(KEYS)
        self.line = ""
        self.label: str | None = None  # a question of the burst wizard
        self.wizard: list = []
        self.history: list[str] = []
        self.block = 0
        self.sync_open = False
        self.live: str | None = None
        self.dash: list[str] = []
        self.baud = 5_000_000
        self.boot()

    # --- state that a reboot resets
    def boot(self) -> None:
        self.t0 = time.monotonic()
        self.seq = 0
        self.tx = self.rx = self.lost = self.bad = 0
        self.expect = None  # next sequence number expected from the peer
        self.sent_at: dict[int, float] = {}
        self.rtts: list[float] = []
        self.prev_dat: tuple[float, str] | None = None
        self.next_dat = time.monotonic() + 0.3
        self.next_status = time.monotonic() + 5
        self.next_button = time.monotonic() + 6 + self.world.rng.random() * 4
        self.release_at = 0.0
        self.pressed = False
        self.edges = 0
        self.burst: dict | None = None
        self.gen: dict | None = None
        self.gen_state = "idle"
        self.watches: dict[str, list] = {}
        self.live = None
        self.dash = []
        self.next_dash = 0.0
        self.boot_card()

    def ms(self) -> int:
        return int((time.monotonic() - self.t0) * 1000)

    # --- console output: the firmware's `out!`, `prompt_clear`, `prompt_draw`
    def w(self, text: str) -> None:
        self.out.put(text.encode())

    def verbosity(self) -> int:
        return ENUMS["verbose"].index(self.settings["verbose"])

    def prompt_clear(self) -> None:
        if self.block:
            return
        if self.app == "eth" and not self.sync_open:  # the UART firmware has no synchronized output
            self.sync_open = True
            self.w("\x1b[?2026h\x1b[?25l")
        rows = int(self.live is not None) + len(self.dash)
        self.w("\r\x1b[2K" + "\x1b[1A\r\x1b[2K" * rows)

    def prompt_draw(self) -> None:
        if self.block:
            return
        if self.live is not None:
            self.w(f"\x1b[?7l{self.live}\x1b[?7h\n")
        for row in self.dash:
            self.w(f"\x1b[?7l{row}\x1b[?7h\n")
        label = self.label or "cli"
        self.w(f"{BOLD}{PROMPT}{label} ›{RST} {self.line}")
        if self.sync_open:
            self.sync_open = False
            self.w("\x1b[?25h\x1b[?2026l")

    def out_line(self, text: str = "") -> None:
        self.prompt_clear()
        self.w(text + "\r\n")
        self.prompt_draw()

    def open_block(self) -> None:
        if self.app != "eth":  # the UART firmware has no block: every line redraws the prompt
            return
        self.prompt_clear()
        self.block += 1

    def close_block(self) -> None:
        if self.app != "eth":
            return
        self.block -= 1
        self.prompt_draw()

    def say(self, text: str) -> None:  # a line inside a block
        if self.app != "eth":
            return self.out_line(text)
        self.w(text + "\r\n")

    def ok(self, text: str) -> None:
        self.out_line(f"  {OK}✓{RST} {text}")

    def err(self, text: str) -> None:
        self.out_line(f"  {ERROR}✗ {text}{RST}")

    def stamp(self) -> str:
        ms = self.ms()
        return f"{TIME}{ms // 1000:>6}.{ms % 1000:03}{RST}"

    # --- frames
    def counters_suffix(self) -> str:
        tail = (
            f"{ERROR}lost {self.lost} bad {self.bad}{RST}"
            if self.lost or self.bad
            else f"{DIM}lost 0{RST}"
        )
        return f"  {DIM}│ tx {self.tx} rx {self.rx}{RST} {tail}"

    def frame_line(
        self, tx: bool, kind: str, seq: int, who: str, body: str, extra: str = ""
    ) -> None:
        v = self.verbosity()
        if v == 0:
            return
        arrow = f"{TX}TX ▶{RST}" if tx else f"{RX}RX ◀{RST}"
        line = f"{self.stamp()} {arrow} {BADGE[kind]} {kind} {RST} {DIM}#{seq:05}{RST} {NODE}{who:<8}{RST} {SEP}│{RST} {body}"
        if extra:
            line += f" {SEP}│{RST} {extra}"
        if v == 1:
            self.prompt_clear()
            self.live = line + self.counters_suffix()
            self.prompt_draw()
        else:
            self.out_line(line)

    def delta_text(self, now: float, tx: bool) -> str:
        prev, self.prev_dat = self.prev_dat, (now, "TX" if tx else "RX")
        if not prev:
            return ""
        us = int((now - prev[0]) * 1e6)
        return f"{DIM}Δ{RST} {BOLD}{us // 1000:>5}.{us % 1000:03}{RST} {DIM}ms{RST} {prev[1] == 'TX' and TX + '← TX' or RX + '← RX'}{RST}"

    def transmit(self, kind: str, **fields) -> int:
        self.seq += 1
        frame = {"kind": kind, "seq": self.seq, "from": self.name, "at": time.monotonic(), **fields}
        self.tx += 1
        self.world.send(self.peer, frame)
        return self.seq

    def send_dat(self, now: float) -> None:
        seq = self.transmit("DAT", pressed=self.pressed, events=self.edges)
        self.sent_at[seq] = now
        btn = f"{WARN}● pressed {RST}" if self.pressed else f"{DIM}○ released{RST}"
        self.frame_line(
            True,
            "DAT",
            seq,
            "",
            f"{DIM}btn{RST} {btn}  {DIM}event{RST} {self.edges:>5}",
            self.delta_text(now, True),
        )

    def receive(self, f: dict) -> None:
        now = time.monotonic()
        if f["kind"] in ("BURST_END", "GEN_END"):  # the end of a test: not a frame of the sequence
            return self.finish_burst_rx(f) if f["kind"] == "BURST_END" else self.finish_gen_rx(f)
        self.rx += 1
        if self.expect is not None and f["seq"] > self.expect:
            n = f["seq"] - self.expect
            self.lost += n
            if self.verbosity():
                self.out_line(f"{self.stamp()} {ERROR}RX ⚠  {n} frame(s) lost{RST}")
        self.expect = f["seq"] + 1
        kind = f["kind"]
        if kind == "DAT":
            btn = f"{WARN}● pressed {RST}" if f["pressed"] else f"{DIM}○ released{RST}"
            self.frame_line(
                False,
                "DAT",
                f["seq"],
                f["from"],
                f"{DIM}btn{RST} {btn}  {DIM}event{RST} {f['events']:>5}",
                self.delta_text(now, False),
            )
            if self.settings["tx_mode"] == "on-rx":
                seq = self.transmit("ECH", echo=f["seq"])
                self.frame_line(True, "ECH", seq, "", f"{DIM}echo of{RST} #{f['seq']:05}")
        elif kind == "EVT":
            mark = (
                f"{BOLD}{WARN}▼ press  {RST}"
                if f["edge"] == "FALL"
                else f"{BOLD}{ACCENT}▲ release{RST}"
            )
            self.frame_line(
                False, "EVT", f["seq"], f["from"], f"{mark}  {DIM}key{RST} {BOLD}{f['key']}{RST}"
            )
        elif kind == "ECH":
            sent = self.sent_at.pop(f["echo"], None)
            rtt = int((now - sent) * 1e6) + (200 if self.app == "eth" else 20) if sent else 0
            if sent:
                self.rtts.append(rtt)
            self.frame_line(
                False,
                "ECH",
                f["seq"],
                f["from"],
                f"{DIM}echo of{RST} #{f['echo']:05}",
                f"{DIM}rtt{RST} {BOLD}{rtt} µs{RST}",
            )

    # --- periodic work
    def step(self, now: float) -> None:
        while True:
            try:
                data = self.inbox.get_nowait()
            except queue.Empty:
                break
            for byte in data:
                self.key_in(byte)
        s = self.settings
        if s["tx_mode"] == "periodic" and now >= self.next_dat:
            self.next_dat = now + parse_period_ms(s["data_period_ms"]) / 1000
            self.send_dat(now)
        if now >= self.next_status:
            self.next_status = now + parse_period_ms(s["status_period_ms"]) / 1000
            if self.verbosity() >= 2:
                self.status_line()
        if now >= self.next_button and not self.pressed:
            self.next_button = now + 8 + self.world.rng.random() * 6
            self.press(True, now)
            self.release_at = now + 0.4
        if self.pressed and now >= self.release_at:
            self.press(False, now)
        if self.burst and now >= self.burst["end"]:
            self.end_burst()
        if self.gen and now >= self.gen["end"]:
            self.end_gen()
        if self.dash or (self.app == "eth" and s["trace_dash"] != "off"):
            if now >= self.next_dash:
                self.next_dash = now + 1.0
                self.redraw_dash()
        for path, w in list(self.watches.items()):
            if now >= w[1]:
                w[1] = now + w[0] / 1000
                value = self.space_value(path)
                if value != w[2]:
                    w[2] = value
                    self.out_line(
                        f"  {DIM}/{path.strip('/').rsplit('/', 1)[0]}/{RST}{ACCENT}{path.rsplit('/', 1)[-1]}{RST} {DIM}={RST} {BOLD}{value}{RST}"
                    )

    def press(self, down: bool, now: float) -> None:
        self.pressed = down
        self.edges += 1
        key = "".join(random.Random().choice("0123456789ABCDEF") for _ in range(16))
        edge = "FALL" if down else "RISE"
        seq = self.transmit("EVT", edge=edge, key=key)
        mark = f"{BOLD}{WARN}▼ press  {RST}" if down else f"{BOLD}{ACCENT}▲ release{RST}"
        self.frame_line(
            True, "EVT", seq, "", f"{mark}  {DIM}key{RST} {BOLD}{key}{RST}", f"{ACCENT}jumper{RST}"
        )

    def status_line(self) -> None:
        speed = (
            "1G"
            if self.app == "eth"
            else (
                f"{self.baud // 1_000_000}M"
                if self.baud % 1_000_000 == 0
                else f"{self.baud // 1000}k"
            )
        )
        line = f"{DIM}─ {self.ms() // 1000:>4}s ─ tx {self.tx:>5} · rx {self.rx:>5} · lost {self.lost:>2} · bad {self.bad:>2} ─{RST} {BOLD}{OK}● UP  {RST} {LINK}{speed:<4}{RST}"
        if self.rtts:
            lo, avg, hi = min(self.rtts), sum(self.rtts) / len(self.rtts), max(self.rtts)
            line += f" {DIM}─ rtt{RST} {BOLD}{lo / 1000:>5.2f}{RST} {DIM}/{RST} {avg / 1000:>5.2f} {DIM}/{RST} {hi / 1000:>7.2f} {DIM}ms{RST}"
            self.rtts = []
        self.out_line(line)
        # the two lines under the stats line: the link's counters (with the peer's DAT period), and on Ethernet the loop
        period = parse_period_ms(self.peer.settings["data_period_ms"])
        period_txt = f"{period / 1000:g} s" if period >= 1000 else f"{period:g} ms"
        kb = lambda n: f"{n * 0.06:.1f} kB"  # noqa: E731
        if self.app == "uart":
            self.peak = max(getattr(self, "peak", 0), self.world.rng.randint(20, 900))
            self.out_line(
                f"{DIM}  link{RST}  tx {kb(self.tx)} · rx {kb(self.rx)} · RX buffer peak {self.peak}/2048 · peer DAT period {period_txt}"
            )
        else:
            self.out_line(
                f"{DIM}  link{RST}  1 Gbit/s · tx {kb(self.tx)} · rx {kb(self.rx)} · rx queue overflow 0 · peer DAT period {period_txt}"
            )
        if self.app == "eth":
            self.out_line(
                f"{DIM}  loop{RST}  {22000 + self.world.rng.randint(0, 900)}/s · slowest {900 + self.world.rng.randint(0, 300)} µs · last tx waited 998 µs (task pickup 894 µs)"
            )

    def redraw_dash(self) -> None:
        s = self.settings
        self.prompt_clear()
        if self.app == "eth" and s["trace_dash"] != "off":
            bar = lambda n: "▁▂▃▄▅▆▇█"[min(7, n)] * 8  # noqa: E731
            self.dash = [
                f"  {RX}RX{RST} {bar(self.world.rng.randint(1, 7))} {self.rx} frames",
                f"  {TX}TX{RST} {bar(self.world.rng.randint(1, 7))} {self.tx} frames",
            ]
        else:
            self.dash = []
        self.prompt_draw()

    def boot_card(self) -> None:
        self.open_block()
        rows = [
            ("name", self.name), ("chip", "ESP32-S31 rev v0.0 · CPU 500 MHz"), ("MAC", f"1C:29:04:D0:6C:{0x1A + ord(self.key) - 97:02X}"),
            ("firmware", f"{self.app == 'eth' and 'ethernet' or 'uart'} v0.1.0"), ("git", "73cfe39 (feat/adc-stream)"),
            ("built", "2026-11-20 18:01:49 UTC"), ("reset", "PowerOn"),
        ]  # fmt: skip
        self.say(
            f"{ACCENT}╭─ {BOLD}link · shared, must match on both nodes{RST}{ACCENT} {'─' * 20}╮{RST}"
        )
        for label, value in rows:
            self.say(f"{ACCENT}│{RST}  {DIM}{label:<10}{RST}{value:<50}{ACCENT}│{RST}")
        self.say(f"{ACCENT}╰{'─' * 62}╯{RST}")
        self.close_block()

    # --- the keyboard: the line editor of `editor.rs` (what the page types)
    def key_in(self, byte: int) -> None:
        if byte == 0x15:  # Ctrl+U
            self.line = ""
        elif byte in (0x7F, 0x08):
            self.line = self.line[:-1]
        elif byte == 0x0D:
            line, self.line = self.line, ""
            self.submit(line)
            return
        elif 0x20 <= byte < 0x7F and len(self.line) < 96:
            self.line += chr(byte)
        else:
            return
        self.prompt_clear()
        self.prompt_draw()

    def submit(self, line: str) -> None:
        if self.wizard:
            self.prompt_clear()
            self.prompt_draw()
            return self.answer_wizard(line)
        if not line.strip():
            self.prompt_clear()
            self.prompt_draw()
            return
        self.out_line(f"{BOLD}{PROMPT}›{RST} {line}")
        self.run(line.strip())

    # --- the CLI
    def run(self, line: str) -> None:
        words = line.split()
        cmd, args = words[0], words[1:]
        s = self.settings
        if cmd in ("config", "cfg", "get"):
            return self.cmd_config()
        if cmd in ("set", "s"):
            if len(args) < 2:
                return self.err("usage: set <key> <value>")
            return self.set_key(args[0], " ".join(args[1:]))
        if cmd in ("reset", "r"):
            for k, v in KEYS:
                if not args or args[0] == k:
                    s[k] = v
            self.after_set()
            return self.ok(
                "back to the build value" if args else "every setting back to the build value"
            )
        if cmd == "led" or cmd == "l":
            if len(args) < 2 or args[0] not in ("1", "2"):
                return self.err("usage: led <1|2> <mode>")
            return self.set_key(f"led{args[0]}", " ".join(args[1:]))
        if cmd in ("next", "n"):
            names = ENUMS["rgb.pattern"]
            return self.set_key(
                "rgb.pattern", names[(names.index(s["rgb.pattern"]) + 1) % len(names)]
            )
        if cmd in ("verbose", "v", "trace", "t", "tr") and not args:
            key = SHORT[cmd]
            names = ENUMS[key]
            nxt = {"verbose": ("frames", "wire", "silent", "live")}.get(key)
            value = (
                nxt[names.index(s[key]) - 1]
                if nxt and False
                else names[(names.index(s[key]) + 1) % len(names)]
            )
            return self.set_key(key, value)
        if cmd in ("dash", "ds") and self.app == "eth":
            if not args:
                return self.set_key(
                    "trace_dash_view",
                    ENUMS["trace_dash_view"][
                        (ENUMS["trace_dash_view"].index(s["trace_dash_view"]) + 1) % 3
                    ],
                )
            key = "trace_dash_view" if args[0] in ENUMS["trace_dash_view"] else "trace_dash"
            return self.set_key(key, args[0])
        if cmd in SHORT and (
            self.app == "eth" or cmd not in ("trace", "t", "tr", "window", "win", "dump", "d")
        ):
            if not args:
                return self.err(f"usage: {cmd} <value>")
            return self.set_key(SHORT[cmd], " ".join(args))
        if cmd in ("tx",):
            return self.set_key("tx_mode", " ".join(args) or "periodic")
        if cmd in ("save", "w"):
            return self.ok("settings saved in flash")
        if cmd in ("forget", "fg"):
            return self.ok("saved settings erased")
        if cmd in ("reboot", "restart", "rb"):
            self.ok("rebooting…")
            self.line, self.label, self.wizard = "", None, []
            self.settings = dict(KEYS)
            self.boot()
            return
        if cmd in ("info", "about", "boot", "i"):
            return self.boot_card()
        if cmd in ("link", "net", "lk") and self.app == "eth":
            return self.cmd_link()
        if cmd in ("baud", "bd") and self.app == "uart":
            return self.cmd_baud(args)
        if cmd in ("burst", "bs"):
            return self.cmd_burst(args)
        if cmd in ("gen", "g") and self.app == "eth":
            return self.cmd_gen(args)
        if cmd in (
            "browse",
            "br",
            "tree",
            "read",
            "rd",
            "write",
            "wr",
            "watch",
            "wa",
            "unwatch",
            "uw",
        ):
            return self.cmd_space(cmd, args)
        if cmd in ("help", "h", "?"):
            self.open_block()
            self.say(f"  {BOLD}commands{RST} {DIM}(simulated node: a subset of the real CLI){RST}")
            self.say(
                "  config set reset verbose pattern next freq cycle bright led tx period burst browse read write watch"
            )
            self.close_block()
            return
        if cmd in ("list", "ls", "status", "st"):
            return self.ok(
                f"{s['rgb.pattern']} · {s['rgb.freq_hz']} Hz · cycle {s['rgb.cycle_ms']} ms · {s['rgb.brightness']}%"
            )
        self.err(f"unknown command '{cmd}' (try `help`)")

    def try_set(self, key: str, value: str) -> str | None:
        """Applies a setting like the firmware's `set`; returns why it was refused, or None."""
        s = self.settings
        if key not in s:
            return f"no such setting '{key}'"
        if key in ENUMS:
            if key == "tx_mode":
                head = value.split()[0]
                if head not in ENUMS[key]:
                    return f"{key}: periodic [period|freq], on-rx, quiet"
                if head == "periodic" and len(value.split()) > 1:
                    ms = parse_period_ms(value.split()[1])
                    if ms is None or not 10 <= ms <= 3.6e6:
                        return "period: 10-3600000 ms, or a frequency: 2.5Hz"
                    s["data_period_ms"] = num(ms)
                value = head
            elif key in ("led1", "led2"):
                pass
            elif value not in ENUMS[key] and not (value.isdigit() and int(value) < len(ENUMS[key])):
                return f"{key}: {', '.join(ENUMS[key])}"
            elif value.isdigit():
                value = ENUMS[key][int(value)]
        elif key in ("led1", "led2"):
            if value.split()[0] not in ("off", "on", "blink", "button", "peer", "link", "rx", "tx"):
                return "led mode: off on blink [period|freq] button peer link rx tx"
        elif key in RANGES:
            ms = parse_period_ms(value) if key.endswith(("_ms", "_hz")) else None
            n = ms if ms is not None else (float(value) if re.fullmatch(r"[\d.]+", value) else None)
            lo, hi = RANGES[key]
            if n is None or not lo <= n <= hi:
                return f"{key}: {num(lo)}-{num(hi)}"
            value = num(n)
        elif key == "burst.rate_hz":
            value = "0" if value in ("max", "0") else value
        s[key] = value
        self.after_set()
        return None

    def set_key(self, key: str, value: str) -> None:
        error = self.try_set(key, value)
        if error:
            return self.err(error)
        off = key in ("trace_dash_view", "trace_window_s") and self.settings["trace_dash"] == "off"
        self.ok(
            f"{key} {self.settings[key]}"
            + ("  (the dashboard is off: `dash normal` shows it)" if off else "")
        )

    def after_set(self) -> None:
        if self.settings["verbose"] != "live":
            self.prompt_clear()
            self.live = None
            self.prompt_draw()
        self.next_dat = min(
            self.next_dat,
            time.monotonic() + parse_period_ms(self.settings["data_period_ms"]) / 1000,
        )

    def cmd_config(self) -> None:
        self.open_block()
        s = self.settings
        self.say(
            f"  {BOLD}{'key':<22}{RST}{SEP}│{RST} {BOLD}{'value':<16}{RST}{SEP}│{RST} {DIM}build value{RST}"
        )
        fixed = [
            ("name", self.name),
            ("pins.button", "4"),
            ("pins.led1", "20"),
            ("pins.led2", "21"),
        ]
        if self.app == "eth":
            fixed += [("net.transport", "udp"), ("net.address", f"192.168.77.{11 if self.key == 'a' else 12}"), ("net.prefix_len", "24"),
                      ("net.peer", f"192.168.77.{12 if self.key == 'a' else 11}"), ("net.port", "7000"), ("net.tcp_role", "client"), ("net.max_speed", "1000")]  # fmt: skip
        else:
            fixed = (
                [("name", self.name), ("pins.uart_tx", "2"), ("pins.uart_rx", "3")]
                + fixed[1:]
                + [("shared.baud_rate", "5000000")]
            )
        for k, v in fixed:
            self.say(
                f"  {DIM}{k:<22}{RST}{SEP}│{RST} {DIM}{v:<16}{RST}{SEP}│{RST} {DIM}read-only (rebuild){RST}"
            )
        for k, build in KEYS:
            if s[k] == build:
                self.say(f"  {ACCENT}{k:<22}{RST}{SEP}│{RST} {s[k]:<16}{SEP}│{RST}")
            else:
                self.say(
                    f"  {ACCENT}{k:<22}{RST}{SEP}│{RST} {BOLD}{WARN}{s[k]:<16}{RST}{SEP}│{RST} {DIM}{build}  (changed){RST}"
                )
        self.close_block()

    def cmd_link(self) -> None:
        self.open_block()
        me, other = (11, 12) if self.key == "a" else (12, 11)
        self.say(f"  {BOLD}cable{RST}     {SEP}│{RST} {BOLD}{OK}● up{RST} · 1 Gbit/s · full duplex")
        self.say(
            f"  {BOLD}capacity{RST}  {SEP}│{RST} {DIM}1 Gbit/s max allowed · UDP frame rate ceiling is the CPU, not the wire{RST}"
        )
        self.say(
            f"  {BOLD}address{RST}   {SEP}│{RST} 192.168.77.{me}/24 {DIM}→ peer 192.168.77.{other}{RST}"
        )
        self.say(
            f"  {BOLD}transport{RST} {SEP}│{RST} UDP port 7000 {DIM}· no connection (datagrams){RST}"
        )
        self.say(f"  {BOLD}peer{RST}      {SEP}│{RST} {BOLD}{OK}● frames arriving{RST}")
        self.say(
            f"  {BOLD}counters{RST}  {SEP}│{RST} {DIM}tx {self.tx} frames / {self.tx * 0.06:.1f} kB · rx {self.rx} frames / {self.rx * 0.06:.1f} kB · rx queue overflow 0{RST}"
        )
        self.close_block()

    def cmd_baud(self, args: list[str]) -> None:
        if args:
            m = re.fullmatch(r"(\d+(?:\.\d+)?)([kKmM]?)", args[0])
            if not m:
                return self.err("baud: 1200 to 5000000 (115200, 921600, 2M...)")
            rate = int(float(m[1]) * {"": 1, "k": 1000, "m": 1_000_000}[m[2].lower()])
            if not 1200 <= rate <= 5_000_000:
                return self.err("baud: 1200 to 5000000")
            self.out_line(f"{self.stamp()} {LINK}LINK ⇄  baud {self.baud} → {rate}{RST}")
            self.baud = self.peer.baud = rate
            self.peer.out_line(f"{self.peer.stamp()} {LINK}LINK ⇄  baud → {rate}{RST}")
        b = self.baud
        name = (
            f"{b // 1_000_000} Mbaud"
            if b % 1_000_000 == 0
            else f"{b // 1000}.{b % 1000 // 100} kbaud"
        )
        self.out_line(
            f"  {BOLD}link{RST} {name} {DIM}· 8N1 · {b / 10 / 1000:.1f} kB/s max · boot speed 5 Mbaud{RST}"
        )

    def cmd_burst(self, args: list[str]) -> None:
        s = self.settings
        if not args:
            self.wizard = [
                ("burst length", s["burst.count"]),
                ("burst rate", s["burst.rate_hz"] if s["burst.rate_hz"] != "0" else "max"),
            ]
            self.wizard_answers: list[str] = []
            self.label = f"{self.wizard[0][0]} ({self.wizard[0][1]})"
            self.prompt_clear()
            self.prompt_draw()
            return
        count = int(args[0]) if args[0].isdigit() else 0
        rate = (
            0.0
            if len(args) < 2 or args[1] in ("max", "0")
            else (1000 / parse_period_ms(args[1]) if args[1][-1].isalpha() else float(args[1]))
        )
        if not 1 <= count <= 100000:
            return self.err("burst: 1-100000 frames")
        self.start_burst(count, rate)

    def answer_wizard(self, line: str) -> None:
        question, default = self.wizard[len(self.wizard_answers)]
        self.out_line(f"{BOLD}{WARN}{question} ›{RST} {line or '(default)'}")
        if line.strip() == "q":
            self.wizard, self.label = [], None
            self.ok("burst cancelled")
            return
        self.wizard_answers.append(line.strip() or default)
        if len(self.wizard_answers) < len(self.wizard):
            self.label = f"{self.wizard[1][0]} ({self.wizard[1][1]})"
            self.prompt_clear()
            self.prompt_draw()
            return
        count, rate = self.wizard_answers
        self.wizard, self.label = [], None
        self.cmd_burst([count, rate])

    def start_burst(self, count: int, rate: float) -> None:
        w = self.world
        per = 1 / rate if rate else (0.00012 if self.app == "eth" else 64 * 10 / self.baud)
        self.burst = {
            "id": getattr(self, "burst_id", 0) + 1,
            "count": count,
            "rate": rate,
            "end": time.monotonic() + count * per,
            "dur": count * per,
        }
        self.burst_id = self.burst["id"]
        what = (
            f"{count} frames back to back…"
            if not rate
            else f"{count} frames at {rate:g} Hz (one every {1000 / rate:g} ms)…"
        )
        self.out_line(f"{self.stamp()} {TX}TX ▶{RST} {BOLD}burst #{self.burst['id']}{RST}: {what}")
        del w

    def throughput(self, count: int, dur: float) -> str:
        total = count * 64
        bps = total / dur if dur else 0
        cap = 125e6 if self.app == "eth" else self.baud / 10
        rate = f"{bps / 1e6:.2f} MB/s" if bps >= 1e6 else f"{bps / 1e3:.1f} kB/s"
        size = f"{total / 1e3:.1f} kB" if total >= 1000 else f"{total} B"
        dur_txt = f"{dur * 1e3:.1f} ms" if dur < 1 else f"{dur:.2f} s"
        return f"{size} in {dur_txt} → {BOLD}{rate}{RST} ({bps / cap * 100:.1f}% of the {'link' if self.app == 'eth' else 'line'})"

    def end_burst(self) -> None:
        b, self.burst = self.burst, None
        self.out_line(
            f"{self.stamp()} {TX}TX ▶{RST} {BOLD}burst #{b['id']}{RST} sent: {b['count']} frames, {self.throughput(b['count'], b['dur'])}"
        )
        lost = self.world.rng.randint(0, 2) if self.world.loss else 0
        self.world.send(
            self.peer,
            {
                "kind": "BURST_END",
                "seq": 0,
                "from": self.name,
                "id": b["id"],
                "count": b["count"],
                "lost": lost,
                "dur": b["dur"],
                "at": time.monotonic(),
            },
        )
        self.peer.expect = None

    def finish_burst_rx(self, f: dict) -> None:
        mark = f"{BOLD}{OK}✓{RST}" if f["lost"] == 0 else f"{BOLD}{ERROR}✗{RST}"
        self.out_line(
            f"{self.stamp()} {RX}RX ◀{RST} {mark} {BOLD}burst #{f['id']}{RST} from {NODE}{f['from']}{RST}: {f['count'] - f['lost']}/{f['count']} frames · {f['lost']} lost · 0 bad"
        )
        self.out_line(
            f"{DIM}                    {RST}{self.throughput(f['count'] - f['lost'], f['dur'])}"
        )

    def cmd_gen(self, args: list[str]) -> None:
        if not args or args[0] == "status":
            return self.gen_status()
        sub = args[0]
        if sub in ("stop",):
            if self.gen:
                self.gen["end"] = 0
            return self.gen_status()
        if sub in ("pause", "resume"):
            if self.gen:
                self.gen_state = "paused" if sub == "pause" else "running"
            return self.gen_status()
        if sub not in ("start", "rate", "size", "shape", "for", "count"):
            return self.err("gen: start|stop|pause|resume|rate|size|shape|for|count")
        spec = getattr(
            self, "gen_spec", {"rate": "2.00 Mbit/s", "size": "512 B", "shape": "flat", "dur": 10.0}
        )
        for a in args[1:] if sub == "start" else args:
            if re.fullmatch(r"[\d.]+(?:mbit|kbit|gbit)", a.lower()):
                spec["rate"] = re.sub(
                    r"([\d.]+)(\w)bit",
                    lambda m: f"{float(m[1]):.2f} {m[2].upper() if m[2] != 'k' else 'k'}bit/s",
                    a.lower(),
                )
            elif re.fullmatch(r"\d+b", a.lower()):
                spec["size"] = a[:-1] + " B"
            elif re.fullmatch(r"\d+(?:s|min)", a):
                spec["dur"] = float(a[:-1]) * (60 if a.endswith("min") else 1)
            elif ":" in a:
                spec["shape"] = a
        self.gen_spec = spec
        if sub == "start":
            bits = parse_rate_bits(spec["rate"])
            dur = min(spec["dur"], 30.0)
            self.gen = {
                "id": getattr(self, "gen_id", 0) + 1,
                "end": time.monotonic() + dur,
                "start": time.monotonic(),
                "bits": bits,
                "size": 512,
            }
            self.gen_id = self.gen["id"]
            self.gen_state = "running"
        self.gen_status()

    def gen_status(self) -> None:
        spec = getattr(
            self, "gen_spec", {"rate": "2.00 Mbit/s", "size": "512 B", "shape": "flat", "dur": 10.0}
        )
        state = self.gen_state if self.gen else ("done" if self.gen_state == "done" else "idle")
        mark = {"idle": "○", "running": "●", "paused": "‖", "done": "✓"}[state]
        paint = {"idle": DIM, "running": OK, "paused": WARN, "done": ACCENT}[state]
        self.open_block()
        self.say(
            f"  {BOLD}gen{RST}      {SEP}│{RST} {BOLD}{paint}{mark} {state}{RST}  {DIM}rate{RST} {spec['rate']} · {DIM}size{RST} {spec['size']} · {DIM}shape{RST} {spec['shape']} · stops after {spec['dur']:.2f} s"
        )
        if self.gen:
            run = time.monotonic() - self.gen["start"]
            sent = int(self.gen["bits"] / 8 / self.gen["size"] * run)
            self.say(
                f"  {'':<8} {SEP}│{RST} {DIM}stream #{self.gen['id']}: sent{RST} {sent} {DIM}datagrams,{RST} {sent * 0.558:.1f} kB {DIM}in{RST} {run:.2f} s {DIM}· socket full 0 times{RST}"
            )
        self.close_block()

    def end_gen(self) -> None:
        g, self.gen = self.gen, None
        dur = max(time.monotonic() - g["start"], 0.01)
        pps = int(g["bits"] / 8 / g["size"])
        sent = int(pps * dur)
        self.gen_state = "done"
        wire = sent * (g["size"] + 46)
        self.out_line(
            f"{self.stamp()} {TX}TX ▶{RST} {BOLD}gen #{g['id']}{RST} {OK}done{RST} {SEP}│{RST} {sent} datagrams, {wire / 1e3:.1f} kB in {dur:.2f} s {SEP}│{RST} {BOLD}{g['bits'] / 1e6:.2f} Mbit/s{RST} · {pps} pkt/s {DIM}· {g['bits'] / 1e9 * 100:.1f}% of the link{RST}"
        )
        lost = self.world.rng.randint(0, 3)
        self.world.send(
            self.peer,
            {
                "kind": "GEN_END",
                "seq": 0,
                "from": self.name,
                "id": g["id"],
                "sent": sent,
                "lost": lost,
                "bits": g["bits"],
                "pps": pps,
                "dur": dur,
                "at": time.monotonic(),
            },
        )
        self.peer.expect = None

    def finish_gen_rx(self, f: dict) -> None:
        got = f["sent"] - f["lost"]
        lost = ERROR if f["lost"] else DIM
        pct = f["lost"] / max(f["sent"], 1) * 100
        self.out_line(
            f"{self.stamp()} {RX}RX ◀{RST} {BOLD}gen #{f['id']}{RST} {SEP}│{RST} {got} received · {lost}lost {f['lost']} ({pct:.2f}%){RST} · {DIM}out of order{RST} 0 · {DIM}jitter{RST} 14 µs "
            f"{SEP}│{RST} {BOLD}{f['bits'] / 1e6:.2f} Mbit/s{RST} · {f['pps']} pkt/s {DIM}· {f['bits'] / 1e9 * 100:.1f}% of the link · {f['dur']:.2f} s{RST}"
        )

    # --- the address space (UA-lite): the peer answers
    def cmd_space(self, cmd: str, args: list[str]) -> None:
        peer = self.peer
        if cmd in ("browse", "br", "tree"):
            path = args[0] if args and args[0].startswith("/") else "/"
            depth = int(args[-1]) if args and args[-1].isdigit() else 1
            kids = [
                (p, k, rw)
                for p, k, rw in SPACE
                if p.startswith(path.rstrip("/") + "/")
                and p.count("/") <= path.rstrip("/").count("/") + depth
            ]
            self.open_block()
            self.say(f"  {BOLD}{path}{RST}" + (f" {DIM}· {depth} levels{RST}" if depth > 1 else ""))
            if not kids and path == "/":
                kids = (
                    [(p, k, rw) for p, k, rw in SPACE if p.count("/") == 1]
                    if depth == 1
                    else list(SPACE)
                )
            objects = variables = 0
            for i, (p, k, rw) in enumerate(kids):
                level = p.count("/") - 1 - path.rstrip("/").count("/")
                name = p.rsplit("/", 1)[-1]
                last = i + 1 == len(kids) or kids[i + 1][0].count("/") < p.count("/")
                stem = "    " * level + ("└── " if last else "├── ")
                if k == "O":
                    objects += 1
                    self.say(f"  {DIM}{stem}{RST}{BOLD}\x1b[34m{name}/{RST}")
                else:
                    variables += 1
                    ty = {"B": "bool", "U": "u32", "T": "text"}[k]
                    pad = " " * max(30 - len(stem) - len(name), 1)
                    self.say(
                        f"  {DIM}{stem}{RST}{ACCENT}{name}{RST}{pad} {DIM}{ty:<5}{RST}{WARN if rw else DIM}{'rw' if rw else 'ro'}{RST}"
                    )
            self.say(
                f"  {DIM}{objects} object{'s' if objects != 1 else ''}, {variables} variable{'s' if variables != 1 else ''}{RST}"
            )
            self.close_block()
            return
        if not args:
            return self.err(f"usage: {cmd} <path>")
        path = args[0]
        known = {p: (k, rw) for p, k, rw in SPACE}
        if cmd in ("read", "rd"):
            return self.space_line(
                path, peer.space_value(path) if path in known and known[path][0] != "O" else None
            )
        if cmd in ("write", "wr"):
            if path not in known or not known[path][1]:
                return self.space_line(
                    path, None, "BadNotWritable" if path in known else "BadNodeIdUnknown"
                )
            value = " ".join(args[1:])
            if path == "/rgb/brightness" and not (value.isdigit() and int(value) <= 100):
                return self.space_line(path, peer.space_value(path), "BadOutOfRange")
            if path.startswith(
                "/config/"
            ):  # the same check as the peer's own `set`: refused, the old value stays
                if peer.try_set(path[len("/config/") :], value):
                    return self.space_line(path, peer.space_value(path), "BadOutOfRange")
                return self.space_line(path, peer.space_value(path))
            peer.apply_space(path, value)
            return self.space_line(path, peer.space_value(path))
        if cmd in ("watch", "wa"):
            ms = parse_period_ms(args[1]) if len(args) > 1 else 250
            self.watches[path] = [ms, 0.0, None]
            return
        if cmd in ("unwatch", "uw"):
            self.watches.pop(path, None) if args else self.watches.clear()

    def space_line(self, path: str, value: str | None, status: str = "Good") -> None:
        branch, leaf = path.rsplit("/", 1)
        cell = f"{DIM}{branch}/{RST}{ACCENT}{leaf}{RST}" + " " * max(26 - len(path), 0)
        if status == "Good" and value is not None:
            self.out_line(f"  {cell} {DIM}={RST} {BOLD}{value}{RST}")
        else:
            status = status if value is not None or status != "Good" else "BadNodeIdUnknown"
            self.out_line(
                f"  {cell} {ERROR}✗ {status}{RST}"
                + (f" {DIM}(still {value}){RST}" if value else "")
            )

    def space_value(self, path: str) -> str:
        s = self.settings
        if path.startswith("/config/"):
            return s.get(path[len("/config/") :], "")
        return {
            "/node/name": self.name, "/node/uptime": str(self.ms()), "/leds/1": s["led1"], "/leds/2": s["led2"],
            "/buttons/jumper": str(int(self.pressed)), "/buttons/boot": "0", "/rgb/pattern": s["rgb.pattern"],
            "/rgb/brightness": s["rgb.brightness"], "/stats/tx": str(self.tx), "/stats/rx": str(self.rx), "/stats/lost": str(self.lost),
        }.get(path, "")  # fmt: skip

    def apply_space(self, path: str, value: str) -> None:
        key = {
            "/leds/1": "led1",
            "/leds/2": "led2",
            "/rgb/pattern": "rgb.pattern",
            "/rgb/brightness": "rgb.brightness",
        }[path]
        self.settings[key] = value
        self.prompt_clear()
        self.prompt_draw()


def parse_rate_bits(text: str) -> float:
    m = re.match(r"([\d.]+)\s*(k|M|G)?bit/s", text)
    return float(m[1]) * {None: 1, "k": 1e3, "M": 1e6, "G": 1e9}[m[2]] if m else 1e6
