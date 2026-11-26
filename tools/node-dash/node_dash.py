#!/usr/bin/env python3
"""Dashboard for the two board-to-board apps (`firmware/ethernet/` and `firmware/uart/`), in your browser.

    python3 scripts/dev.py boards assign a 6C:1A     # once: which board is node A / node B
    python3 scripts/dev.py dash                      # the page for both boards (close their serial monitors first)
    python3 scripts/dev.py dash --demo               # two simulated nodes, no hardware (--demo uart for the UART app)

It is the CLI you type in the serial terminal, with a page around it: each board's USB console is opened
here, what the board prints is shown as in a terminal (colours, the live line, the traffic dashboard) and
read for numbers (counters, rates, round-trip times, link state, bursts, the settings), and everything the
CLI does is a control: verbose level, RGB pattern and brightness, LEDs, tx mode and period, baud rate,
burst, the traffic generator, the network trace, the peer's address space, `config` / `set` / `save`...
What is typed in a page terminal goes to the board like a key press in `espflash monitor`.

The peer's settings travel over the link the app uses (Ethernet or UART): the board is asked to `read` / `write`
the peer's `/config/<key>` variables, one per setting, and the page shows both sides in a table to pull (set here to
the peer's values) or push (write on the peer) the ones you choose.

How it fits together:

    board A ──USB serial──▶ Board (thread: bytes → Screen → lines → numbers)  ──┐
    board B ──USB serial──▶ Board                                              ├─▶ GET /events (Server-Sent Events) ─▶ page
                                    ▲                                          ┘
                 POST /api/cmd ─────┘  (a line of the CLI, sent as if typed)

Nothing is added to the firmware: the page reads the console text the firmware already prints, so it works
with the boards as they are flashed. The board's verbose level decides what there is to read: `live` (the
default) gives the counters and the last frame, `frames` or `wire` every frame (round-trip times, the interval
between data frames), so the page asks for `frames` when you click "Charts need frames".

The serial port is opened without touching DTR/RTS: opening it the other way resets the chip (the USB
Serial/JTAG reads that as a reset request), and the node would restart each time this tool does.
Only one program can hold a port: stop `espflash monitor` / `dev.py monitor` first.
"""

from __future__ import annotations

import argparse
import codecs
import collections
import http.client
import http.server
import importlib.util
import json
import re
import sys
import threading
import time
import urllib.parse
import webbrowser
from pathlib import Path

PAGE = Path(__file__).with_name("node_dash.html")
ROOT = Path(__file__).resolve().parent.parent.parent
CDN_PLOTLY = "https://cdnjs.cloudflare.com/ajax/libs/plotly.js/2.35.2/plotly.min.js"
MAX_CMD = 96  # the board's line editor takes 96 characters (cli::MAX_INPUT)
NODES = ("a", "b")
# First words of the commands that change a setting: the page asks `config` again after them.
SETTERS = {
    "set", "s", "reset", "r", "verbose", "v", "pattern", "p", "next", "n", "freq", "f", "cycle", "c",
    "bright", "b", "led", "l", "tx", "period", "pd", "trace", "t", "tr", "dash", "ds", "window", "win",
    "dump", "d", "baud", "bd", "forget", "fg", "save", "w",
}  # fmt: skip


# --------------------------------------------------------------------------------------------
# The terminal: the board's bytes (text with ANSI escape codes) become lines
# --------------------------------------------------------------------------------------------


