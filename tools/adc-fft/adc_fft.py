#!/usr/bin/env python3
"""Live spectrum (FFT) of the ADC samples streamed by the `adc-stream` firmware, in your browser.

    python3 scripts/dev.py setup                       # once: numpy and plotly go in .venv
    python3 scripts/dev.py --app adc flash -m          # flash the board (firmware/adc-stream/)
    python3 scripts/dev.py fft                         # then, on the computer (or this file with .venv/bin/python)

The board is at 192.168.77.20 (UDP port 7100): give your computer's Ethernet port an address in
192.168.77.0/24 (e.g. 192.168.77.1, netmask 255.255.255.0) and cable it to the board's RJ45.
The page opens at http://127.0.0.1:8050 (Plotly.js, served from the plotly package: no internet
needed). No board at hand: `--demo` runs a simulated one on this computer, `--selftest` checks the
whole chain without a browser.

How it fits together:

    board ──UDP──▶ Stream (thread: datagrams → last samples)
                      │
                   Analyzer (windowed FFT, averaging, peak hold)  ◀── POST /api/config (the page's controls)
                      │ one frame every 1/fps, JSON
                   Hub ──▶ GET /events (Server-Sent Events) ──▶ the page draws it with Plotly

The board's protocol is documented in firmware/adc-stream/src/stream.rs. In short: this script
sends `start rate=20000 n=256` to the board every second (the keep-alive), and the board streams
datagrams with 256 raw samples each back to it.
"""

from __future__ import annotations

import argparse
import collections
import http.client
import http.server
import importlib.util
import json
import math
import queue
import socket
import struct
import sys
import threading
import time
import urllib.parse
import webbrowser
from pathlib import Path

import numpy as np

HEADER = struct.Struct("<4sIIIHH")  # magic, seq, rate_mhz, dropped, count, pin
MAGIC = b"ADC1"
LOG = struct.Struct("<4sIIB")  # "LOG1", seq, the board's uptime in ms, level; then the text
LOG_MAGIC = b"LOG1"
LEVELS = (
    "info",
    "ok",
    "warn",
    "error",
    "stat",
    "reply",
    "debug",
)  # what the board's level numbers mean
REPLY_MAGIC = b"RPL1"  # the answer to a command: the layout of LOG1, never replayed, seq 0
# Commands the page may send to the board (`start` and `stop` are this tool's own business).
BOARD_COMMANDS = ("info", "verbose", "reboot", "help")
MAX_SCOPE = 16384  # samples of the waveform at most
# The S31's SAR is differential: 0 V reads about ZERO, the full scale is FULL (esp-hal's constants).
ZERO_CODE = 2198
FULL_SCALE = 4393
HALF_SCALE = FULL_SCALE - ZERO_CODE  # amplitude of a full-scale sine, in codes
KEEPALIVE_S = 1.0
# The ADC's continuous mode (DMA) on the ESP32-S31: 611 Hz to 83 333 Hz (SOC_ADC_SAMPLE_FREQ_THRES_*).
MIN_RATE_HZ, MAX_RATE_HZ = 611, 83_333
HISTORY = 65536  # samples kept: twice the largest FFT
PAGE = Path(__file__).with_name("adc_fft.html")
CDN_PLOTLY = "https://cdnjs.cloudflare.com/ajax/libs/plotly.js/2.35.2/plotly.min.js"