def _color256(n: int):
    """A 256-colour index as what the page draws: 0-15 stay an index (the page's palette), the rest are hex."""
    if n < 16:
        return n
    if n < 232:
        n -= 16
        r, g, b = (n // 36) % 6, (n // 6) % 6, n % 6
        return "#%02x%02x%02x" % tuple(0 if v == 0 else 55 + 40 * v for v in (r, g, b))
    v = 8 + 10 * (n - 232)
    return "#%02x%02x%02x" % (v, v, v)


class Screen:
    """Reads the console bytes like a terminal would, and keeps what a log needs.

    The firmware draws a *block* at the bottom of the screen, under the text it prints: the live line (the
    last frame, redrawn in place), the traffic dashboard, and the `cli ›` prompt. Each time it prints it
    erases the block (`ESC[2K`, `ESC[1A`...), prints the line, and draws the block again, all between
    `ESC[?2026h` and `ESC[?2026l` (synchronized output). So:

    - a row finished by a newline is a log line, except the rows drawn with auto-wrap off (`ESC[?7l`),
      which belong to the block: they are kept as `live` and `dash`, not logged;
    - what is on the cursor's row at `ESC[?2026l` is the prompt.
    """

    def __init__(self) -> None:
        self._dec = codecs.getincrementaldecoder("utf-8")("replace")
        self._cells: list[tuple[str, tuple]] = []
        self._col = 0
        self._style: tuple = (None, None, 0)  # foreground, background, bold(1) / faint(2)
        self._state = 0  # 0 text, 1 after ESC, 2 inside a CSI sequence
        self._csi = ""
        # The Ethernet firmware wraps each redraw of the block in `ESC[?2026h` ... `ESC[?2026l`; the UART one doesn't:
        # there, the block ends where the next erase begins (see `_csi_done`), or when the bytes stop.
        self._sync = False
        self._block = False  # the current row was drawn with auto-wrap off
        self._pending: list[list] = []  # block rows since the last `ESC[?2026h`
        self.live: list | None = None  # runs of the live line
        self.dash: list[list] = []  # runs of each row of the traffic dashboard
        self.prompt = ""  # plain text of the prompt row, e.g. `cli › help`
        self.rows: list[list] = []  # finished log rows since the last `take()`

    def take(self) -> list[list]:
        rows, self.rows = self.rows, []
        return rows

    def feed(self, data: bytes) -> None:
        for ch in self._dec.decode(data):
            self._char(ch)
        if (
            not self._sync
        ):  # no `ESC[?2026l` to say the block is complete: what is drawn now is shown meanwhile
            self._publish(final=False)

    # --- one character
    def _char(self, ch: str) -> None:
        if self._state == 1:
            self._state = 2 if ch == "[" else 0
            self._csi = ""
            return
        if self._state == 2:
            if "\x40" <= ch <= "\x7e":
                self._state = 0
                self._csi_done(self._csi, ch)
            else:
                self._csi += ch
            return
        if ch == "\x1b":
            self._state = 1
        elif ch == "\n":
            self._finish_row()
        elif ch == "\r":
            self._col = 0
        elif ch == "\t":
            for _ in range(4 - self._col % 4):
                self._put(" ")
        elif ch >= " " and ch != "\x7f":
            self._put(ch)

    def _put(self, ch: str) -> None:
        cell = (ch, self._style)
        if self._col < len(self._cells):
            self._cells[self._col] = cell
        else:
            self._cells.append(cell)
        self._col += 1

    def _runs(self) -> list:
        runs: list = []
        for ch, st in self._cells:
            if runs and runs[-1][1:] == list(st):
                runs[-1][0] += ch
            else:
                runs.append([ch, *st])
        return runs

    def _finish_row(self) -> None:
        runs = self._runs()
        if self._block:
            self._pending = (self._pending + [runs])[-40:]
        else:
            self.rows.append(runs)
        self._cells, self._col, self._block = [], 0, False

    # --- escape sequences
    def _csi_done(self, params: str, final: str) -> None:
        if final == "m":
            self._sgr([int(p) if p.isdigit() else 0 for p in params.split(";")] if params else [0])
        elif final == "K":
            mode = params or "0"
            if mode == "2" and not self._sync and self._cells:
                self._publish(
                    final=True
                )  # the firmware erases its block to redraw it: this one is complete
            if mode == "0":
                del self._cells[self._col :]
            elif mode == "1":
                self._cells[: self._col] = [(" ", (None, None, 0))] * min(
                    self._col, len(self._cells)
                )
            else:
                self._cells.clear()
        elif final in "hl" and params.startswith("?"):
            if params == "?7" and final == "l":
                self._block = True
            elif params == "?2026":
                self._sync = True
                if final == "h":
                    self._pending = []
                else:
                    self._sync_end()
        # cursor movements (A B C D T H J...) change where the block is drawn, not what it says

    def _sync_end(self) -> None:
        self._publish(final=True)
        self._cells, self._col = [], 0

    def _publish(self, final: bool) -> None:
        """The block as drawn: the live line, the dashboard rows, the prompt. `final`: the next one starts."""
        pending = self._pending
        if pending and FRAME_RE.match(plain(pending[0])):
            self.live, self.dash = pending[0], list(pending[1:])
        else:
            self.live, self.dash = None, list(pending)
        text = "".join(ch for ch, _ in self._cells).strip()
        if "›" in text:  # a half-printed log line is not a prompt
            self.prompt = text
        if final:
            self._pending = []

    def _sgr(self, codes: list[int]) -> None:
        fg, bg, fl = self._style
        i = 0
        while i < len(codes):
            c = codes[i]
            if c == 0:
                fg, bg, fl = None, None, 0
            elif c == 1:
                fl = (fl & ~2) | 1
            elif c == 2:
                fl = (fl & ~1) | 2
            elif c == 22:
                fl = 0
            elif 30 <= c <= 37:
                fg = c - 30
            elif 90 <= c <= 97:
                fg = c - 90 + 8
            elif c == 39:
                fg = None
            elif 40 <= c <= 47:
                bg = c - 40
            elif 100 <= c <= 107:
                bg = c - 100 + 8
            elif c == 49:
                bg = None
            elif c in (38, 48) and i + 1 < len(codes):
                if codes[i + 1] == 5 and i + 2 < len(codes):
                    color, i = _color256(codes[i + 2]), i + 2
                elif codes[i + 1] == 2 and i + 4 < len(codes):
                    color, i = "#%02x%02x%02x" % tuple(codes[i + 2 : i + 5]), i + 4
                else:
                    color = None
                if c == 38:
                    fg = color
                else:
                    bg = color
            i += 1
        self._style = (fg, bg, fl)


def plain(runs: list) -> str:
    return "".join(r[0] for r in runs)


# --------------------------------------------------------------------------------------------
# What the firmware prints, read for numbers (the lines are in term.rs of both apps)
# --------------------------------------------------------------------------------------------

#    12.513 TX ▶  DAT  #00004          │ btn ○ released │ event    3 │ Δ   357.206 ms ← RX
FRAME_RE = re.compile(
    r"^\s*(\d+)\.(\d{3})\s+(TX|RX)\s*[▶◀]\s+(DAT|EVT|BAU|ECH|BST)\s+#(\d+)\s*(.*)$"
)
#   the live line adds:  │ tx 12 rx 12 lost 0 bad 0
LIVE_RE = re.compile(r"│\s*tx\s+(\d+)\s+rx\s+(\d+)\s+lost\s+(\d+)(?:\s+bad\s+(\d+))?\s*$")
#   ─  485s ─ tx   242 · rx   242 · lost  0 · bad  0 ─ ● UP   1G   ─ rtt  4.08 /  8.55 / 1005.62 ms
STATUS_RE = re.compile(
    r"^\s*─\s*(\d+)s\s*─\s*tx\s+(\d+)\s*·\s*rx\s+(\d+)\s*·\s*lost\s+(\d+)\s*·\s*bad\s+(\d+)\s*─\s*●\s*(UP|DOWN)\s*(\S*)"
    r"(?:\s*─\s*rtt\s+([\d.]+)\s*/\s*([\d.]+)\s*/\s*([\d.]+)\s*ms)?"
)
LOOP_RE = re.compile(r"^\s+loop\s+(\d+)/s\s*·\s*slowest\s+(\d+)\s*µs")
PEERPERIOD_RE = re.compile(r"^\s+link\s+.*peer DAT period\s+(.+?)\s*$")
RXPEAK_RE = re.compile(r"RX buffer peak\s+(\d+)/(\d+)")
CONFIG_RE = re.compile(r"^\s+([a-z][a-z0-9_.]*)\s+│\s+(.*?)\s*│\s*(.*?)\s*$")
CARD_RE = re.compile(r"^│\s\s(.{10})(.*?)\s*│$")
NET_RE = re.compile(r"^\s*(cable|capacity|address|transport|peer|counters)\s+│\s*(.*?)\s*$")
RTT_RE = re.compile(r"\brtt\s+([\d.]+)\s*(µs|ms)")
DELTA_RE = re.compile(r"Δ\s+(-?[\d.]+)\s*ms\s*←\s*(TX|RX)")
BURST_START_RE = re.compile(
    r"TX ▶\s+burst #(\d+): (\d+) frames (?:back to back|at ([\d.]+) (Hz|kHz))"
)
BURST_SENT_RE = re.compile(
    r"TX ▶\s+burst #(\d+) sent: (\d+) frames, (.+?) in (.+?) → (.+?) \(([\d.]+)% of the (?:link|line)\)"
)
BURST_RECV_RE = re.compile(
    r"RX ◀\s+([✓✗])\s+burst #(\d+) from (\S+): (\d+)/(\d+) frames · (\d+) lost · (\d+) bad"
)
THROUGHPUT_RE = re.compile(r"^\s+(.+?) in (.+?) → (.+?) \(([\d.]+)% of the (?:link|line)\)\s*$")
GEN_TX_RE = re.compile(
    r"TX ▶\s+gen #(\d+)\s+done\s+│\s+(\d+) datagrams, (.+?) in (.+?)\s+│\s+(\S+ \S+) · (\d+) pkt/s\s*· ([\d.]+)% of the link"
)
GEN_RX_RE = re.compile(
    r"RX ◀\s+gen #(\d+)\s+│\s+(\d+) received · lost (\d+) \(([\d.]+)%\) · out of order (\d+) · jitter (\d+) µs"
    r"\s+│\s+(\S+ \S+) · (\d+) pkt/s\s*· ([\d.]+)% of the link · (.+?)(?: · no end marker.*)?$"
)
GEN_STATE_RE = re.compile(
    r"^\s+gen\s+│\s+[○●‖✓]\s+(idle|running|paused|done)\s+rate (.+?) · size (.+?) · shape (.+?) · "
    r"(runs until stopped|stops after (.+?))(?: \(the shape's length\))?\s*$"
)
#           │ stream #1: sent 1464 datagrams, 750.0 kB in 3.00 s · socket full 0 times
GEN_STREAM_RE = re.compile(
    r"│\s*stream #(\d+): sent (\d+) datagrams, (.+?) in (.+?) · socket full (\d+) times"
)
LOST_RE = re.compile(r"RX ⚠\s+(\d+) frame\(s\) lost")
RESTART_RE = re.compile(r"RX ↻\s+(\S+) restarted")
BAD_RE = re.compile(r"RX ✗\s+(bad checksum|invalid frame)")
LINK_EVENT_RE = re.compile(r"LINK ⇄\s+(.*)$")
TREE_HEAD_RE = re.compile(r"^\s{2}(/\S*)(?: · (\d) levels)?\s*$")
TREE_ROW_RE = re.compile(
    r"^\s{2}((?:│   |    )*)(?:├──|└──) (\S+?)(/?)(?:\s+(bool|u32|text)\s+(rw|ro))?\s*$"
)
VALUE_RE = re.compile(r"^\s{2}(/\S*)\s+=\s+(.*?)\s*$")
VALUE_ERR_RE = re.compile(r"^\s{2}(/\S*)\s+✗\s+(\w+)(?:\s+\(still (.*)\))?\s*$")
WIRE_RE = re.compile(r"^\s+wire\s")
ECHO_RE = re.compile(r"^›\s+(.*)$")

UNITS = {"B": 1, "kB": 1e3, "MB": 1e6, "GB": 1e9}


def parse_bytes(text: str) -> float:
    m = re.match(r"([\d.]+)\s*(B|kB|MB|GB)", text)
    return float(m[1]) * UNITS[m[2]] if m else 0.0


def parse_bps(text: str) -> float:
    """`12.2 kB/s` -> bytes per second."""
    return parse_bytes(text.replace("/s", ""))


def parse_bitrate(text: str) -> float:
    """`5.00 Mbit/s` -> bit/s."""
    m = re.match(r"([\d.]+)\s*(bit|kbit|Mbit|Gbit)/s", text)
    return float(m[1]) * {"bit": 1, "kbit": 1e3, "Mbit": 1e6, "Gbit": 1e9}[m[2]] if m else 0.0


def parse_dur_ms(text: str) -> float:
    """`850.0 ms`, `2.00 s`, `1 min 5 s`, `850 µs` -> ms."""
    m = re.match(r"(\d+) min (\d+) s", text)
    if m:
        return (int(m[1]) * 60 + int(m[2])) * 1000.0
    m = re.match(r"([\d.]+)\s*(µs|ms|s)", text)
    return float(m[1]) * {"µs": 0.001, "ms": 1, "s": 1000}[m[2]] if m else 0.0


def classify(text: str) -> str:
    """The log level of a printed line, for the page's filters."""
    if FRAME_RE.match(text):
        return "frame"
    if STATUS_RE.match(text) or LOOP_RE.match(text) or PEERPERIOD_RE.match(text):
        return "stat"
    if WIRE_RE.match(text):
        return "debug"
    if ECHO_RE.match(text):
        return "cmd"
    if "✗" in text:
        return "error"
    if "⚠" in text or "↻" in text:
        return "warn"
    if "✓" in text:
        return "ok"
    return "info"


class Series:
    """Points appended over time; a page asks for what came after the last one it has."""

    def __init__(self, maxlen: int) -> None:
        self.items: collections.deque = collections.deque(maxlen=maxlen)
        self.n = 0

    def add(self, item) -> None:
        self.items.append(item)
        self.n += 1

    def since(self, cursor: int) -> tuple[list, int]:
        fresh = min(max(self.n - cursor, 0), len(self.items))
        return (list(self.items)[len(self.items) - fresh :] if fresh else []), self.n


# --------------------------------------------------------------------------------------------
# The serial port
# --------------------------------------------------------------------------------------------


class SerialTransport:
    """The board's USB Serial/JTAG port. DTR and RTS are left as the system sets them: lowering both
    (what `dtr=False, rts=False` does) is a reset request for the chip."""

    def __init__(self, port: str) -> None:
        import serial

        self.port = port
        self.ser = serial.Serial(port, 115200, timeout=0.1)

    def read(self) -> bytes:
        return self.ser.read(4096)

    def write(self, data: bytes) -> None:
        self.ser.write(data)
        self.ser.flush()

    def close(self) -> None:
        try:
            self.ser.close()
        except OSError:
            pass


# --------------------------------------------------------------------------------------------
# One board
# --------------------------------------------------------------------------------------------


class Board:
    """A node's console: the connection, the log, and everything read from what it prints."""

    def __init__(
        self, key: str, factory, port: str | None, note: str = "", app: str | None = None
    ) -> None:
        self.key = key
        self.app_hint = (
            app  # "eth" / "uart" when told: the page knows which controls to show before `config`
        )
        self.factory = factory  # () -> transport, or None when no board is assigned
        self.port = port
        self.note = note
        self.lock = threading.Lock()
        self.stop_flag = threading.Event()
        self.transport = None
        self.status = "unassigned" if factory is None else "connecting"
        self.detail = note
        self.screen = Screen()
        self.logs: collections.deque = collections.deque(maxlen=2500)
        self.log_n = 0
        self.last_rx = 0.0
        self.series = {
            "rate": Series(3600),  # [t, tx frames/s, rx frames/s]
            "rtt": Series(4000),  # [t, round-trip time µs]
            "rttstat": Series(1000),  # [t, min, avg, max ms]: the status line's
            "delta": Series(4000),  # [t, ms since the previous DAT, which way that one went]
            "marks": Series(500),  # [t, kind, text]: lost frames, link events, reboots, bad frames
            "events": Series(300),  # [t, dir, edge, key, who, button]: button edges
            "tests": Series(200),  # one burst / gen result each
        }
        self.frames: collections.deque = collections.deque(maxlen=20000)  # for the CSV
        self._seen: collections.deque = collections.deque(maxlen=64)
        self.reset_state()
        self.app = self.app_hint
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._refresh_at = 0.0
        self._quiet: dict[str, float] = (
            {}
        )  # command -> until when its answer is kept out of the log
        self._gen_poll = 0.0
        self._live_text = ""
        self._last_sample = (time.monotonic(), None, None)
        self.typed = 0  # lines sent from here
        self.other: Board | None = None  # the other node's board, set by the Server
        # The peer's settings, read (`read /config/<key>`) and written (`write /config/<key> <value>`) over the
        # link by the board itself: Ethernet or UART, whichever the app uses. See `peer_action`.
        self.peer: dict = {
            "state": "idle",
            "op": None,
            "rows": [],
            "done": 0,
            "total": 0,
            "error": "",
            "n": 0,
        }
        self._sync_active = False
        self._sync_events: dict[str, threading.Event] = {}
        self._sync_values: dict[str, dict] = (
            {}
        )  # the answers, apart from the address space table of the page

    def reset_state(self) -> None:
        self.name: str | None = None
        self.app: str | None = None  # "eth" | "uart", from the settings
        self.settings: dict[str, list] = {}  # key -> [value, build value, read-only, changed]
        self.card: dict[str, str] = {}
        self.net: dict[str, str] = {}
        self.counters: dict = {"tx": None, "rx": None, "lost": None, "bad": None}
        self.counted = False  # counters come from counting printed lines, not from the board
        self.link_up: bool | None = None
        self.speed = ""
        self.rtt_stat: list | None = None
        self.uptime_s: int | None = None
        self.loop: list | None = None  # [loops/s, slowest µs]
        self.peer_period = ""
        self.rx_peak: list | None = None
        self.last_frame: dict | None = None
        self.peer_btn: str | None = None  # the peer's button, from its last DAT
        self.own_btn: str | None = None
        self.boots = 0
        self.gen: dict | None = None
        self.burst_run: dict | None = (
            None  # a burst this node is sending: {id, count, rate_hz (0 = back to back), t}
        )
        self.peer_last: float | None = (
            None  # when the last frame from the peer arrived (host clock)
        )
        self.space = {"tree": [], "values": {}, "v": 0}
        self._tree_base = "/"
        self._tree_stack: list[str] = []
        self._pending_burst: dict | None = None

    # --- life of the connection
    def start(self) -> None:
        self._thread.start()

    def close(self) -> None:
        self.stop_flag.set()
        self._thread.join(timeout=2)
        if self.transport:
            self.transport.close()

    def host_log(self, level: str, text: str) -> None:
        with self.lock:
            self._store({"src": "host", "level": level, "runs": [[text, None, None, 0]]})

    def _store(self, entry: dict) -> None:
        self.log_n += 1
        entry["i"] = self.log_n
        entry["t"] = time.time()
        self.logs.append(entry)

    def _run(self) -> None:
        while not self.stop_flag.is_set():
            if self.factory is None:
                self.stop_flag.wait(1)
                continue
            try:
                transport = self.factory()
            except Exception as e:  # port busy, unplugged...
                msg = getattr(e, "strerror", None) or str(e)
                with self.lock:
                    self.status, self.detail = "error", msg
                self.stop_flag.wait(2)
                continue
            self.transport = transport
            with self.lock:
                self.status, self.detail = "connected", self.port or ""
            self.host_log("ok", f"connected to {self.port}")
            self.request_config(delay=0.4)
            try:
                while not self.stop_flag.is_set():
                    data = transport.read()
                    if data:
                        self._on_bytes(data)
            except Exception as e:  # unplugged
                self.host_log("warn", f"connection lost: {getattr(e, 'strerror', None) or e}")
            finally:
                transport.close()
                self.transport = None
                with self.lock:
                    self.status, self.detail = "connecting", "waiting for the port"
                    self.link_up = None
            self.stop_flag.wait(1)

    # --- sending
    def send(self, text: str, quiet: bool = False) -> str | None:
        """Types `text` + Enter on the board's console; returns why it wasn't sent, or None."""
        words = text.split()
        if not self.transport or self.status != "connected":
            return f"node {self.key.upper()} is not connected"
        if len(text) > MAX_CMD:
            return f"too long: the board takes {MAX_CMD} characters"
        if any(not (" " <= c <= "~") for c in text):
            return "only printable ASCII goes to the board"
        # An empty line answers a question of the `burst` wizard with its default; outside it the board ignores it.
        if quiet:
            self._quiet[text] = time.monotonic() + 2.5
        else:
            self.typed += 1
        try:
            # Ctrl+U first: whatever is half typed on the board's line is not part of this command.
            self.transport.write(b"\x15" + text.encode("ascii") + b"\r")
        except Exception as e:
            return f"not sent: {getattr(e, 'strerror', None) or e}"
        if not quiet and words and words[0] in SETTERS:
            self.request_config(delay=0.8)
        return None

    def request_config(self, delay: float) -> None:
        self._refresh_at = time.monotonic() + delay

    def tick(self, now: float) -> None:
        """Once a second or so, from the server: the pending `config` refresh, and the rate samples."""
        if self._refresh_at and now >= self._refresh_at:
            self._refresh_at = 0.0
            self.send("config", quiet=True)
        if self.gen and self.gen.get("state") == "running" and now - self._gen_poll >= 1.0:
            self._gen_poll = (
                now  # a stream runs: its progress is in `gen status`, asked once a second
            )
            self.send("gen status", quiet=True)
        t0, tx0, rx0 = self._last_sample
        if now - t0 >= 1.0:
            with self.lock:
                tx, rx = self.counters["tx"], self.counters["rx"]
                if tx is not None and rx is not None and tx0 is not None and rx0 is not None:
                    dt = now - t0
                    self.series["rate"].add(
                        [
                            round(time.time(), 2),
                            round(max(tx - tx0, 0) / dt, 2),
                            round(max(rx - rx0, 0) / dt, 2),
                        ]
                    )
                self._last_sample = (now, tx, rx)

    # --- reading
    def _on_bytes(self, data: bytes) -> None:
        with self.lock:
            self.last_rx = time.monotonic()
            self.screen.feed(data)
            for runs in self.screen.take():
                self._on_row(runs)
            live = plain(self.screen.live) if self.screen.live else ""
            if live != self._live_text:
                self._live_text = live
                if live:
                    self._on_frame(live, live=True)

    def _on_row(self, runs: list) -> None:
        text = plain(runs)
        now = time.monotonic()
        quiet = now < self._quiet.get("config", 0.0)
        quiet_gen = now < self._quiet.get("gen status", 0.0)
        if (
            self._sync_active
        ):  # the answers to the peer sync are for the page's table, not for the log
            ok, bad = VALUE_RE.match(text), VALUE_ERR_RE.match(text)
            m = ok or bad
            if m and m[1].startswith("/config/"):
                self._sync_values[m[1]] = (
                    {"value": m[2], "status": "Good", "t": time.time()}
                    if ok
                    else {"value": m[3] or "", "status": m[2], "t": time.time()}
                )
                event = self._sync_events.get(m[1])
                if event:
                    event.set()
                return
            if re.match(r"^›\s*(read|write) /config/", text) or (
                " UA " in text and "/config/" in text
            ):
                return
        if CONFIG_RE.match(text):
            self._on_config(text)
            if quiet:
                return
        elif quiet and re.match(r"^›\s*config\s*$", text):
            return
        if quiet_gen and (
            re.match(r"^›\s*gen status\s*$", text)
            or GEN_STATE_RE.match(text)
            or GEN_STREAM_RE.search(text)
        ):
            self._on_test(text, time.time())
            return
        if self._on_frame(text, live=False):
            if FRAME_RE.match(text) and FRAME_RE.match(text)[4] == "BST":
                return  # frames of a burst are counted, not shown
        else:
            self._on_other(text)
        self._store({"src": "board", "level": classify(text), "runs": runs})

    def _count(self, field: str, n: int = 1) -> None:
        c = self.counters
        c[field] = (c[field] or 0) + n

    def _on_frame(self, text: str, live: bool) -> bool:
        m = FRAME_RE.match(text)
        if not m:
            return False
        sec, ms, way, kind, seq, rest = m.groups()
        board_ms = int(sec) * 1000 + int(ms)
        key = (way, kind, seq, board_ms)
        suffix = LIVE_RE.search(rest)
        if suffix:  # the live line carries the counters
            tx, rx, lost, bad = suffix.groups()
            self.counters.update(tx=int(tx), rx=int(rx), lost=int(lost), bad=int(bad or 0))
            self.counted = False
        if key in self._seen:
            return True  # the live line is drawn again whenever anything is printed
        self._seen.append(key)
        if not suffix and not live:
            if self.counters["tx"] is None:
                self.counters.update(tx=0, rx=0, lost=0, bad=0)
                self.counted = True
            if kind != "BST":
                self._count("tx" if way == "TX" else "rx")
        t = round(time.time(), 3)
        parts = [p.strip() for p in rest.split("│")]
        who = parts[0] if parts else ""
        info = " │ ".join(parts[1:])
        info = LIVE_RE.sub("", " │ " + info).strip(" │")
        frame = {"t": t, "bt": board_ms, "dir": way, "kind": kind, "seq": int(seq), "node": who}
        if kind == "DAT":
            b = re.search(r"btn\s+[●○]\s*(pressed|released)", info)
            e = re.search(r"event\s+(\d+)", info)
            frame["btn"] = b[1] if b else None
            if b:
                if way == "RX":
                    self.peer_btn = b[1]
                else:
                    self.own_btn = b[1]
            frame["events"] = int(e[1]) if e else None
        elif kind == "EVT":
            e = re.search(r"(press|release)", info)
            k = re.search(r"key\s+([0-9A-F]{16})", info)
            btn = re.search(r"\b(jumper|boot)\b", info)
            if e and k:
                self.series["events"].add([t, way, e[1], k[1], who, btn[1] if btn else ""])
        elif kind == "ECH":
            e = re.search(r"echo of\s+#(\d+)", info)
            frame["echo"] = int(e[1]) if e else None
        elif kind == "BAU":
            frame["info"] = info
        d = DELTA_RE.search(info)
        if d and kind == "DAT":
            self.series["delta"].add([t, float(d[1]), d[2], way])
        r = RTT_RE.search(info)
        if r:
            us = float(r[1]) * (1000 if r[2] == "ms" else 1)
            self.series["rtt"].add([t, us])
            frame["rtt_us"] = us
        if way == "RX":
            self.peer_last = t
        self.last_frame = frame
        self.frames.append(frame)
        self.uptime_s = board_ms // 1000
        return True

    def _on_other(self, text: str) -> None:
        t = round(time.time(), 3)
        m = STATUS_RE.match(text)
        if m:
            up, tx, rx, lost, bad, link, speed = m.groups()[:7]
            self.counters.update(tx=int(tx), rx=int(rx), lost=int(lost), bad=int(bad))
            self.counted = False
            was = self.link_up
            self.link_up = link == "UP"
            if was is not None and was != self.link_up:
                self.series["marks"].add([t, "link", f"link {link}"])
            self.speed = speed if speed != "-" else ""
            self.uptime_s = int(up)
            if m[8]:
                self.rtt_stat = [float(m[8]), float(m[9]), float(m[10])]
                self.series["rttstat"].add([t, *self.rtt_stat])
            return
        m = LOOP_RE.match(text)
        if m:
            self.loop = [int(m[1]), int(m[2])]
            return
        m = PEERPERIOD_RE.match(text)
        if m:
            self.peer_period = m[1]
            peak = RXPEAK_RE.search(text)
            self.rx_peak = [int(peak[1]), int(peak[2])] if peak else None
            return
        m = LOST_RE.search(text)
        if m:
            self._count("lost", int(m[1]))
            self.series["marks"].add([t, "lost", f"{m[1]} frame(s) lost"])
            return
        m = RESTART_RE.search(text)
        if m:
            self.series["marks"].add([t, "restart", f"{m[1]} restarted"])
            return
        m = BAD_RE.search(text)
        if m:
            self._count("bad")
            self.series["marks"].add([t, "bad", m[1]])
            return
        m = LINK_EVENT_RE.search(text)
        if m:
            self.series["marks"].add([t, "link", m[1]])
            return
        if text.startswith("╭─ "):  # the boot card: the board (re)started
            self.boots += 1
            self.burst_run = self.gen = None
            self.card = {}
            self.counters.update(tx=None, rx=None, lost=None, bad=None)
            self._last_sample = (time.monotonic(), None, None)
            self._seen.clear()
            self.series["marks"].add([t, "boot", "boot card"])
            self.request_config(delay=2.0)
            return
        m = CARD_RE.match(text)
        if m:
            label = m[1].strip()
            if label and label not in self.card:
                self.card[label] = m[2].strip()
            return
        m = NET_RE.match(text)
        if m:
            self.net[m[1]] = m[2]
            if m[1] == "cable":
                self.link_up = "● up" in m[2]
                sp = re.search(r"(\d+(?:\.\d+)?) (Mbit/s|Gbit/s)", m[2])
                if sp:
                    self.speed = "1G" if sp[2] == "Gbit/s" else f"{int(float(sp[1]))}M"
            return
        if self._on_test(text, t):
            return
        self._on_space(text)

    def _on_config(self, text: str) -> None:
        m = CONFIG_RE.match(text)
        key, value, build = m[1], m[2], m[3]
        if key == "key":
            return
        ro = build.startswith("read-only")
        changed = build.endswith("(changed)")
        build_value = (
            build.replace("(changed)", "").strip() if changed else (value if not ro else "")
        )
        if self.settings.get(key) != [value, build_value, ro, changed]:
            self.settings[key] = [value, build_value, ro, changed]
        if key == "name":
            self.name = value
        elif key == "net.transport":
            self.app = "eth"
        elif key == "shared.baud_rate":
            self.app = "uart"

    def _on_test(self, text: str, t: float) -> bool:
        m = BURST_START_RE.search(text)
        if m:
            self._pending_burst = None
            rate = float(m[3]) * (1000 if m[4] == "kHz" else 1) if m[3] else 0.0
            self.burst_run = {"id": int(m[1]), "count": int(m[2]), "rate_hz": rate, "t": t}
            return True
        m = BURST_SENT_RE.search(text)
        if m:
            self.burst_run = None
            self.series["tests"].add({
                "t": t, "kind": "burst", "dir": "tx", "id": int(m[1]), "frames": int(m[2]), "lost": 0, "bad": 0,
                "bytes": parse_bytes(m[3]), "dur_ms": parse_dur_ms(m[4]), "bps": parse_bps(m[5]),
                "pct": float(m[6]), "who": "", "ok": True,
            })  # fmt: skip
            return True
        m = BURST_RECV_RE.search(text)
        if m:
            self._pending_burst = {
                "t": t, "kind": "burst", "dir": "rx", "id": int(m[2]), "who": m[3], "frames": int(m[4]),
                "of": int(m[5]), "lost": int(m[6]), "bad": int(m[7]), "ok": m[1] == "✓",
            }  # fmt: skip
            return True
        if self._pending_burst:
            m = THROUGHPUT_RE.match(text)
            if m:
                b, self._pending_burst = self._pending_burst, None
                b.update(
                    bytes=parse_bytes(m[1]),
                    dur_ms=parse_dur_ms(m[2]),
                    bps=parse_bps(m[3]),
                    pct=float(m[4]),
                )
                self.series["tests"].add(b)
                return True
            self._pending_burst = None
        m = GEN_TX_RE.search(text)
        if m:
            if self.gen:
                self.gen.update(state="done", stream=None)
            self.series["tests"].add({
                "t": t, "kind": "gen", "dir": "tx", "id": int(m[1]), "frames": int(m[2]), "lost": 0, "bad": 0,
                "bytes": parse_bytes(m[3]), "dur_ms": parse_dur_ms(m[4]), "bps": parse_bitrate(m[5]) / 8,
                "pps": int(m[6]), "pct": float(m[7]), "who": "", "ok": True,
            })  # fmt: skip
            return True
        m = GEN_RX_RE.search(text)
        if m:
            self.series["tests"].add({
                "t": t, "kind": "gen", "dir": "rx", "id": int(m[1]), "frames": int(m[2]), "lost": int(m[3]),
                "loss_pct": float(m[4]), "reordered": int(m[5]), "jitter_us": int(m[6]), "bad": 0,
                "bps": parse_bitrate(m[7]) / 8, "pps": int(m[8]), "pct": float(m[9]),
                "dur_ms": parse_dur_ms(m[10]), "who": "", "ok": int(m[3]) == 0,
            })  # fmt: skip
            return True
        m = GEN_STATE_RE.match(text)
        if m:
            total = parse_dur_ms(m[6]) if m[6] and not m[6].endswith("packets") else None
            self.gen = {
                "state": m[1], "rate": m[2], "size": m[3], "shape": m[4], "total_ms": total or None,
                "forever": m[5] == "runs until stopped", "limit": m[6] if m[6] and m[6].endswith("packets") else "",
                "stream": (self.gen or {}).get("stream") if m[1] in ("running", "paused") else None, "t": t,
            }  # fmt: skip
            return True
        m = GEN_STREAM_RE.search(text)
        if m and self.gen:
            self.gen["stream"] = {
                "id": int(m[1]), "sent": int(m[2]), "bytes": parse_bytes(m[3]), "dur_ms": parse_dur_ms(m[4]),
            }  # fmt: skip
            self.gen["t"] = t
            return True
        return False

    def _on_space(self, text: str) -> None:
        sp = self.space
        m = TREE_HEAD_RE.match(text)
        if m:
            sp["tree"], self._tree_base, self._tree_stack = [], m[1], []
            sp["v"] += 1
            return
        m = TREE_ROW_RE.match(text)
        if m and (sp["tree"] or self._tree_base):
            level = len(m[1]) // 4
            del self._tree_stack[level:]
            self._tree_stack.append(m[2])
            path = self._tree_base.rstrip("/") + "/" + "/".join(self._tree_stack)
            sp["tree"].append(
                {
                    "path": path,
                    "name": m[2],
                    "level": level,
                    "obj": m[3] == "/",
                    "type": m[4] or "",
                    "rw": m[5] == "rw",
                }
            )
            sp["v"] += 1
            return
        m = VALUE_RE.match(text)
        if m:
            sp["values"][m[1]] = {"value": m[2], "status": "Good", "t": round(time.time(), 3)}
            sp["v"] += 1
            return
        m = VALUE_ERR_RE.match(text)
        if m:
            sp["values"][m[1]] = {"value": m[3] or "", "status": m[2], "t": round(time.time(), 3)}
            sp["v"] += 1

    # --- the peer's settings, over the link
    def peer_action(self, action: str, keys: list[str] | None = None) -> str | None:
        """`read`: the peer's value of every setting. `pull`: set the given keys here to the peer's values (read
        before). `push`: write this node's values of the given keys to the peer. Runs in a thread (one command
        at a time, each waits for its answer); the page follows `peer` in the snapshot. Returns why it didn't
        start, or None."""
        with self.lock:
            if action not in ("read", "pull", "push"):
                return "unknown action"
            if not self.transport or self.status != "connected":
                return f"node {self.key.upper()} is not connected"
            if self._sync_active:
                return "a peer sync is already running"
            known = [k for k, v in self.settings.items() if not v[2]]
            if not known:
                return "the settings are not known yet: press Refresh"
            if action == "read":
                keys = known
            else:
                keys = [k for k in dict.fromkeys(keys or []) if k in known]
                have = {r["key"]: r for r in self.peer["rows"] if r["peer"] is not None}
                if not keys:
                    return "no setting selected"
                if action == "pull" and any(k not in have for k in keys):
                    return "read the peer's config first"
            self._sync_active = True
            self.peer.update(state="running", op=action, done=0, total=len(keys), error="")
        threading.Thread(target=self._peer_run, args=(action, keys), daemon=True).start()
        return None

    def _ask(self, command: str, path: str, wait: float = 2.0) -> dict | None:
        """Sends `command` and returns the answer about `path` (`{"value", "status"}`), None if none came."""
        event = threading.Event()
        started = time.time()
        with self.lock:
            self._sync_events[path] = event
        if self.send(command):
            return None
        event.wait(wait)
        with self.lock:
            answer = self._sync_values.get(path)
            return answer if answer and answer["t"] >= started else None

    def _peer_run(self, action: str, keys: list[str]) -> None:
        rows = {r["key"]: r for r in self.peer["rows"]}
        misses = 0
        try:
            for i, key in enumerate(keys):
                if self.stop_flag.is_set():
                    return
                path = f"/config/{key}"
                row = rows.setdefault(key, {"key": key, "peer": None, "status": "", "note": ""})
                if action == "read":
                    answer = self._ask(f"read {path}", path)
                    row["peer"], row["status"], row["note"] = None, "", ""
                    if answer and answer["status"] == "Good":
                        row["peer"], row["status"] = answer["value"], "ok"
                    else:
                        row["status"] = answer["status"] if answer else "no answer"
                elif action == "push":
                    value = (self.settings.get(key) or [""])[0]
                    if len(value) > 24:  # the address space carries 24 characters at most
                        row["status"], row["note"] = "skipped", "longer than 24 characters"
                        answer = {}
                    else:
                        answer = self._ask(f"write {path} {value}", path)
                        if answer and answer["status"] == "Good":
                            row["peer"], row["status"], row["note"] = answer["value"], "pushed", ""
                        else:
                            row["status"] = answer["status"] if answer else "no answer"
                            row["note"] = (
                                f"still {answer['value']}" if answer and answer["value"] else ""
                            )
                else:  # pull: the same `set` as typed in the console
                    value = row["peer"]
                    self.send(f"set {key} {value}")
                    row["status"], row["note"] = "sent", ""
                    answer = {}
                    time.sleep(0.25)
                misses = misses + 1 if answer is None else 0
                with self.lock:
                    self.peer["done"] = i + 1
                    self.peer["rows"] = [rows[k] for k in self._order(rows)]
                if misses >= 3:
                    self.peer["error"] = (
                        "the peer doesn't answer: link down, or a firmware without the address space"
                    )
                    break
            if action == "pull":  # what the board accepted is in its `config` again
                self.send("config", quiet=True)
                time.sleep(1.2)
                with self.lock:
                    for key in keys:
                        row = rows[key]
                        if row["status"] == "sent":
                            now = (self.settings.get(key) or [None])[0]
                            ok = now is not None and now.strip() == str(row["peer"]).strip()
                            row["status"], row["note"] = (
                                ("pulled", "") if ok else ("refused", f"still {now}")
                            )
                    self.peer["rows"] = [rows[k] for k in self._order(rows)]
        finally:
            with self.lock:
                self.peer.update(state="idle", n=self.peer["n"] + 1)
                self._sync_active = False
                self._sync_events.clear()
            self.request_config(0.3)
            if self.other and action == "push":
                self.other.request_config(0.6)  # the peer's own console now shows what it was given
            done = [r for r in self.peer["rows"] if r["status"] in ("ok", "pushed", "pulled")]
            self.host_log(
                "warn" if self.peer["error"] else "ok",
                self.peer["error"] or f"peer config {action}: {len(done)} of {len(keys)} settings",
            )

    def _order(self, rows: dict) -> list[str]:
        """The rows in the order of `config`."""
        order = [k for k, v in self.settings.items() if not v[2] and k in rows]
        return order + [k for k in rows if k not in order]

    # --- for the page
    def snapshot(self, cursors: dict) -> dict:
        with self.lock:
            now = time.monotonic()
            silent = now - self.last_rx if self.last_rx else None
            series = {}
            for name, s in self.series.items():
                items, n = s.since(cursors.get(("s", self.key, name), 0))
                cursors[("s", self.key, name)] = n
                series[name] = {"n": n, "items": items}
            verbose = (self.settings.get("verbose") or [None])[0]
            return {
                "key": self.key,
                "status": self.status,
                "detail": self.detail,
                "port": self.port,
                "name": self.name,
                "app": self.app,
                "verbose": verbose,
                "silent_s": silent,
                "counters": dict(self.counters),
                "counted": self.counted,
                "link_up": self.link_up,
                "speed": self.speed,
                "rtt_stat": self.rtt_stat,
                "uptime_s": self.uptime_s,
                "loop": self.loop,
                "peer_period": self.peer_period,
                "rx_peak": self.rx_peak,
                "last_frame": self.last_frame,
                "peer_last": self.peer_last,
                "burst_run": self.burst_run,
                "peer_btn": self.peer_btn,
                "own_btn": self.own_btn,
                "card": self.card,
                "net": self.net,
                "boots": self.boots,
                "gen": self.gen,
                "settings": self.settings,
                "space": self.space,
                "peer": self.peer,
                "live": self.screen.live,
                "dash": self.screen.dash,
                "prompt": self.screen.prompt,
                "typed": self.typed,
                "series": series,
            }

    def logs_since(self, cursor: int) -> tuple[list[dict], int]:
        with self.lock:
            return [e for e in self.logs if e["i"] > cursor], self.log_n

    def export(self, kind: str) -> tuple[str, str]:
        """(file name, text) of the frames as CSV, or the whole log as plain text."""
        with self.lock:
            if kind == "frames":
                rows = ["host_time,board_ms,dir,kind,seq,node,button,events,rtt_us"]
                for f in self.frames:
                    rows.append(",".join(str(x) for x in (
                        f["t"], f["bt"], f["dir"], f["kind"], f["seq"], f["node"], f.get("btn") or "",
                        "" if f.get("events") is None else f["events"], f.get("rtt_us", ""),
                    )))  # fmt: skip
                return f"node-{self.key}-frames.csv", "\n".join(rows) + "\n"
            lines = []
            for e in self.logs:
                stamp = time.strftime("%H:%M:%S", time.localtime(e["t"]))
                lines.append(f"{stamp} {e['src']:5} {plain(e['runs'])}")
            return f"node-{self.key}-log.txt", "\n".join(lines) + "\n"


# --------------------------------------------------------------------------------------------
# The web side: one page, one event stream, one command endpoint
# --------------------------------------------------------------------------------------------


def plotly_js() -> Path | None:
    """plotly.min.js from the `plotly` Python package, if installed (so the page works offline)."""
    spec = importlib.util.find_spec("plotly")
    if spec and spec.submodule_search_locations:
        path = Path(next(iter(spec.submodule_search_locations))) / "package_data" / "plotly.min.js"
        return path if path.is_file() else None
    return None


class Server:
    def __init__(self, boards: dict[str, Board], host: str, port: int, fps: int) -> None:
        self.boards = boards
        for k, b in boards.items():
            b.other = boards.get("b" if k == "a" else "a")
        self.fps = fps
        self.stop_flag = threading.Event()
        self.httpd = http.server.ThreadingHTTPServer((host, port), self._handler())
        self.httpd.daemon_threads = True
        self.port = self.httpd.server_address[1]
        self._threads = [
            threading.Thread(target=self.httpd.serve_forever, daemon=True),
            threading.Thread(target=self._ticker, daemon=True),
        ]

    def start(self) -> None:
        for b in self.boards.values():
            b.start()
        for t in self._threads:
            t.start()

    def close(self) -> None:
        self.stop_flag.set()
        self.httpd.shutdown()
        self.httpd.server_close()
        for b in self.boards.values():
            b.close()

    def _ticker(self) -> None:
        while not self.stop_flag.is_set():
            now = time.monotonic()
            for b in self.boards.values():
                b.tick(now)
            time.sleep(0.2)

    def command(self, targets: str, text: str, quiet: bool = False) -> tuple[int, dict]:
        """Sends `text` to node a, b or both (`targets` = "a", "b" or "ab")."""
        keys = [k for k in NODES if k in targets]
        if not keys or any(k not in NODES for k in targets):
            return 400, {"ok": False, "error": "unknown board"}
        text = text.strip()
        if quiet and text != "config":
            return 400, {"ok": False, "error": "only `config` may be asked quietly"}
        errors = {}
        for k in keys:
            error = self.boards[k].send(text, quiet=quiet)
            if error:
                errors[k] = error
                self.boards[k].host_log("warn", error)
        return (200 if not errors else 400), {"ok": not errors, "errors": errors}

    def _handler(self) -> type[http.server.BaseHTTPRequestHandler]:
        server = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args) -> None:  # quiet
                pass

            def _send(self, code: int, body: bytes, ctype: str, extra: dict | None = None) -> None:
                self.send_response(code)
                self.send_header("Content-Type", ctype)
                self.send_header("Content-Length", str(len(body)))
                for k, v in (extra or {}).items():
                    self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self) -> None:
                url = urllib.parse.urlparse(self.path)
                if url.path in ("/", "/index.html"):
                    self._send(
                        200,
                        PAGE.read_bytes(),
                        "text/html; charset=utf-8",
                        {"Cache-Control": "no-store"},
                    )
                elif url.path == "/plotly.min.js":
                    js = plotly_js()
                    if js:
                        self._send(
                            200,
                            js.read_bytes(),
                            "text/javascript",
                            {"Cache-Control": "max-age=86400"},
                        )
                    else:  # `pip install plotly` for offline use
                        self._send(302, b"", "text/plain", {"Location": CDN_PLOTLY})
                elif url.path == "/events":
                    self._events()
                elif url.path == "/api/export":
                    q = urllib.parse.parse_qs(url.query)
                    key, kind = q.get("board", ["a"])[0], q.get("kind", ["frames"])[0]
                    if key not in server.boards or kind not in ("frames", "log"):
                        self._send(404, b"not found", "text/plain")
                        return
                    name, text = server.boards[key].export(kind)
                    ctype = "text/csv" if kind == "frames" else "text/plain"
                    self._send(200, text.encode(), ctype + "; charset=utf-8",
                               {"Content-Disposition": f"attachment; filename={name}"})  # fmt: skip
                else:
                    self._send(404, b"not found", "text/plain")

            def do_POST(self) -> None:
                # application/json from a foreign page would need a CORS preflight, which we never grant.
                ctype = self.headers.get("Content-Type", "").split(";")[0]
                if self.path not in ("/api/cmd", "/api/peer") or ctype != "application/json":
                    self._send(400, b"bad request", "text/plain")
                    return
                try:
                    length = min(int(self.headers.get("Content-Length", "0")), 4096)
                    body = json.loads(self.rfile.read(length) or b"{}")
                    if not isinstance(body, dict):
                        raise ValueError
                except ValueError:
                    self._send(400, b"bad json", "text/plain")
                    return
                if self.path == "/api/peer":
                    board = server.boards.get(str(body.get("board", "")))
                    keys = body.get("keys")
                    if board is None or not (keys is None or isinstance(keys, list)):
                        self._send(400, b'{"ok":false,"error":"bad request"}', "application/json")
                        return
                    error = board.peer_action(
                        str(body.get("action", "")), [str(k) for k in keys or []]
                    )
                    if error:
                        board.host_log("warn", error)
                    code = 200 if error is None else 400
                    self._send(
                        code,
                        json.dumps({"ok": error is None, "error": error}).encode(),
                        "application/json",
                    )
                    return
                code, reply = server.command(
                    str(body.get("board", "")), str(body.get("text", "")), body.get("quiet") is True
                )
                self._send(code, json.dumps(reply).encode(), "application/json")

            def _events(self) -> None:
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-store")
                self.end_headers()
                cursors: dict = {}
                log_cursors = {k: 0 for k in server.boards}
                period = 1.0 / server.fps
                try:
                    while not server.stop_flag.is_set():
                        for k, b in server.boards.items():
                            entries, log_cursors[k] = b.logs_since(log_cursors[k])
                            if entries:
                                self._sse(
                                    b"log",
                                    json.dumps(
                                        {"board": k, "rows": entries}, separators=(",", ":")
                                    ).encode(),
                                )
                        frame = {
                            "t": time.time(),
                            "boards": {k: b.snapshot(cursors) for k, b in server.boards.items()},
                        }
                        self._sse(None, json.dumps(frame, separators=(",", ":")).encode())
                        self.wfile.flush()
                        time.sleep(period)
                except OSError:
                    pass  # the page went away

            def _sse(self, event: bytes | None, data: bytes) -> None:
                self.wfile.write(
                    (b"event: " + event + b"\n" if event else b"") + b"data: " + data + b"\n\n"
                )

        return Handler