class Stream:
    """Receives the datagrams on a thread and keeps the last `HISTORY` samples (codes, as floats).

    A gap (a lost datagram, or samples the board dropped) empties the contiguous window: a spectrum
    never spans a discontinuity. `filled` is how many samples are contiguous since the last gap.
    """

    def __init__(self, board: tuple[str, int], rate_hz: int, count: int, history: int = HISTORY):
        self.board = board
        self.count = count
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("", 0))
        self.sock.settimeout(0.2)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1 << 20)
        self.data = np.zeros(history, dtype=np.float64)
        self.filled = 0
        self.rate_mhz = (
            rate_hz * 1000
        )  # what the datagrams said last (exact, from the board's timer)
        self.requested_hz = rate_hz  # what we ask for
        self.lock = threading.Lock()
        self.stop_flag = threading.Event()
        self.packets = 0
        self.lost_packets = 0
        self.dropped_by_board = 0  # samples the board's ring buffer lost
        self.gaps = 0
        self.board_info = ""
        self.board_verbose: bool | None = (
            None  # what the board says (in its `ok`), None until known
        )
        self.want_verbose: bool | None = None  # what the user asked: re-applied after a reboot
        self.last_rx = 0.0  # time.monotonic() of the last datagram
        self._arrivals: list[float] = []
        self.logs: collections.deque = collections.deque(maxlen=1000)
        self._log_count = 0  # lines ever stored: the cursor of `logs_since`
        self._board_log_seq = -1
        self._board_log_ms = 0
        self._next_seq: int | None = None
        self._last_dropped = 0
        self._thread = threading.Thread(target=self._run, daemon=True)

    def start(self) -> None:
        self.host_log("info", f"looking for the board at {self.board[0]}:{self.board[1]}")
        self._thread.start()

    # --- the log: the board's lines (LOG1 datagrams) and this program's own, for the page's terminal
    def host_log(self, level: str, text: str) -> None:
        with self.lock:
            self._store_log({"src": "host", "t": time.time(), "level": level, "text": text})

    def _store_log(self, entry: dict) -> None:
        self._log_count += 1
        entry["i"] = self._log_count
        self.logs.append(entry)

    def logs_since(self, cursor: int) -> tuple[list[dict], int]:
        """The lines stored after `cursor`, and the new cursor."""
        with self.lock:
            return [e for e in self.logs if e["i"] > cursor], self._log_count

    def log_history(self, n: int = 400) -> list[dict]:
        with self.lock:
            return list(self.logs)[-n:]

    def send_command(self, text: str) -> str | None:
        """Sends a typed command to the board; returns why it wasn't sent, or None."""
        words = text.split()
        if not words or words[0] not in BOARD_COMMANDS or len(text) > 64:
            return f"unknown command: try {', '.join(BOARD_COMMANDS)}"
        if words[0] == "verbose":
            arg = words[1] if len(words) > 1 else None
            if arg in ("on", "1"):
                self.want_verbose = True
            elif arg in ("off", "0"):
                self.want_verbose = False
            elif arg is None:
                self.want_verbose = not (
                    self.board_verbose if self.want_verbose is None else self.want_verbose
                )
                text = "verbose on" if self.want_verbose else "verbose off"
            else:
                return "verbose [on|off]"
        self.host_log("cmd", text)
        try:
            self.sock.sendto(text.encode(), self.board)
        except OSError as e:
            return f"not sent: {e.strerror or e}"
        return None

    def _on_log(self, datagram: bytes) -> None:
        magic, seq, ms, level = LOG.unpack_from(datagram)
        text = datagram[LOG.size :].decode(errors="replace")
        lvl = LEVELS[level] if level < len(LEVELS) else "info"
        if magic == REPLY_MAGIC:
            with self.lock:
                self._store_log({"src": "board", "t": ms, "level": lvl, "text": text})
            return
        with self.lock:
            if ms + 1000 < self._board_log_ms:  # the uptime went back: the board restarted
                self._board_log_seq = -1
            if seq <= self._board_log_seq:  # the history is replayed to each new reader
                return
            self._board_log_seq, self._board_log_ms = seq, ms
            self._store_log({"src": "board", "t": ms, "level": lvl, "text": text})

    def close(self) -> None:
        self.stop_flag.set()
        self._thread.join(timeout=1)
        try:
            self.sock.sendto(b"stop", self.board)
        except OSError:
            pass
        self.sock.close()

    @property
    def fs(self) -> float:
        return self.rate_mhz / 1000

    def set_rate(self, rate_hz: int) -> None:
        """Ask the board for another sample rate (it restarts the stream at once)."""
        self.requested_hz = min(max(int(rate_hz), MIN_RATE_HZ), MAX_RATE_HZ)
        self.host_log("info", f"asked the board for {self.requested_hz} Hz")
        self._send_request()

    def age(self) -> float:
        """Seconds since the last datagram (large before the first one)."""
        return time.monotonic() - self.last_rx if self.last_rx else 1e9

    def pps(self) -> float:
        """Datagrams per second over the last second."""
        now = time.monotonic()
        with self.lock:
            self._arrivals = [t for t in self._arrivals if now - t < 1.0]
            return float(len(self._arrivals))

    def snapshot(self, n: int) -> np.ndarray | None:
        """The last `n` contiguous samples, or None while there aren't that many yet."""
        with self.lock:
            return self.data[-n:].copy() if self.filled >= n else None

    def _send_request(self) -> None:
        try:
            self.sock.sendto(f"start rate={self.requested_hz} n={self.count}".encode(), self.board)
        except OSError:
            pass  # network down for now: retry at the next keep-alive

    def _run(self) -> None:
        last_keepalive = 0.0
        while not self.stop_flag.is_set():
            now = time.monotonic()
            if now - last_keepalive >= KEEPALIVE_S:
                last_keepalive = now
                self._send_request()
            try:
                datagram = self.sock.recv(2048)
            except TimeoutError:
                continue
            except OSError:
                break
            if datagram.startswith(b"ok"):
                self.board_info = datagram.decode(errors="replace").strip()
                self._on_ok(self.board_info)
            elif datagram.startswith(MAGIC) and len(datagram) >= HEADER.size:
                self._on_samples(datagram)
            elif datagram.startswith((LOG_MAGIC, REPLY_MAGIC)) and len(datagram) >= LOG.size:
                self._on_log(datagram)

    def _on_ok(self, info: str) -> None:
        """The board's answer to `start` says whether verbose is on: after a reboot it is off again."""
        for word in info.split():
            if word.startswith("verbose="):
                self.board_verbose = word.endswith("1")
        if self.want_verbose is not None and self.board_verbose != self.want_verbose:
            try:
                self.sock.sendto(b"verbose on" if self.want_verbose else b"verbose off", self.board)
            except OSError:
                pass

    def _on_samples(self, datagram: bytes) -> None:
        _, seq, rate_mhz, dropped, count, _pin = HEADER.unpack_from(datagram)
        if len(datagram) < HEADER.size + 2 * count:
            return
        samples = np.frombuffer(datagram, dtype="<u2", count=count, offset=HEADER.size).astype(
            np.float64
        )
        now = time.monotonic()
        with self.lock:
            gap = False
            if self._next_seq is not None and seq != self._next_seq:
                # seq going backwards is a restarted stream (the board saw a new `start`).
                if seq > self._next_seq:
                    self.lost_packets += seq - self._next_seq
                gap = True
            if seq == 0 or rate_mhz != self.rate_mhz:
                gap = True
            if dropped != self._last_dropped:
                self.dropped_by_board = dropped
                gap = gap or dropped > self._last_dropped
            self._last_dropped = dropped
            self._next_seq = (seq + 1) & 0xFFFFFFFF
            self.rate_mhz = rate_mhz
            if gap:
                self.filled = 0
                self.gaps += 1
            k = min(count, len(self.data))
            self.data[:-k] = self.data[k:]
            self.data[-k:] = samples[-k:]
            self.filled = min(self.filled + count, len(self.data))
            self.packets += 1
            self.last_rx = now
            self._arrivals.append(now)


# --------------------------------------------------------------------------------------------
# The FFT
# --------------------------------------------------------------------------------------------


def _flattop(n: int) -> np.ndarray:
    """Flat-top window (5-term cosine sum): accurate amplitudes for lines between two bins."""
    k = np.arange(n) * 2 * np.pi / max(n - 1, 1)
    a = (0.21557895, 0.41663158, 0.277263158, 0.083578947, 0.006947368)
    return sum(c * np.cos(i * k) * (-1) ** i for i, c in enumerate(a))


WINDOWS = {
    "hann": np.hanning,
    "hamming": np.hamming,
    "blackman": np.blackman,
    "flattop": _flattop,
    "rect": np.ones,
}
_window_cache: dict[tuple[str, int], np.ndarray] = {}


def window(name: str, n: int) -> np.ndarray:
    if (name, n) not in _window_cache:
        _window_cache[(name, n)] = WINDOWS[name](n)
    return _window_cache[(name, n)]


def spectrum(x: np.ndarray, fs: float, win: str = "hann") -> tuple[np.ndarray, np.ndarray]:
    """Frequencies (Hz) and amplitudes (dB relative to a full-scale sine) of `x`.

    The mean is removed first: the ADC reads ~2198 for 0 V, and the DC term would bury the rest.
    Scaled so that a sine of amplitude A codes reads 20 log10(A / HALF_SCALE), whatever the window.
    """
    x = x - x.mean()
    w = window(win, len(x))
    amplitude = np.abs(np.fft.rfft(x * w)) * 2.0 / w.sum()
    return np.fft.rfftfreq(len(x), 1.0 / fs), 20.0 * np.log10(
        np.maximum(amplitude, 1e-9) / HALF_SCALE
    )


def peak(freq: np.ndarray, db: np.ndarray, min_hz: float = 0.0) -> tuple[float, float]:
    """The strongest line above `min_hz`: (frequency, dB), refined by a parabola through the 3 bins."""
    lo = max(2, int(np.searchsorted(freq, min_hz)))
    i = lo + int(np.argmax(db[lo:-1]))
    a, b, c = db[i - 1], db[i], db[i + 1]
    denom = a - 2 * b + c
    delta = 0.5 * (a - c) / denom if denom else 0.0
    return float(freq[i] + delta * (freq[1] - freq[0])), float(b - 0.25 * (a - c) * delta)