# --------------------------------------------------------------------------------------------
# Which boards
# --------------------------------------------------------------------------------------------


def serial_boards(args) -> dict[str, Board]:
    """Node A and node B: `--a` / `--b` ports, else the boards assigned with `dev.py boards assign`."""
    sys.path.insert(0, str(ROOT / "scripts"))
    boards: dict[str, Board] = {}
    plugged = {}
    try:
        import boards as board_lib

        plugged = {b.mac: b for b in board_lib.detect()}
        assigned = board_lib.load_assignments()
    except ImportError:  # no pyserial: the ports must be given
        assigned = {}
    for key in NODES:
        port = getattr(args, key)
        note = ""
        if not port:
            mac = assigned.get(key)
            if mac and mac in plugged:
                port = plugged[mac].port
            elif mac:
                note = f"node {key.upper()}'s board ({mac}) is not plugged in"
            else:
                note = f"no board assigned: dev.py boards assign {key} <mac> (or --{key} PORT)"
        factory = (lambda p=port: SerialTransport(p)) if port else None
        boards[key] = Board(key, factory, port, note, app=args.app)
    return boards


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "--a", metavar="PORT", help="serial port of node A (default: the board assigned to a)"
    )
    ap.add_argument(
        "--b", metavar="PORT", help="serial port of node B (default: the board assigned to b)"
    )
    ap.add_argument(
        "--fps",
        type=int,
        default=5,
        help="updates per second sent to the page (default: %(default)s)",
    )
    ap.add_argument(
        "--http-port",
        type=int,
        default=8060,
        help="where the page is served (default: %(default)s)",
    )
    ap.add_argument(
        "--host", default="127.0.0.1", help="interface of the page; 0.0.0.0 shows it to the network"
    )
    ap.add_argument("--no-browser", action="store_true", help="don't open the page")
    ap.add_argument(
        "--demo",
        nargs="?",
        const="auto",
        choices=("auto", "eth", "uart"),
        help="two simulated nodes, no hardware (the app: eth by default, or --app)",
    )
    ap.add_argument(
        "--app",
        choices=("eth", "uart"),
        help="the app the boards run: the page shows its controls from the start (it also reads it from `config`)",
    )
    ap.add_argument(
        "--selftest",
        action="store_true",
        help="test the whole chain on simulated nodes, no browser",
    )
    args = ap.parse_args()

    if args.selftest:
        import node_selftest

        return node_selftest.run()

    world = None
    if args.demo:
        import node_sim

        app = args.demo if args.demo != "auto" else args.app or "eth"
        world = node_sim.World(app)
        boards = {
            k: Board(k, world.factory(k), f"simulated node {k.upper()}", app=app) for k in NODES
        }
    else:
        boards = serial_boards(args)
    server = Server(boards, args.host, args.http_port, args.fps)
    server.start()
    url = f"http://{'127.0.0.1' if args.host == '0.0.0.0' else args.host}:{server.port}"
    print(f"Node dashboard at {url}   (Ctrl-C to stop)")
    for k, b in boards.items():
        print(f"  node {k.upper()}: {b.port or b.note}")
    if not args.no_browser:
        webbrowser.open(url)
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        pass
    finally:
        server.close()
        if world:
            world.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