def decimate_max(a: np.ndarray, target: int = 4100) -> tuple[np.ndarray, int]:
    """At most `target` points for the screen, keeping the maximum of each group (lines stay visible)."""
    g = math.ceil(len(a) / target)
    if g == 1:
        return a, 1
    return a[: len(a) // g * g].reshape(-1, g).max(axis=1), g


def find_trigger(seg: np.ndarray, length: int) -> int:
    """Start of a steady view of `seg`: a rising edge through the mean, with a little hysteresis (noise
    near the mean must not trigger). 0 when there is none, or when `length` samples wouldn't follow.
    """
    mean, sd = float(seg.mean()), float(seg.std())
    if sd == 0 or len(seg) < 2 * length:
        return 0
    last = len(seg) - length
    low = np.flatnonzero(seg[: last + 1] < mean - 0.25 * sd)
    if low.size == 0:
        return 0
    high = np.flatnonzero(seg[low[0] : last + 1] > mean + 0.25 * sd)
    return int(low[0] + high[0]) if high.size else 0


def find_lines(freq: np.ndarray, db: np.ndarray, f0: float, limit: int = 8) -> list[dict]:
    """The strongest lines of the spectrum, at full resolution: local maxima 15 dB above the median,
    at least 6 bins apart, each refined by a parabola. `h` is the harmonic number of the line
    relative to the strongest one, when it is close to a whole multiple of it."""
    if len(db) < 8:
        return []
    inner = db[1:-1]
    cand = np.flatnonzero((inner > np.median(db) + 15) & (inner > db[:-2]) & (inner >= db[2:])) + 1
    cand = cand[freq[cand] >= 1.0]
    chosen: list[int] = []
    for i in cand[np.argsort(db[cand])[::-1]]:
        if all(abs(int(i) - j) > 6 for j in chosen):
            chosen.append(int(i))
        if len(chosen) == limit:
            break
    df = float(freq[1] - freq[0])
    lines = []
    for i in chosen:
        a, b, c = db[i - 1], db[i], db[i + 1]
        denom = a - 2 * b + c
        delta = 0.5 * (a - c) / denom if denom else 0.0
        f, level = float(freq[i] + delta * df), float(b - 0.25 * (a - c) * delta)
        k = round(f / f0) if f0 > 0 else 0
        harmonic = k if k >= 1 and abs(f - k * f0) <= max(2 * df, 0.01 * k * f0) else None
        lines.append({"f": round(f, 2), "db": round(level, 1), "h": harmonic})
    return lines


def measure(
    xs: np.ndarray, freq: np.ndarray, db: np.ndarray, fs: float, pf: float, pdb: float
) -> dict:
    """Numbers about the window: AC rms, mean (the DC level), peak-to-peak, the noise floor (median of the
    spectrum) and, when one tone stands out, estimates of its SNR, SINAD and THD (harmonics up to the 10th
    that rise above the noise). The estimates assume a single tone: they are only that."""
    var = float(xs.var())
    floor_db = float(np.median(db))
    out: dict = {
        "rms": var**0.5,
        "dc": float(xs.mean()),
        "pp": float(xs.max() - xs.min()),
        "floor_db": floor_db,
        "snr": None,
        "sinad": None,
        "thd_db": None,
    }
    if pdb < floor_db + 15 or pf <= 0:
        return out
    tone = HALF_SCALE * 10 ** (pdb / 20)  # amplitude of the main tone, in codes
    harm_sq = 0.0
    df = float(freq[1] - freq[0])
    for k in range(2, 11):
        i = round(k * pf / df)
        if i + 2 >= len(db):
            break
        level = float(
            db[max(i - 1, 0) : i + 2].max()
        )  # a little tolerance for the window's scalloping
        if level > floor_db + 10:
            harm_sq += (HALF_SCALE * 10 ** (level / 20)) ** 2
    tone_p = tone**2 / 2
    if harm_sq > 0:
        out["thd_db"] = 20 * np.log10(max(harm_sq**0.5 / tone, 1e-9))
    noise_p = var - tone_p - harm_sq / 2
    if noise_p > 0:
        out["snr"] = 10 * np.log10(tone_p / noise_p)
    if var - tone_p > 0:
        out["sinad"] = 10 * np.log10(tone_p / (var - tone_p))
    return out


class Analyzer:
    """Turns the stream into display frames, under the settings the page sends."""

    FFT_SIZES = (1024, 2048, 4096, 8192, 16384, 32768)

    def __init__(
        self, stream: Stream, fft: int = 4096, win: str = "hann", avg: int = 1, scope_ms: int = 20
    ):
        self.stream = stream
        self.lock = threading.Lock()
        self.s = {
            "fft": fft,
            "window": win,
            "avg": avg,
            "hold": False,
            "paused": False,
            "rate": stream.requested_hz,
            "scope_ms": scope_ms,  # the waveform's time span
            "trigger": False,  # steady waveform: start at a rising edge
        }
        self._avg_power: np.ndarray | None = None
        self._hold_db: np.ndarray | None = None
        self._key: tuple | None = None
        self._frozen: dict = {}
        self.last_samples: np.ndarray | None = None
        self.last_spectrum: tuple[np.ndarray, np.ndarray] | None = None

    def configure(self, patch: dict) -> dict:
        """Applies what is valid in `patch` (the rest is ignored) and returns the settings."""
        with self.lock:
            s = self.s
            if patch.get("fft") in self.FFT_SIZES:
                s["fft"] = patch["fft"]
            if patch.get("window") in WINDOWS:
                s["window"] = patch["window"]
            if isinstance(patch.get("avg"), int) and 1 <= patch["avg"] <= 64:
                s["avg"] = patch["avg"]
            ms = patch.get("scope_ms")
            if isinstance(ms, int) and not isinstance(ms, bool) and 1 <= ms <= 1000:
                s["scope_ms"] = ms
            for flag in ("hold", "paused", "trigger"):
                if isinstance(patch.get(flag), bool):
                    s[flag] = patch[flag]
            if patch.get("reset_hold") is True:
                self._hold_db = None
            rate = patch.get("rate")
            if (
                isinstance(rate, int)
                and not isinstance(rate, bool)
                and MIN_RATE_HZ <= rate <= MAX_RATE_HZ
            ):
                s["rate"] = rate
                self.stream.set_rate(rate)
            return dict(s)

    def frame(self) -> dict:
        """One display frame (JSON-able). While paused: the last one again, flagged `frozen`."""
        with self.lock:
            s = dict(self.s)
        st = self.stream
        out: dict = {
            "settings": s,
            "fs": st.fs,
            "status": "streaming" if st.age() < 1.0 else "waiting",
            "stats": {
                "packets": st.packets,
                "pps": st.pps(),
                "lost": st.lost_packets,
                "dropped": st.dropped_by_board,
                "gaps": st.gaps,
                "board": st.board_info,
                "verbose": st.board_verbose,
                "addr": f"{st.board[0]}:{st.board[1]}",
            },
        }
        if s["paused"]:
            out.update(self._frozen)
            out["frozen"] = bool(self._frozen)
            return out
        n = s["fft"]
        fs = st.fs
        length = int(min(max(round(s["scope_ms"] * fs / 1000), 16), MAX_SCOPE))
        x = st.snapshot(max(n, length * (2 if s["trigger"] else 1)))
        if x is None:
            out["filling"] = min(1.0, st.filled / n)
            return out

        xs = x[-n:]
        freq, db = spectrum(xs, fs, s["window"])
        # Average in power, exponentially; start over when what is averaged changes meaning.
        key = (n, s["window"], fs)
        power = 10 ** (db / 10)
        if key != self._key or self._avg_power is None:
            self._key, self._avg_power, self._hold_db = key, power, None
        else:
            self._avg_power += (power - self._avg_power) / s["avg"]
        db = 10 * np.log10(np.maximum(self._avg_power, 1e-30))  # never log10(0)
        if self._hold_db is None:
            self._hold_db = db.copy()
        else:
            np.maximum(self._hold_db, db, out=self._hold_db)

        pf, pdb = peak(freq, db, 1.0)
        measures = measure(xs, freq, db, fs, pf, pdb)
        lines = find_lines(freq, db, pf)
        shown, g = decimate_max(db)
        spec = {"df": float(freq[1] - freq[0]) * g, "y": np.round(shown, 1).tolist()}
        if s["hold"]:
            spec["hold"] = np.round(decimate_max(self._hold_db)[0], 1).tolist()
        if s["trigger"]:
            seg = x[-2 * length :]
            start = find_trigger(seg, length)
            sc = seg[start : start + length]
        else:
            sc = x[-length:]
        data = {
            "peak": {"f": pf, "db": pdb},
            "metrics": measures,
            "lines": lines,
            "spec": spec,
            "scope": {"y": np.round(sc - sc.mean(), 1).tolist()},
        }
        self.last_samples = xs.copy()
        self.last_spectrum = (freq, db)
        self._frozen = data
        out.update(data)
        return out


# --------------------------------------------------------------------------------------------
# The web side: one page, one event stream, one config endpoint.
# --------------------------------------------------------------------------------------------


def plotly_js() -> Path | None:
    """plotly.min.js from the `plotly` Python package, if installed (so the page works offline)."""
    spec = importlib.util.find_spec("plotly")
    if spec and spec.submodule_search_locations:
        path = Path(next(iter(spec.submodule_search_locations))) / "package_data" / "plotly.min.js"
        return path if path.is_file() else None
    return None


class Client:
    """What one open page is waiting for: the latest frame (older ones are dropped) and all the log lines."""

    def __init__(self) -> None:
        self._cond = threading.Condition()
        self._frame: bytes | None = None
        self._logs: list[bytes] = []

    def put_frame(self, message: bytes) -> None:
        with self._cond:
            self._frame = message
            self._cond.notify()

    def put_logs(self, message: bytes) -> None:
        with self._cond:
            self._logs = (self._logs + [message])[-200:]
            self._cond.notify()

    def get(self, timeout: float) -> tuple[bytes | None, list[bytes]]:
        with self._cond:
            if self._frame is None and not self._logs:
                self._cond.wait(timeout)
            frame, logs = self._frame, self._logs
            self._frame, self._logs = None, []
            return frame, logs


class Hub:
    """Fan-out to the connected pages; a slow page misses frames (never log lines) instead of slowing the rest."""

    def __init__(self) -> None:
        self._clients: set[Client] = set()
        self._lock = threading.Lock()

    def subscribe(self) -> Client:
        client = Client()
        with self._lock:
            self._clients.add(client)
        return client

    def unsubscribe(self, client: Client) -> None:
        with self._lock:
            self._clients.discard(client)

    def count(self) -> int:
        with self._lock:
            return len(self._clients)

    def _each(self) -> list[Client]:
        with self._lock:
            return list(self._clients)

    def publish(self, message: bytes) -> None:
        for client in self._each():
            client.put_frame(message)

    def publish_logs(self, message: bytes) -> None:
        for client in self._each():
            client.put_logs(message)


class Server:
    """The analyzer, a thread producing frames while a page is open, and the HTTP server."""

    def __init__(self, analyzer: Analyzer, host: str, port: int, fps: int):
        self.analyzer = analyzer
        self.hub = Hub()
        self.fps = fps
        self.stop_flag = threading.Event()
        self.httpd = http.server.ThreadingHTTPServer((host, port), self._handler())
        self.httpd.daemon_threads = True
        self.port = self.httpd.server_address[1]
        self._log_cursor = analyzer.stream.logs_since(0)[1] - 0
        self._status: str | None = None
        self._threads = [
            threading.Thread(target=self.httpd.serve_forever, daemon=True),
            threading.Thread(target=self._produce, daemon=True),
        ]

    def start(self) -> None:
        for t in self._threads:
            t.start()

    def close(self) -> None:
        self.stop_flag.set()
        self.httpd.shutdown()
        self.httpd.server_close()

    def _produce(self) -> None:
        period = 1.0 / self.fps
        stream = self.analyzer.stream
        while not self.stop_flag.is_set():
            t0 = time.monotonic()
            self._watch_board(stream)
            entries, self._log_cursor = stream.logs_since(self._log_cursor)
            if entries and self.hub.count():
                self.hub.publish_logs(json.dumps(entries, separators=(",", ":")).encode())
            if self.hub.count():
                self.hub.publish(json.dumps(self.analyzer.frame(), separators=(",", ":")).encode())
            time.sleep(max(0.005, period - (time.monotonic() - t0)))

    def _watch_board(self, stream: Stream) -> None:
        """Says in the log when samples start or stop coming."""
        status = "streaming" if stream.age() < 1.5 else "waiting"
        if status != self._status:
            if status == "streaming":
                stream.host_log("ok", f"receiving samples from {stream.board[0]}:{stream.board[1]}")
            elif self._status is not None:
                stream.host_log("warn", "no sample from the board for 1.5 s")
            self._status = status

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
                elif url.path == "/api/snapshot.csv":
                    kind = urllib.parse.parse_qs(url.query).get("kind", ["spectrum"])[0]
                    self._csv(kind)
                else:
                    self._send(404, b"not found", "text/plain")

            def do_POST(self) -> None:
                # application/json from a foreign page would need a CORS preflight, which we never grant.
                ctype = self.headers.get("Content-Type", "").split(";")[0]
                if self.path not in ("/api/config", "/api/cmd") or ctype != "application/json":
                    self._send(400, b"bad request", "text/plain")
                    return
                try:
                    length = min(int(self.headers.get("Content-Length", "0")), 4096)
                    patch = json.loads(self.rfile.read(length) or b"{}")
                    if not isinstance(patch, dict):
                        raise ValueError
                except ValueError:
                    self._send(400, b"bad json", "text/plain")
                    return
                if self.path == "/api/cmd":
                    error = server.analyzer.stream.send_command(str(patch.get("text", "")).strip())
                    if error:
                        server.analyzer.stream.host_log("warn", error)
                    verbose = server.analyzer.stream.want_verbose
                    body = json.dumps({"ok": error is None, "verbose": verbose}).encode()
                    self._send(200 if error is None else 400, body, "application/json")
                    return
                settings = server.analyzer.configure(patch)
                self._send(200, json.dumps(settings).encode(), "application/json")

            def _events(self) -> None:
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-store")
                self.end_headers()
                client = server.hub.subscribe()
                try:
                    # The log so far (the board's boot included), then what comes; the page skips lines it has.
                    history = server.analyzer.stream.log_history()
                    if history:
                        self._sse(b"log", json.dumps(history, separators=(",", ":")).encode())
                    while not server.stop_flag.is_set():
                        frame, logs = client.get(5)
                        if frame is None and not logs:
                            self.wfile.write(b": keep-alive\n\n")
                        for batch in logs:
                            self._sse(b"log", batch)
                        if frame is not None:
                            self._sse(None, frame)
                        self.wfile.flush()
                except OSError:
                    pass  # the page went away
                finally:
                    server.hub.unsubscribe(client)

            def _sse(self, event: bytes | None, data: bytes) -> None:
                self.wfile.write(
                    (b"event: " + event + b"\n" if event else b"") + b"data: " + data + b"\n\n"
                )

            def _csv(self, kind: str) -> None:
                a = server.analyzer
                if kind == "samples" and a.last_samples is not None:
                    fs = a.stream.fs
                    rows = ["t_s,code"] + [
                        f"{i / fs:.9f},{v:.0f}" for i, v in enumerate(a.last_samples)
                    ]
                elif kind == "spectrum" and a.last_spectrum is not None:
                    f, db = a.last_spectrum
                    rows = ["frequency_hz,db_rel_full_scale"] + [
                        f"{x:.3f},{y:.2f}" for x, y in zip(f, db)
                    ]
                else:
                    self._send(404, b"nothing captured yet", "text/plain")
                    return
                disposition = {"Content-Disposition": f"attachment; filename=adc-{kind}.csv"}
                self._send(200, ("\n".join(rows) + "\n").encode(), "text/csv", disposition)

        return Handler


# --------------------------------------------------------------------------------------------
# A simulated board, to try the tool (and test it) without hardware.
# --------------------------------------------------------------------------------------------


class FakeBoard:
    """Speaks the firmware's protocol on `127.0.0.1`: 1 kHz and 3.3 kHz tones plus noise.

    `drop_every=N` skips every Nth datagram (a lost one), to exercise the gap handling.
    """

    def __init__(self, drop_every: int = 0):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.settimeout(0.01)
        self.port = self.sock.getsockname()[1]
        self.drop_every = drop_every
        self.verbose = False
        self.reboots = 0
        self.stop_flag = threading.Event()
        self.t0 = time.monotonic()
        self.history: list[tuple[int, int, int, str]] = (
            []
        )  # (seq, uptime ms, level, text), like the firmware's
        self.say(0, "adc-stream · simulated board · MAC 00:00:00:00:00:00")
        self.say(0, "GPIO43 (ADC1 channel 1) · continuous mode with DMA · 611 to 83333 Hz")
        self.say(1, "link up · 1000 Mbit/s · full duplex")
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def say(self, level: int, text: str) -> tuple[int, int, int, str]:
        entry = (len(self.history), int((time.monotonic() - self.t0) * 1000), level, text)
        self.history.append(entry)
        return entry

    def _send_log(self, entry: tuple[int, int, int, str], addr) -> None:
        seq, ms, level, text = entry
        self.sock.sendto(LOG.pack(LOG_MAGIC, seq, ms, level) + text.encode(), addr)

    def _reply(self, level: int, text: str, addr) -> None:
        ms = int((time.monotonic() - self.t0) * 1000)
        self.sock.sendto(LOG.pack(REPLY_MAGIC, 0, ms, level) + text.encode(), addr)

    def _command(self, words: list[str], addr) -> bool:
        """The firmware's `info`, `verbose`, `reboot`, `help`; True if the board rebooted."""
        reply = lambda text, level=5: self._reply(level, text, addr)  # noqa: E731
        if words[0] == "info":
            up = int(time.monotonic() - self.t0)
            reply("chip   ESP32-S31 v0.1 · 500 MHz · MAC 00:00:00:00:00:00 (simulated)")
            reply("build  v0.1.0 · git simulated · last reset PowerOn")
            reply(
                "adc    GPIO43 (ADC1 ch1) · DMA continuous · 611-83333 Hz · 0 V = 2198, full 4393"
            )
            reply("net    127.0.0.1 · link 1000 Mbit/s full duplex")
            reply("stream idle · waiting for a `start` · DMA stalls 0")
            reply(
                f"system up {up // 3600}:{up // 60 % 60:02}:{up % 60:02} · verbose "
                f"{'on' if self.verbose else 'off'} · log history {len(self.history)} lines"
            )
        elif words[0] == "verbose":
            self.verbose = {"on": True, "off": False}.get(
                words[1] if len(words) > 1 else "", not self.verbose
            )
            reply("verbose on: one detail line per second" if self.verbose else "verbose off")
        elif words[0] == "reboot":
            reply("rebooting…")
            self.reboots += 1
            self.verbose = False
            return True
        elif words[0] == "help":
            reply("commands: info · verbose [on|off] · reboot · help")
        else:
            reply(f"{' '.join(words)!r}: not a command (try `help`)", 2)
        return False

    def close(self) -> None:
        self.stop_flag.set()
        self._thread.join(timeout=1)
        self.sock.close()

    def _run(self) -> None:
        rng = np.random.default_rng(1)
        client = None
        request = None
        seq = 0
        index = 0
        due = 0.0
        heard = 0.0
        next_stat = 0.0
        while not self.stop_flag.is_set():
            try:
                text, addr = self.sock.recvfrom(128)
                words = text.decode().split()
                if words[:1] == ["start"]:
                    opts = dict(w.split("=") for w in words[1:] if "=" in w)
                    new = (
                        min(max(int(opts.get("rate", 20000)), MIN_RATE_HZ), MAX_RATE_HZ),
                        min(max(int(opts.get("n", 256)), 16), 700),
                    )
                    heard = time.monotonic()
                    if (addr, new) != (client, request):
                        first = addr != client
                        client, request, seq, index, due = addr, new, 0, 0, time.monotonic()
                        next_stat = time.monotonic() + 2
                        self.say(
                            1,
                            f"client {addr[0]}:{addr[1]} · {new[0]} Hz · {new[1]} samples/datagram",
                        )
                        if first:  # a new reader gets the history first
                            for entry in self.history:
                                self._send_log(entry, addr)
                        else:
                            self._send_log(self.history[-1], addr)
                    rate_hz, count = new
                    reply = f"ok rate_mhz={rate_hz * 1000} n={count} pin=43 zero={ZERO_CODE} full={FULL_SCALE} verbose={int(self.verbose)}"
                    self.sock.sendto(reply.encode(), addr)
                elif words[:1] == ["stop"]:
                    client = None
                elif words and self._command(words, addr):
                    client = None  # a reboot: it forgets the reader, which will say `start` again
            except (TimeoutError, socket.timeout):
                pass
            if client is None or time.monotonic() - heard > 3.0:
                client = None
                continue
            rate_hz, count = request
            if time.monotonic() >= next_stat:
                next_stat += 2
                self._send_log(
                    self.say(
                        4,
                        f"{rate_hz / 1000:.1f} kS/s · {rate_hz / count:.0f} datagrams/s · {seq} sent · lost 0",
                    ),
                    client,
                )
            while time.monotonic() >= due:
                t = (index + np.arange(count)) / rate_hz
                volts = 800 * np.sin(2 * np.pi * 1000 * t) + 200 * np.sin(2 * np.pi * 3300 * t)
                volts += rng.normal(0, 8, count)
                codes = np.clip(ZERO_CODE + volts, 0, FULL_SCALE).astype("<u2")
                index += count
                due += count / rate_hz
                seq += 1
                if self.drop_every and seq % self.drop_every == 0:
                    continue
                head = HEADER.pack(MAGIC, seq - 1, rate_hz * 1000, 0, count, 43)
                self.sock.sendto(head + codes.tobytes(), client)


# --------------------------------------------------------------------------------------------


def selftest() -> int:
    """Whole chain on this computer: a simulated board, the receiver, the FFT, the web server."""
    failures = []

    def check(ok: bool, what: str) -> None:
        print(("ok    " if ok else "FAIL  ") + what)
        if not ok:
            failures.append(what)

    # 1. receiver and FFT against the simulated board
    board = FakeBoard(drop_every=0)
    stream = Stream(("127.0.0.1", board.port), 20000, 256)
    stream.start()
    time.sleep(0.8 + 4096 / stream.fs)
    x = stream.snapshot(4096)
    check(x is not None, "4096 contiguous samples received")
    if x is not None:
        expected = 20 * math.log10(800 / HALF_SCALE)
        for win in WINDOWS:
            freq, db = spectrum(x, stream.fs, win)
            f1, d1 = peak(freq, db, 1.0)
            # A line between two bins loses up to 3.9 dB with the rectangular window (scalloping).
            ok = abs(f1 - 1000) < 5 and abs(d1 - expected) < (4.5 if win == "rect" else 1.5)
            check(ok, f"{win}: line at {f1:.1f} Hz, {d1:.1f} dB (expect 1000, {expected:.1f})")
        freq, db = spectrum(x, stream.fs)
        mask = (freq > 2000) & (freq < 5000)
        f2 = freq[mask][np.argmax(db[mask])]
        check(abs(f2 - 3300) < 10, f"second line is {f2:.1f} Hz (expected 3300)")
        check(abs(stream.fs - 20000) < 1, "sample rate read from the headers: 20 kHz")
    check(stream.lost_packets == 0, "no datagram lost on localhost")
    check(
        stream.board_info.startswith("ok rate_mhz=20000000"),
        f"board answered: {stream.board_info!r}",
    )

    # 2. the web side: page, event stream, config, a rate change, pause, CSV
    server = Server(Analyzer(stream), "127.0.0.1", 0, fps=25)
    server.start()

    def get(path: str) -> tuple[int, bytes]:
        c = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
        c.request("GET", path)
        r = c.getresponse()
        return r.status, r.read()

    def post(patch: dict, ctype: str = "application/json") -> tuple[int, bytes]:
        c = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
        c.request("POST", "/api/config", json.dumps(patch), {"Content-Type": ctype})
        r = c.getresponse()
        return r.status, r.read()

    logs_seen: list[dict] = []

    def next_frame(conn: http.client.HTTPResponse, want) -> dict | None:
        """The next display frame that satisfies `want`; log events met on the way go to `logs_seen`."""
        deadline = time.time() + 6
        event = None
        while time.time() < deadline:
            line = conn.readline()
            if line.startswith(b"event: "):
                event = line[7:].strip()
            elif line.startswith(b"data: "):
                payload = json.loads(line[6:])
                if event == b"log":
                    logs_seen.extend(payload)
                elif want(payload):
                    return payload
                event = None
        return None

    status, body = get("/")
    check(status == 200 and b"<title>ADC FFT</title>" in body, "GET / serves the page")
    status, body = get("/plotly.min.js")
    check(
        status in (200, 302),
        f"GET /plotly.min.js ({'local package' if status == 200 else 'CDN redirect'})",
    )
    check(get("/nope")[0] == 404, "unknown path is a 404")

    events = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    events.request("GET", "/events")
    resp = events.getresponse()
    f = next_frame(resp, lambda f: "spec" in f)
    check(
        f is not None and abs(f["peak"]["f"] - 1000) < 5,
        "event stream carries frames, peak at 1 kHz",
    )
    check(
        any(e["src"] == "board" and "adc-stream" in e["text"] for e in logs_seen),
        f"the board's log reaches the page, boot banner replayed ({len(logs_seen)} lines so far)",
    )
    check(any(e["src"] == "host" for e in logs_seen), "the tool's own lines are in the log too")
    if f:
        check(
            len(f["spec"]["y"]) == 2049 and abs(f["spec"]["df"] - 20000 / 4096) < 1e-6,
            "4096-point FFT: 2049 bins",
        )
        check(
            len(f["scope"]["y"]) == 400 and f["status"] == "streaming",
            "waveform: 20 ms at 20 kS/s = 400 samples, streaming",
        )
        m = f["metrics"]
        check(abs(m["rms"] - 598) < 30, f"AC rms {m['rms']:.0f} codes (expected 598)")
        check(abs(m["dc"] - ZERO_CODE) < 5, f"DC level {m['dc']:.0f} (expected {ZERO_CODE})")
        top = f["lines"][0] if f["lines"] else {}
        check(
            abs(top.get("f", 0) - 1000) < 3 and top.get("h") == 1,
            f"strongest line {top.get('f')} Hz, harmonic number {top.get('h')} (expected 1000, 1)",
        )
        check(
            any(abs(line["f"] - 3300) < 8 for line in f["lines"]),
            "the 3.3 kHz line is in the table too",
        )
        # One tone is assumed: the 3.3 kHz one is "noise" to it, 10 log10(800^2 / 200^2 + ...) = about 12 dB.
        check(
            m["snr"] is not None and 10 < m["snr"] < 14,
            f"SNR estimate {m['snr']:.1f} dB (about 12)",
        )

    st, body = post({"scope_ms": 5, "trigger": True})
    check(st == 200 and json.loads(body)["scope_ms"] == 5, "POST: waveform span and trigger")
    f = next_frame(resp, lambda f: "scope" in f and len(f["scope"]["y"]) == 100)
    check(f is not None, "waveform: 5 ms = 100 samples")
    t = np.arange(2000) / 20000
    sine = ZERO_CODE + 800 * np.sin(2 * np.pi * 1000 * t + 1.0)
    i = find_trigger(sine, 400)
    check(
        i > 0
        and sine[i - 1] < sine[i]
        and sine[i] > sine.mean()
        and sine[i - 1] < sine.mean() + 200,
        f"the trigger finds a rising edge through the mean (sample {i})",
    )
    post({"scope_ms": 20, "trigger": False})

    st, body = post({"fft": 2048, "window": "blackman", "avg": 4, "hold": True})
    check(st == 200 and json.loads(body)["fft"] == 2048, "POST /api/config applies settings")
    f = next_frame(
        resp, lambda f: f["settings"]["fft"] == 2048 and "spec" in f and "hold" in f["spec"]
    )
    check(
        f is not None and len(f["spec"]["y"]) == 1025 and len(f["spec"]["hold"]) == 1025,
        "next frames use them (1025 bins, peak hold)",
    )
    st, body = post({"fft": 123, "window": "nope", "avg": 9999, "rate": "fast"})
    s = json.loads(body)
    check(
        s["fft"] == 2048 and s["window"] == "blackman" and s["avg"] == 4,
        "invalid settings are ignored",
    )
    check(post({"fft": 1024}, "text/plain")[0] == 400, "a POST that isn't JSON is refused")

    post({"rate": 40000})
    f = next_frame(
        resp, lambda f: abs(f["fs"] - 40000) < 1 and "spec" in f and f["settings"]["rate"] == 40000
    )
    check(
        f is not None and abs(f["peak"]["f"] - 1000) < 10,
        "rate change reaches the board: 40 kS/s, still a 1 kHz line",
    )

    post({"paused": True})
    f = next_frame(resp, lambda f: f["settings"]["paused"])
    check(
        f is not None and f.get("frozen") is True and "spec" in f,
        "paused: the last frame is repeated, flagged frozen",
    )
    post({"paused": False})
    next_frame(resp, lambda f: not f["settings"]["paused"])
    st, body = get("/api/snapshot.csv?kind=spectrum")
    check(st == 200 and body.startswith(b"frequency_hz,"), "spectrum CSV")
    st, body = get("/api/snapshot.csv?kind=samples")
    check(st == 200 and body.startswith(b"t_s,code") and body.count(b"\n") > 1000, "samples CSV")

    # Commands typed in the page's terminal: info, verbose (kept across a reboot), reboot, refusals.
    def command(text: str) -> int:
        c = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
        c.request(
            "POST", "/api/cmd", json.dumps({"text": text}), {"Content-Type": "application/json"}
        )
        r = c.getresponse()
        r.read()
        return r.status

    def saw(text: str, level: str | None = None) -> bool:
        return any(
            e["src"] == "board" and text in e["text"] and (level is None or e["level"] == level)
            for e in logs_seen
        )

    check(command("info") == 200, "POST /api/cmd info")
    next_frame(resp, lambda f: saw("system up"))
    replies = [e for e in logs_seen if e["level"] == "reply"]
    check(len(replies) == 6, f"info answers in six lines ({len(replies)})")
    check(
        any(e["src"] == "host" and e["level"] == "cmd" for e in logs_seen), "the command is echoed"
    )
    check(command("verbose on") == 200, "POST /api/cmd verbose on")
    f = next_frame(resp, lambda f: f["stats"]["verbose"] is True)
    check(f is not None and board.verbose, "the board is verbose, and its `ok` says so")
    check(command("reboot") == 200, "POST /api/cmd reboot")
    f = next_frame(
        resp, lambda f: board.reboots == 1 and board.verbose and f["status"] == "streaming"
    )
    check(
        saw("rebooting") and f is not None and board.verbose,
        "after a reboot the stream resumes and verbose is asked again",
    )
    check(command("verbose") == 200, "verbose without argument toggles")
    next_frame(resp, lambda f: f["stats"]["verbose"] is False)
    check(not board.verbose, "verbose off")
    check(command("start rate=5") == 400 and command("rm -rf") == 400, "other commands are refused")
    check(command("verbose maybe") == 400, "bad argument refused")

    resp.close()
    events.close()
    server.close()
    stream.close()
    board.close()

    # 3. lost datagrams
    board = FakeBoard(drop_every=40)
    stream = Stream(("127.0.0.1", board.port), 20000, 256)
    stream.start()
    time.sleep(1.5)
    check(stream.lost_packets > 0, f"lost datagrams are noticed ({stream.lost_packets} lost)")
    check(stream.filled < 40 * 256 + 256, "a gap empties the contiguous window")
    stream.close()
    board.close()
    print("selftest:", "FAILED" if failures else "passed")
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "--board", default="192.168.77.20", help="the board's address (default: %(default)s)"
    )
    ap.add_argument("--port", type=int, default=7100, help="its UDP port (default: %(default)s)")
    ap.add_argument(
        "--rate",
        type=int,
        default=20000,
        metavar="HZ",
        help="sample rate, Hz: 611 to 83333 (default: %(default)s)",
    )
    ap.add_argument(
        "--count", type=int, default=256, help="samples per datagram, 16-700 (default: %(default)s)"
    )
    ap.add_argument(
        "--fft", type=int, default=4096, choices=Analyzer.FFT_SIZES, help="initial FFT length"
    )
    ap.add_argument("--avg", type=int, default=1, help="initial averaging, 1 = none")
    ap.add_argument(
        "--scope-ms",
        type=int,
        default=20,
        help="time span of the waveform, ms (default: %(default)s)",
    )
    ap.add_argument(
        "--fps",
        type=int,
        default=20,
        help="frames per second sent to the page (default: %(default)s)",
    )
    ap.add_argument(
        "--http-port",
        type=int,
        default=8050,
        help="where the page is served (default: %(default)s)",
    )
    ap.add_argument(
        "--host", default="127.0.0.1", help="interface of the page; 0.0.0.0 shows it to the network"
    )
    ap.add_argument("--no-browser", action="store_true", help="don't open the page")
    ap.add_argument("--demo", action="store_true", help="use a simulated board on this computer")
    ap.add_argument(
        "--selftest", action="store_true", help="test receiver, FFT and web server, no browser"
    )
    args = ap.parse_args()

    args.rate = min(max(args.rate, MIN_RATE_HZ), MAX_RATE_HZ)
    args.count = min(max(args.count, 16), 700)

    if args.selftest:
        return selftest()

    fake = FakeBoard() if args.demo else None
    board = ("127.0.0.1", fake.port) if fake else (args.board, args.port)
    stream = Stream(board, args.rate, args.count)
    stream.start()
    server = Server(
        Analyzer(stream, args.fft, "hann", args.avg, args.scope_ms),
        args.host,
        args.http_port,
        args.fps,
    )
    server.start()
    url = f"http://{'127.0.0.1' if args.host == '0.0.0.0' else args.host}:{server.port}"
    print(f"ADC FFT at {url}   (board {board[0]}:{board[1]}, Ctrl-C to stop)")
    if not args.no_browser:
        webbrowser.open(url)
    warned = False
    try:
        while True:
            time.sleep(1)
            if not fake and not warned and stream.age() > 5:
                warned = True
                print(
                    "No data from the board yet: is adc-stream flashed, the cable in, the interface on 192.168.77.x?"
                )
    except KeyboardInterrupt:
        pass
    finally:
        server.close()
        stream.close()
        if fake:
            fake.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
