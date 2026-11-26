#!/usr/bin/env python3
"""Live spectrum (FFT) of the ADC samples streamed by the `adc-stream` firmware, in your browser.

    python3 scripts/dev.py setup                       # once: numpy and plotly go in .venv
    python3 scripts/dev.py flash -m --app adc          # flash the board (firmware/adc-stream/)
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
import copy
import http.client
import http.server
import importlib.util
import json
import math
import os
import queue
import re
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
BOARD_COMMANDS = ("info", "verbose", "pwm", "pot", "reboot", "help")
# `pwm [freq=HZ] [duty=%] [on|off]` (or `pwm 1000 25`): what the board's generator on GPIO20 accepts.
PWM_ARG = re.compile(r"on|off|(?:(?:freq|f|hz|duty|d)=)?[0-9]+(?:\.[0-9]*)?(?:e[+-]?[0-9]+)?", re.I)
PWM_MIN_HZ, PWM_MAX_HZ = 0.2, 1_000_000.0
# `pot [off|freq|duty] [min=N] [max=N]`: the potentiometer on GPIO44 sets the generator's frequency or duty cycle.
POT_ARG = re.compile(r"off|freq|duty|(?:min|max)=[0-9]+(?:\.[0-9]*)?(?:e[+-]?[0-9]+)?", re.I)
POT = struct.Struct(
    "<4sBxHHxxf"
)  # "POT1", mode, position (1/100 %), averaged code, the value the generator was given
POT_MAGIC = b"POT1"
POT_MODES = ("off", "freq", "duty")
MAX_SCOPE = 16384  # samples of the waveform at most
# The S31's SAR is differential: 0 V reads about ZERO, the full scale is FULL (esp-hal's constants).
ZERO_CODE = 2198
FULL_SCALE = 4393
HALF_SCALE = FULL_SCALE - ZERO_CODE  # amplitude of a full-scale sine, in codes
KEEPALIVE_S = 1.0
# The ADC's continuous mode (DMA) on the ESP32-S31: 611 Hz to 83 333 Hz (SOC_ADC_SAMPLE_FREQ_THRES_*).
MIN_RATE_HZ, MAX_RATE_HZ = 611, 83_333
POT_MAX_RATE_HZ = (
    MAX_RATE_HZ // 2
)  # the ADC alternates the two pins: the signal gets half the conversions
HISTORY = 65536  # samples kept: twice the largest FFT
PAGE = Path(__file__).with_name("adc_fft.html")
DEFAULT_RATE_HZ = 20_000
PREFS_VERSION = 1
CDN_PLOTLY = "https://cdnjs.cloudflare.com/ajax/libs/plotly.js/2.35.2/plotly.min.js"


# What /audio sends: records of `rate_mhz` (u32, the exact sample rate in mHz, from the board's timer), `flags`
# (u8: AUDIO_GAP = the samples before this record are missing, AUDIO_IDLE = a heartbeat, no sample came,
# AUDIO_F32 = the samples are float32 in codes around zero, the output of the digital filter) and `count` (u16),
# then `count` samples: raw ADC codes (u16 little-endian) or float32.
AUDIO = struct.Struct("<IBH")
AUDIO_GAP, AUDIO_IDLE, AUDIO_F32 = 1, 2, 4
AUDIO_MAX_BYTES = 8192  # samples of one record, at most 4096


def join_samples(
    items: list[tuple[int, bool, bytes]], limit: int = AUDIO_MAX_BYTES
) -> list[tuple[int, bool, bytes]]:
    """Joins what arrived in a burst into fewer pieces: samples that follow each other at the same rate."""
    out: list[list] = []
    for rate_mhz, gap, payload in items:
        last = out[-1] if out else None
        if (
            last is not None
            and not gap
            and last[0] == rate_mhz
            and len(last[2]) + len(payload) <= limit
        ):
            last[2] += payload
        else:
            out.append([rate_mhz, gap, bytearray(payload)])
    return [(rate_mhz, gap, bytes(body)) for rate_mhz, gap, body in out]


def audio_record(rate_mhz: int, gap: bool, payload: bytes, floats: bool = False) -> bytes:
    flags = (AUDIO_GAP if gap else 0) | (AUDIO_F32 if floats else 0)
    return AUDIO.pack(rate_mhz, flags, len(payload) // (4 if floats else 2)) + payload


def coalesce(items: list[tuple[int, bool, bytes]], limit: int = AUDIO_MAX_BYTES) -> list[bytes]:
    """`join_samples`, as records."""
    return [audio_record(rate, gap, body) for rate, gap, body in join_samples(items, limit)]


class AudioFilter:
    """The page's digital filter, run on the stream as it arrives (the FIR's tail is kept from one piece to the
    next), so that the sound can be the filtered signal. It follows the filter settings as they change.
    """

    def __init__(self, analyzer: "Analyzer") -> None:
        self.analyzer = analyzer
        self._key: tuple | None = None
        self._h = np.zeros(1)
        self._tail = np.zeros(0)

    def run(self, rate_mhz: int, gap: bool, payload: bytes) -> tuple[bytes | None, bool]:
        """(float32 samples in codes around zero, or None when the filter is off: send the raw ones, and whether the
        piece must be marked as starting afresh: the filter or its input changed)."""
        kind, f1, f2, sharp = self.analyzer.filter_spec()
        if kind == "off":
            fresh = self._key is not None
            self._key = None
            return None, fresh
        fs = rate_mhz / 1000
        taps = fir_taps(sharp, fs)
        key = (kind, f1, f2, taps, round(fs))
        fresh = gap or key != self._key
        if fresh:
            self._key = key
            self._h = fir_design(kind, f1, f2, fs, taps)
            self._tail = np.zeros(taps - 1)
        x = np.frombuffer(payload, dtype="<u2").astype(np.float64) - ZERO_CODE
        buf = np.concatenate([self._tail, x])
        self._tail = buf[len(buf) - (taps - 1) :] if taps > 1 else buf[:0]
        return fir_valid(buf, self._h).astype("<f4").tobytes(), fresh


class AudioHub:
    """Fan-out of the raw samples to the pages that listen. A page that falls behind loses its backlog and is told
    (a gap), it never slows the receiver."""

    def __init__(self) -> None:
        self._queues: set[queue.Queue] = set()
        self._lock = threading.Lock()

    def subscribe(self) -> queue.Queue:
        q: queue.Queue = queue.Queue(maxsize=512)
        with self._lock:
            self._queues.add(q)
        return q

    def unsubscribe(self, q: queue.Queue) -> None:
        with self._lock:
            self._queues.discard(q)

    def count(self) -> int:
        with self._lock:
            return len(self._queues)

    def publish(self, rate_mhz: int, gap: bool, payload: bytes) -> None:
        with self._lock:
            queues = list(self._queues)
        for q in queues:
            try:
                q.put_nowait((rate_mhz, gap, payload))
            except queue.Full:
                with q.mutex:
                    q.queue.clear()
                q.put_nowait((rate_mhz, True, payload))


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
        self.audio = AudioHub()  # the pages that listen to the signal
        self.stop_flag = threading.Event()
        self.packets = 0
        self.dgram_bytes = 0  # size of the last sample datagram, UDP payload
        self.started = time.monotonic()
        self.lost_packets = 0
        self.dropped_by_board = 0  # samples the board's ring buffer lost
        self.gaps = 0
        self.board_info = ""
        self.board_verbose: bool | None = (
            None  # what the board says (in its `ok`), None until known
        )
        self.want_verbose: bool | None = None  # what the user asked: re-applied after a reboot
        # The board's PWM generator, as its `ok` says ({on, freq, duty, pin}); None for a firmware without one.
        self.board_pwm: dict | None = None
        # The potentiometer: the mode and range from the board's `ok`, and the live reading from its `POT1` datagrams.
        self.board_pot: dict | None = None
        self._pot_live: dict | None = None
        self._pot_live_at = 0.0
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
        if words[0] == "pwm" and not all(PWM_ARG.fullmatch(w) for w in words[1:]):
            return "pwm [freq=HZ] [duty=%] [on|off]"
        if words[0] == "pot" and not all(POT_ARG.fullmatch(w) for w in words[1:]):
            return "pot [off|freq|duty] [min=N] [max=N]"
        self.host_log("cmd", text)
        try:
            self.sock.sendto(text.encode(), self.board)
            if words[0] in ("pwm", "pot"):
                self._send_request()  # its `ok` carries the generator's and the knob's new state
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
            elif datagram.startswith(POT_MAGIC) and len(datagram) >= POT.size:
                self._on_pot(datagram)
            elif datagram.startswith((LOG_MAGIC, REPLY_MAGIC)) and len(datagram) >= LOG.size:
                self._on_log(datagram)

    def _on_ok(self, info: str) -> None:
        """The board's answer to `start` says whether verbose is on: after a reboot it is off again."""
        pwm: dict = {}
        pot: dict = {}
        for word in info.split():
            if word.startswith("verbose="):
                self.board_verbose = word.endswith("1")
            key, _, value = word.partition("=")
            try:
                if key == "pwm":
                    pwm["on"] = value == "1"
                elif key in ("pwm_hz", "pwm_duty"):
                    pwm["freq" if key == "pwm_hz" else "duty"] = float(value)
                elif key == "pwm_pin":
                    pwm["pin"] = int(value)
                elif key == "pot":
                    pot["mode"] = value
                elif key == "pot_pin":
                    pot["pin"] = int(value)
                elif key in ("pot_lo", "pot_hi"):
                    pot["lo" if key == "pot_lo" else "hi"] = float(value)
            except ValueError:
                pass
        self.board_pwm = pwm if "on" in pwm else None
        self.board_pot = pot if pot.get("mode") in POT_MODES else None
        if self.want_verbose is not None and self.board_verbose != self.want_verbose:
            try:
                self.sock.sendto(b"verbose on" if self.want_verbose else b"verbose off", self.board)
            except OSError:
                pass

    def _on_pot(self, datagram: bytes) -> None:
        _, mode, position, code, value = POT.unpack_from(datagram)
        if mode < len(POT_MODES):
            with self.lock:
                self._pot_live = {
                    "mode": POT_MODES[mode],
                    "position": position / 100.0,  # % of the knob's travel
                    "code": code,
                    "value": value,  # Hz or %, what the generator was given
                }
                self._pot_live_at = time.monotonic()

    def pot_state(self) -> dict | None:
        """The potentiometer for the page: the board's mode and range, plus the live reading while it arrives."""
        with self.lock:
            if self.board_pot is None:
                return None
            state = dict(self.board_pot)
            if self._pot_live and time.monotonic() - self._pot_live_at < 1.0:
                state.update(self._pot_live)
                state["live"] = True
            return state

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
            self.dgram_bytes = len(datagram)
            self.last_rx = now
            self._arrivals.append(now)
        if self.audio.count():
            self.audio.publish(rate_mhz, gap, datagram[HEADER.size : HEADER.size + 2 * count])


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


def find_trigger(
    seg: np.ndarray, length: int, level: float = 0.0, slope: str = "rise", pos: float = 0.2
) -> tuple[int, float] | None:
    """Where to start a steady view of `seg`, like an oscilloscope's trigger.

    The trigger is a crossing of `level` (-100 to +100: percent of half the peak-to-peak swing, around
    the middle of the swing) going up (`slope` "rise") or down ("fall"). It is armed only after the
    signal went 10 % of the half-swing beyond the level on the other side (hysteresis: noise near the
    level doesn't trigger). `pos` (0 to 1) is where the trigger sits in the view: 0 puts it at the left
    edge, 0.5 in the middle (what happened before it is shown too). The latest crossing that leaves
    room for a whole view is used.

    Returns `(start, threshold, exact_start)`: the first sample of the view, the threshold (in the units
    of `seg`), and the same start with the crossing located between two samples (feed it to
    `view_at`); None when nothing triggers (flat signal, no crossing, `seg` too short).
    """
    n = len(seg)
    pre = int(round(min(max(pos, 0.0), 1.0) * (length - 1)))
    if n < length + 2 or length < 4:
        return None
    lo, hi = np.percentile(seg, [1, 99])
    half = float(hi - lo) / 2
    if half <= 0:
        return None
    thr = float((hi + lo) / 2 + min(max(level, -100.0), 100.0) / 100 * half)
    x, t = (seg, thr) if slope != "fall" else (-seg, -thr)
    hyst = 0.1 * half
    above = x >= t
    last_above = np.maximum.accumulate(np.where(above, np.arange(n), -1))
    # A crossing at i: x[i-1] below, x[i] at or above; the view [i - pre, i - pre + length) must fit.
    cross = np.flatnonzero(~above[:-1] & above[1:]) + 1
    cross = cross[(cross >= pre) & (cross - pre + length <= n)]
    for i in cross[::-1]:
        run = x[last_above[i - 1] + 1 : i]
        if run.size and run.min() < t - hyst:
            # Where the line between the two samples crosses the threshold (sub-sample).
            frac = (t - x[i - 1]) / (x[i] - x[i - 1])
            return int(i - pre), thr, float(i - 1 + frac - pre)
    return None


_KERNEL_TAPS = 8


def tapered_mean(x: np.ndarray) -> float:
    """The mean of `x` seen through a Hann window: a signal that doesn't fill whole periods of its tones
    (and hum) leaves a plain mean moving from one window to the next; this one hardly moves."""
    return float(np.average(x, weights=np.hanning(len(x) + 2)[1:-1]))


def view_at(seg: np.ndarray, start: float, length: int) -> np.ndarray:
    """`length` samples of `seg` starting at the fractional position `start`, by 8-tap windowed-sinc
    interpolation (flat within a fraction of a dB up to 0.4 of the sample rate). Needs 3 samples before
    and 4 after: callers keep `3 <= start` and `start + length + 4 <= len(seg)`."""
    i0 = int(np.floor(start))
    d = start - i0
    if d < 1e-9:
        return seg[i0 : i0 + length].astype(float)
    k = np.arange(-3, 5)
    arg = k - d
    h = np.sinc(arg) * (0.42 + 0.5 * np.cos(np.pi * arg / 4) + 0.08 * np.cos(2 * np.pi * arg / 4))
    h /= h.sum()
    out = np.zeros(length)
    for kk, hk in zip(k, h):
        out += hk * seg[i0 + kk : i0 + kk + length]
    return out


def strongest_tone(x: np.ndarray, fs: float, fmin: float = 20.0) -> float | None:
    """Frequency (Hz) of the strongest spectral line of `x` above `fmin`, refined between the bins by a
    parabola through the three bins around the peak; None when no line stands 10 dB above the median.
    """
    m = min(len(x), 8192)
    if m < 64:
        return None
    seg = x[-m:] - x[-m:].mean()
    mag = np.abs(np.fft.rfft(seg * np.hanning(m))) + 1e-12
    k0 = max(2, int(np.ceil(fmin * m / fs)))
    if k0 >= len(mag) - 2:
        return None
    k = k0 + int(np.argmax(mag[k0:-1]))
    if mag[k] < 3.2 * np.median(mag[k0:]):
        return None
    a, b, c = np.log(mag[k - 1 : k + 2])
    den = a - 2 * b + c
    off = 0.5 * (a - c) / den if den < 0 else 0.0
    f = (k + float(np.clip(off, -0.5, 0.5))) * fs / m
    return f if f < 0.45 * fs else None


def tone_crossings(
    seg: np.ndarray, fs: float, f0: float, length: int, level: float, slope: str, pos: float
) -> tuple[np.ndarray, float] | None:
    """The trigger of an oscilloscope's *filtered* source, made exact: fits the tone of frequency `f0`
    (see `strongest_tone`) to `seg` (amplitude and phase; a constant and a slope absorb the offset and
    drift), then returns where its rising (or falling) crossings of `level` (-100 to +100 % of the
    tone's amplitude) fall.

    Mains hum, other tones and noise are not in the reference, so every crossing is the same phase of the
    tone, whatever the rest of the signal does. Returns `(starts, level)`: the fractional first samples of
    the views that fit in `seg`, oldest first, and the level above the signal's mean (`level` % of the
    tone's amplitude, in the units of `seg`); None when no view fits."""
    n = len(seg)
    if n < length + 12:
        return None
    w = 2 * np.pi * f0 / fs
    i = np.arange(n)
    a = np.column_stack([np.cos(w * i), np.sin(w * i), np.ones(n), (i - n / 2) / n])
    ca, sa = np.linalg.lstsq(a, seg, rcond=None)[0][:2]
    amp = float(np.hypot(ca, sa))
    if amp <= 0:
        return None
    phi = float(np.arctan2(sa, ca))
    p = min(max(level / 100, -0.98), 0.98)
    u = np.arccos(p) * (1 if slope == "fall" else -1)  # cos falls on (0, pi), rises on (-pi, 0)
    pre = pos * (length - 1)
    period = 2 * np.pi / w
    first = (phi + u) / w
    k = np.arange(
        int(np.ceil((pre + 3 - first) / period)), int((n - 5 - length + pre - first) // period) + 1
    )
    starts = first + k * period - pre
    starts = starts[(starts >= 3) & (starts + length + 4 <= n)]
    if starts.size == 0:
        return None
    return starts, float(p * amp)


def pick_view(
    seg: np.ndarray, starts: np.ndarray, length: int, template: np.ndarray | None
) -> tuple[float, np.ndarray]:
    """Among the candidate views, the latest, or (with a `template`, the picture shown before) the one
    that looks most like it: a signal made of several tones repeats its pattern only now and then, and
    taking the same phase of the pattern keeps the picture still."""
    if starts.size > 160:  # keep the work bounded: every k-th, the latest included
        starts = starts[::-1][:: int(np.ceil(starts.size / 160))][::-1]
    if template is None or len(template) != length or starts.size == 1:
        st = float(starts[-1])
        return st, view_at(seg, st, length)
    ref = template - template.mean()
    best, best_score, best_view = float(starts[-1]), -np.inf, None
    for st in starts[::-1]:
        v = view_at(seg, float(st), length)
        d = v - v.mean()
        score = float(d @ ref) / (np.sqrt(float(d @ d) * float(ref @ ref)) + 1e-12)
        if score > best_score + 1e-9:  # ties go to the latest
            best, best_score, best_view = float(st), score, v
    assert best_view is not None
    return best, best_view


def remove_hum(
    x: np.ndarray, fs: float, base: float, f_hint: float | None = None, fit_last: int = 16384
) -> tuple[np.ndarray, float]:
    """Subtracts mains hum: `base` Hz (50 or 60) and its harmonics below 0.45 fs, fitted by least squares
    (amplitude and phase of each harmonic, so a hum that doesn't change is removed completely).
    The fit uses the last `fit_last` samples, the result is subtracted from all of `x`. The true mains
    frequency is a little off `base`: it is searched within +-0.4 Hz first (`f_hint` skips the search).
    Returns the cleaned samples and the frequency used."""
    t_all = np.arange(len(x)) / fs
    t = t_all[-fit_last:]
    y = x[-fit_last:] - x[-fit_last:].mean()

    def design(f: float, k: int, tv: np.ndarray) -> np.ndarray:
        w = 2 * np.pi * f * tv
        return np.column_stack(
            [np.cos(w * h) for h in range(1, k + 1)] + [np.sin(w * h) for h in range(1, k + 1)]
        )

    def energy(f: float, k: int) -> float:
        a = design(f, k, t)
        r = y - a @ np.linalg.lstsq(a, y, rcond=None)[0]
        return float(r @ r)

    if f_hint is None:
        # Coarse: 9 frequencies within +-0.4 Hz (fundamental only), then a golden-section search around
        # the best with three harmonics: a harmonic k is off by k times the frequency error.
        grid = base + np.linspace(-0.4, 0.4, 9)
        centre = float(grid[int(np.argmin([energy(float(f), 1) for f in grid]))])
        lo, hi = centre - 0.1, centre + 0.1
        k3 = int(max(1, min(3, 0.45 * fs // base)))
        g = 0.6180339887
        a_, b_ = hi - g * (hi - lo), lo + g * (hi - lo)
        ea, eb = energy(a_, k3), energy(b_, k3)
        for _ in range(24):
            if ea < eb:
                hi, b_, eb = b_, a_, ea
                a_ = hi - g * (hi - lo)
                ea = energy(a_, k3)
            else:
                lo, a_, ea = a_, b_, eb
                b_ = lo + g * (hi - lo)
                eb = energy(b_, k3)
        best_f = (lo + hi) / 2
    else:
        best_f = f_hint
    k = int(min(10, 0.45 * fs // best_f))
    if k < 1:
        return x, best_f
    coef = np.linalg.lstsq(design(best_f, k, t), y, rcond=None)[0]
    return x - design(best_f, k, t_all) @ coef, best_f


HOLD_WINDOWS = (
    0,
    0.1,
    0.2,
    0.5,
    1,
    2,
    5,
    10,
    30,
    60,
)  # seconds the envelope remembers (0: forever)
FILTERS = ("off", "lowpass", "highpass", "bandpass", "bandstop")
# Length of the FIR filter (taps) for each steepness; the transition band is about 5.5 fs / taps
# with the Blackman window used here, and the filter delays the signal by half its length.
SHARPNESS = {"soft": 127, "medium": 511, "sharp": 2047, "extreme": 4095}


def fir_taps(sharp: str, fs: float) -> int:
    """Odd number of taps for a steepness, never longer than a quarter second of samples."""
    n = min(SHARPNESS[sharp], int(fs * 0.25))
    return max(n | 1, 15)


def fir_design(kind: str, f1: float, f2: float, fs: float, taps: int) -> np.ndarray:
    """Linear-phase FIR (windowed sinc, Blackman) for `kind`; `f1` is the cutoff (low-pass, high-pass) or
    the lower edge (band-pass, band-stop), `f2` the upper edge. Edges are kept inside 0..fs/2."""
    nyq = fs / 2
    f1 = min(max(f1, 0.5), nyq * 0.995)
    f2 = min(max(f2, 0.5), nyq * 0.995)
    if kind in ("bandpass", "bandstop") and f1 > f2:
        f1, f2 = f2, f1
    m = np.arange(taps) - (taps - 1) / 2

    def lowpass(fc: float) -> np.ndarray:
        return 2 * fc / fs * np.sinc(2 * fc / fs * m)

    delta = np.zeros(taps)
    delta[(taps - 1) // 2] = 1.0
    if kind == "lowpass":
        h = lowpass(f1)
    elif kind == "highpass":
        h = delta - lowpass(f1)
    elif kind == "bandpass":
        h = lowpass(f2) - lowpass(f1)
    else:  # bandstop
        h = delta - (lowpass(f2) - lowpass(f1))
    h = h * np.blackman(taps)
    if kind in ("lowpass", "bandstop"):
        h /= h.sum()  # unity gain at DC
    return h


def fir_valid(x: np.ndarray, h: np.ndarray) -> np.ndarray:
    """`x` filtered by `h` without edge effects: `len(h) - 1` samples shorter (the output at index i
    belongs to input index i + (len(h) - 1) / 2)."""
    n = len(x) + len(h) - 1
    size = 1 << (n - 1).bit_length()
    y = np.fft.irfft(np.fft.rfft(x, size) * np.fft.rfft(h, size), size)
    return y[len(h) - 1 : len(x)]


def response_db(h: np.ndarray, n: int) -> np.ndarray:
    """Magnitude response of `h` in dB on the `n // 2 + 1` bins of an `n`-point spectrum."""
    return 20 * np.log10(np.maximum(np.abs(np.fft.rfft(h, n)), 1e-6))


def histogram(xs: np.ndarray, max_bins: int = 256) -> dict:
    """Distribution of the codes: one bar per code, or per group of codes when there are more than
    `max_bins` (`step`). `clip` is the share of samples at the very ends of the range."""
    codes = xs.astype(np.int64)
    lo, hi = int(codes.min()), int(codes.max())
    step = max(1, -(-(hi - lo + 1) // max_bins))
    counts = np.bincount((codes - lo) // step)
    return {
        "lo": lo,
        "step": step,
        "counts": counts.tolist(),
        "clip": float(np.mean((codes <= 1) | (codes >= FULL_SCALE - 1))),
        "mean": float(xs.mean()),
        "std": float(xs.std()),
    }


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


def harmonic_series(
    freq: np.ndarray, db: np.ndarray, f0: float, db0: float, fs: float, count: int = 32
) -> list[dict]:
    """Levels of the harmonics of the line at `f0` (`db0`): for each k, the line near k * f0, folded back
    below half the sample rate when k * f0 is above it (`alias` True: the line is a copy of the real harmonic
    at k * f0, which the ADC can't see). The search follows the harmonics: each one found refines the f0
    they are looked for at, so a small error on f0 doesn't walk off the 30th. Levels are in dB (as the
    spectrum's), taken as the strongest of the bins around the line (a line between two bins loses up to 1.4
    dB with the Hann window)."""
    df = float(freq[1] - freq[0])
    nyq = fs / 2
    floor_db = float(np.median(db))
    out: list[dict] = []
    f_est = f0
    for k in range(1, count + 1):
        f = k * f_est
        r = f % fs
        fa = fs - r if r > nyq else r  # where the sampled line sits
        i = int(round(fa / df))
        w = (
            3 + k // 4
        )  # bins each side: wider for the high ones, where the error on f0 is multiplied by k
        lo, hi = max(i - w, 1), min(i + w + 1, len(db) - 1)
        if lo >= hi:
            break
        j = lo + int(np.argmax(db[lo:hi]))
        level = float(db[j])
        alias = f > nyq
        if k == 1:
            level = db0
        elif not alias and level > floor_db + 8:
            f_est = float(freq[j]) / k
        out.append(
            {
                "k": k,
                "f": round(k * f_est, 2),
                "fa": round(float(freq[j]) if k > 1 else f0, 2),
                "db": round(level, 1),
                "alias": alias,
            }
        )
    return out


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


class SlidingExtrema:
    """Per-position minimum and maximum of the arrays pushed during the last `window` seconds, or since the
    last reset when `window` is 0 (forever).

    Remembering every array would cost a lot of memory (25 per second, up to 16k values each), so the
    time is cut in `BUCKETS` slices: each keeps its own minimum and maximum, and a slice leaves the
    window as a whole. The window is therefore exact to one slice (`window / BUCKETS`).
    """

    BUCKETS = 30

    def __init__(self) -> None:
        self._slices: collections.deque = collections.deque()  # [start, minimum, maximum]
        # `reset` comes from a request thread while the producer pushes and reads: one at a time.
        self._lock = threading.Lock()

    def reset(self) -> None:
        with self._lock:
            self._slices.clear()

    def push(self, x: np.ndarray, window: float, now: float) -> None:
        with self._lock:
            q = self._slices
            if q and len(q[-1][1]) != len(x):  # not the same view any more: start over
                q.clear()
            span = window / self.BUCKETS
            if not q or (window and now - q[-1][0] >= span):
                q.append([now, x.copy(), x.copy()])
            else:
                np.minimum(q[-1][1], x, out=q[-1][1])
                np.maximum(q[-1][2], x, out=q[-1][2])
            if window:
                while len(q) > 1 and q[0][0] + span < now - window:
                    q.popleft()
            elif len(q) > 1:  # back to "forever": the slices merge into one
                q[0] = [
                    q[0][0],
                    np.minimum.reduce([b[1] for b in q]),
                    np.maximum.reduce([b[2] for b in q]),
                ]
                while len(q) > 1:
                    q.pop()

    def extrema(self) -> tuple[np.ndarray, np.ndarray] | None:
        with self._lock:
            q = self._slices
            if not q:
                return None
            if len(q) == 1:
                return q[0][1].copy(), q[0][2].copy()
            return np.minimum.reduce([b[1] for b in q]), np.maximum.reduce([b[2] for b in q])


def default_prefs_path() -> Path:
    """`~/.config/adc-fft/settings.json` (under `$XDG_CONFIG_HOME` when it is set)."""
    base = os.environ.get("XDG_CONFIG_HOME")
    return (Path(base) if base else Path.home() / ".config") / "adc-fft" / "settings.json"


class Prefs:
    """What the user chose, kept in a JSON file so that a restart finds it again.

    Four sections: `settings` (the analyzer's: sample rate, FFT, filter, trigger...), `ui` and `theme` (the
    page's view settings), `gen` (the demo's simulated signal). Changes are written half a second after the
    last one (a slider sends many), by `flush`, and at exit. `path` None: nothing is read or written.
    """

    SECTIONS = ("settings", "ui", "theme", "gen")

    def __init__(self, path: Path | None):
        self.path = path
        self.data: dict = {}
        self.problem: str | None = None  # why the file could not be read or written
        self.saved_at: float | None = None  # when it was last written (time.time())
        self._lock = threading.Lock()
        self._dirty_at: float | None = None
        if path is not None:
            self._load()

    def _load(self) -> None:
        assert self.path is not None
        try:
            raw = json.loads(self.path.read_text("utf-8"))
        except FileNotFoundError:
            return
        except (OSError, ValueError) as e:
            self.problem = f"can't read {self.path} ({e}): starting from the defaults"
            return
        if isinstance(raw, dict) and raw.get("version") == PREFS_VERSION:
            self.data = {k: raw[k] for k in self.SECTIONS if k in raw}

    def get(self, section: str, default=None):
        with self._lock:
            return copy.deepcopy(self.data.get(section, default))

    def set(self, section: str, value) -> None:
        """Remembers `value` for `section` (written soon, if it changed)."""
        if self.path is None:
            return
        with self._lock:
            if self.data.get(section) != value:
                self.data[section] = copy.deepcopy(value)
                self._dirty_at = time.monotonic()

    def clear(self) -> None:
        """Forgets everything, the file included."""
        with self._lock:
            self.data = {}
            self._dirty_at = None
            if self.path is not None:
                try:
                    self.path.unlink()
                except FileNotFoundError:
                    pass
                except OSError as e:
                    self.problem = f"can't delete {self.path} ({e})"

    def flush(self, force: bool = False) -> str | None:
        """Writes the file when a change is half a second old (or at once with `force`). Returns the reason
        when the write fails."""
        if self.path is None:
            return None
        with self._lock:
            if self._dirty_at is None or (not force and time.monotonic() - self._dirty_at < 0.5):
                return None
            text = json.dumps({"version": PREFS_VERSION, **self.data}, indent=1, sort_keys=True)
            self._dirty_at = None
            tmp = self.path.with_name(self.path.name + ".tmp")
            try:
                self.path.parent.mkdir(parents=True, exist_ok=True)
                tmp.write_text(text + "\n", "utf-8")
                os.replace(tmp, self.path)  # whole file or nothing, even if we are killed meanwhile
                self.saved_at = time.time()
                self.problem = None
            except OSError as e:
                self.problem = f"can't write {self.path} ({e})"
                return self.problem
        return None


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
            "trigger": False,  # steady waveform: start at a crossing of the trigger level
            "scope_avg": 1,  # frames averaged in the waveform
            "hum": "off",  # remove mains hum from the waveform: "off" | "50" | "60"
            "filt": "off",  # digital filter on everything shown: see FILTERS
            "filt_f1": 1000.0,  # Hz: cutoff, or lower edge of the band
            "filt_f2": 5000.0,  # Hz: upper edge of the band
            "filt_sharp": "medium",  # see SHARPNESS
            "filt_raw": True,  # show the unfiltered signal underneath, for comparison
            "scope_hold": False,  # min/max envelope of the waveform
            "scope_hold_s": 0,  # how long it remembers, seconds (a sliding window); 0: until reset
            #  (frames come 20 to 25 times a second: 0.1 s is two or three of them)
            "trig_slope": "rise",  # "rise" | "fall"
            "trig_level": 0,  # percent of half the swing, -100..100
            "trig_pos": 20,  # percent of the view before the trigger point, 0..100
            "trig_mode": "auto",  # "auto": free-run without trigger; "normal": keep the last triggered view
            "trig_lock": "pattern",  # "edge": the signal's own edges; "tone": phase of the strongest line;
            #  "pattern": the tone's phase, and among its crossings the one that looks like the last picture
        }
        self._avg_power: np.ndarray | None = None
        self._hold_db: np.ndarray | None = None
        self._key: tuple | None = None
        self._frozen: dict = {}
        self._fir: tuple | None = None  # (key, taps, _) of the filter in use
        self._resp: tuple | None = None  # (key, response in dB) for the page
        self._avg_raw: np.ndarray | None = None  # averaged power of the unfiltered spectrum
        self._hum: tuple[float, float, str] = (0.0, -1e9, "")  # mains frequency found, when, base
        self._last_scope: tuple[np.ndarray, dict, float] | None = (
            None  # last triggered view (normal mode)
        )
        self._scope_key: tuple | None = None
        self._scope_avg: np.ndarray | None = None
        self._envelope = SlidingExtrema()
        self.last_samples: np.ndarray | None = None
        self.last_spectrum: tuple[np.ndarray, np.ndarray] | None = None

    def filter_spec(self) -> tuple[str, float, float, str]:
        """The digital filter as it is set: kind, f1, f2, steepness."""
        with self.lock:
            s = self.s
            return s["filt"], float(s["filt_f1"]), float(s["filt_f2"]), s["filt_sharp"]

    def persistent(self) -> dict:
        """The settings worth keeping from one run to the next (not the pause)."""
        with self.lock:
            return {k: v for k, v in self.s.items() if k != "paused"}

    def load(self, saved: dict) -> None:
        """Applies settings kept earlier (what is invalid or unknown is ignored, as for the page's)."""
        self.configure(
            {k: v for k, v in saved.items() if k not in ("paused", "reset_hold", "rate")}
        )

    def reset(self) -> dict:
        """Back to the defaults (sample rate included); the pause stays as it is."""
        fresh = Analyzer(self.stream).s
        fresh["rate"] = DEFAULT_RATE_HZ
        fresh.pop("paused")
        return self.configure({**fresh, "reset_hold": True})

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
            if isinstance(patch.get("scope_avg"), int) and 1 <= patch["scope_avg"] <= 64:
                s["scope_avg"] = patch["scope_avg"]
            if patch.get("hum") in ("off", "50", "60"):
                s["hum"] = patch["hum"]
            if patch.get("filt") in FILTERS:
                s["filt"] = patch["filt"]
            if patch.get("filt_sharp") in SHARPNESS:
                s["filt_sharp"] = patch["filt_sharp"]
            for key in ("filt_f1", "filt_f2"):
                v = patch.get(key)
                if isinstance(v, (int, float)) and not isinstance(v, bool) and 1 <= v <= 1e5:
                    s[key] = float(v)
            if patch.get("trig_slope") in ("rise", "fall"):
                s["trig_slope"] = patch["trig_slope"]
            if patch.get("trig_mode") in ("auto", "normal"):
                s["trig_mode"] = patch["trig_mode"]
            if patch.get("trig_lock") in ("edge", "tone", "pattern"):
                s["trig_lock"] = patch["trig_lock"]
            if patch.get("scope_hold_s") in HOLD_WINDOWS and not isinstance(
                patch.get("scope_hold_s"), bool
            ):
                s["scope_hold_s"] = patch["scope_hold_s"]
            for key, lo, hi in (("trig_level", -100, 100), ("trig_pos", 0, 100)):
                v = patch.get(key)
                if isinstance(v, int) and not isinstance(v, bool) and lo <= v <= hi:
                    s[key] = v
            for flag in ("hold", "paused", "trigger", "scope_hold", "filt_raw"):
                if isinstance(patch.get(flag), bool):
                    s[flag] = patch[flag]
            if patch.get("reset_hold") is True:
                self._hold_db = None
                self._envelope.reset()
            rate = patch.get("rate")
            if (
                isinstance(rate, int)
                and not isinstance(rate, bool)
                and MIN_RATE_HZ <= rate <= MAX_RATE_HZ
            ):
                s["rate"] = rate
                self.stream.set_rate(rate)
            return dict(s)

    def _scope(self, x: np.ndarray, raw: np.ndarray | None, length: int, s: dict) -> dict:
        """The waveform: the last `length` samples of `x` (already filtered), or a triggered view of them;
        mean removed, then averaged from frame to frame (`scope_avg`) and held as a min/max envelope
        (`scope_hold`). `raw` is the same samples before the filter: the same view of it is sent for
        comparison, mean removed too and never averaged.

        Averaging works with or without trigger. Without it the views have no common phase, so a periodic
        signal averages towards zero (noise too): that is what an oscilloscope's average does as well.
        """
        trig: dict | None = None
        dc = float(x[-length:].mean())
        fresh = True  # False: the held view is shown again, not averaged a second time
        rview = raw[-length:] if raw is not None else None
        rdc: float | None = None  # the mean taken off the unfiltered view; None: its own
        if s["trigger"]:
            m3 = min(length * 3, len(x))
            # The pattern lock looks further back for a period that resembles the last picture: a signal
            # of several tones recurs only now and then (a chord about every 100 ms).
            m = min(length * 6, len(x)) if s["trig_lock"] == "pattern" else m3
            seg = x[-m:]
            pos = s["trig_pos"] / 100
            start = thr = view = level_rel = None
            if s["trig_lock"] != "edge":
                # The strongest line of the whole window (finer than that of the short segment).
                f0 = strongest_tone(x, self.stream.fs)
                cand = (
                    tone_crossings(
                        seg, self.stream.fs, f0, length, s["trig_level"], s["trig_slope"], pos
                    )
                    if f0 is not None
                    else None
                )
                if cand is not None:
                    template = (
                        self._last_scope[0]
                        if s["trig_lock"] == "pattern" and self._last_scope
                        else None
                    )
                    start, view = pick_view(seg, cand[0], length, template)
                    level_rel = cand[1]
            if start is None:
                m, seg = m3, x[-m3:]
                found = find_trigger(seg, length, s["trig_level"], s["trig_slope"], pos)
                if found is not None:
                    thr, start = found[1], found[2]
                    ok = start >= 3 and start + length + 4 <= len(seg)
                    view = view_at(seg, start, length) if ok else seg[found[0] : found[0] + length]
            if start is not None:
                if raw is not None:
                    rs = raw[-m:]
                    rview = (
                        view_at(rs, start, length)
                        if start >= 3 and start + length + 4 <= len(rs)
                        else rs[int(round(start)) : int(round(start)) + length]
                    )
                # The mean of the whole window, not of this view: a view holds a different share of a
                # period each time, so its own mean (and the trigger level drawn against it) would jump.
                dc = tapered_mean(x)
                rdc = tapered_mean(raw) if raw is not None else None
                view = view - dc
                mark = {
                    "i": int(round(s["trig_pos"] / 100 * (length - 1))),
                    "level": float(level_rel if level_rel is not None else thr - dc),
                }
                trig = {"state": "triggered", **mark}
                self._last_scope = (view, mark, dc, rview)
            elif (
                s["trig_mode"] == "normal"
                and self._last_scope
                and len(self._last_scope[0]) == length
            ):
                view, mark, dc, rview = self._last_scope
                trig, fresh = {"state": "held", **mark}, False
            else:
                view = x[-length:] - dc
                trig = {"state": "free"}
        else:
            view = x[-length:] - dc

        # Averaging and the envelope start over when what they combine changes meaning.
        key = (
            length,
            round(self.stream.fs),
            s["trigger"],
            s["trig_slope"],
            s["trig_level"],
            s["trig_pos"],
            s["trig_mode"],
            s["trig_lock"],
            s["hum"],
            s["filt"],
            s["filt_f1"],
            s["filt_f2"],
            s["filt_sharp"],
        )
        if key != self._scope_key or self._scope_avg is None or len(self._scope_avg) != length:
            self._scope_key, self._scope_avg = key, view.copy()
            self._envelope.reset()
        elif fresh:
            self._scope_avg += (view - self._scope_avg) / s["scope_avg"]
        shown = self._scope_avg
        out: dict = {"y": np.round(shown, 1).tolist(), "dc": round(dc, 1)}
        if rview is not None and s["filt_raw"]:
            rd = rdc if rdc is not None else float(rview.mean())
            out["raw"] = np.round(rview - rd, 1).tolist()
            out["rawdc"] = round(rd, 1)
        if s["scope_hold"]:
            extrema = None if fresh else self._envelope.extrema()
            if extrema is None:
                self._envelope.push(shown, float(s["scope_hold_s"]), time.monotonic())
                extrema = self._envelope.extrema()
            lo, hi = extrema if extrema is not None else (shown, shown)  # a reset came in between
            out["min"] = np.round(lo, 1).tolist()
            out["max"] = np.round(hi, 1).tolist()
        else:
            self._envelope.reset()  # off: it starts afresh when switched on
        if trig is not None:
            out["trig"] = trig
        return out

    def _process(
        self, x: np.ndarray, s: dict, fs: float
    ) -> tuple[np.ndarray, np.ndarray | None, dict]:
        """Hum removal, then the digital filter. Returns the processed samples, the original samples
        aligned with them (None when nothing is applied), and facts about the filter for the page.
        """
        raw: np.ndarray | None = None
        info: dict = {}
        if s["hum"] == "off" and s["filt"] == "off":
            return x, None, info
        xh = x
        if s["hum"] != "off":
            # The exact mains frequency is searched again every 3 s only.
            now = time.monotonic()
            hint = self._hum[0] if now - self._hum[1] < 3 and self._hum[2] == s["hum"] else None
            xh, f0 = remove_hum(x, fs, float(s["hum"]), hint)
            if hint is None:
                self._hum = (f0, now, s["hum"])
            info["hum_hz"] = round(self._hum[0], 3)
        y, delay = xh, 0
        if s["filt"] != "off":
            taps = fir_taps(s["filt_sharp"], fs)
            fkey = (s["filt"], s["filt_f1"], s["filt_f2"], taps, round(fs))
            if self._fir is None or self._fir[0] != fkey:
                self._fir = (fkey, fir_design(s["filt"], s["filt_f1"], s["filt_f2"], fs, taps), {})
            y = fir_valid(xh, self._fir[1])
            delay = (taps - 1) // 2
            info["taps"] = taps
            info["delay_ms"] = round(delay / fs * 1000, 1)
        raw = x[delay : delay + len(y)]
        return y, raw, info

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
                "pwm": st.board_pwm,
                "pot": st.pot_state(),
                "addr": f"{st.board[0]}:{st.board[1]}",
                "age": round(st.age(), 3),  # seconds since the last datagram
                "dgram": st.dgram_bytes,  # UDP payload of a sample datagram, bytes
                "count": st.count,  # samples per datagram asked for
                "asked": st.requested_hz,
            },
        }
        if s["paused"]:
            out.update(self._frozen)
            out["frozen"] = bool(self._frozen)
            return out
        n = s["fft"]
        fs = st.fs
        length = int(min(max(round(s["scope_ms"] * fs / 1000), 16), MAX_SCOPE))
        # The filter eats `taps - 1` samples: ask for them on top of what the views need.
        eaten = fir_taps(s["filt_sharp"], fs) - 1 if s["filt"] != "off" else 0
        want = max(n, min(length * 3, HISTORY) if s["trigger"] else length) + eaten
        x = st.snapshot(min(want, HISTORY))
        if x is None:
            out["filling"] = min(1.0, st.filled / n)
            return out

        y, raw, info = self._process(x, s, fs)
        out["filter"] = info
        if len(y) < n:  # not enough history for this filter and FFT length: show what there is
            out["filling"] = len(y) / n
            return out
        xs = y[-n:]
        raw_xs = raw[-n:] if raw is not None else xs
        freq, db = spectrum(xs, fs, s["window"])
        # Average in power, exponentially; start over when what is averaged changes meaning.
        key = (n, s["window"], fs, s["hum"], s["filt"], s["filt_f1"], s["filt_f2"], s["filt_sharp"])
        power = 10 ** (db / 10)
        if key != self._key or self._avg_power is None:
            self._key, self._avg_power, self._hold_db, self._avg_raw = key, power, None, None
        else:
            self._avg_power += (power - self._avg_power) / s["avg"]
        db = 10 * np.log10(np.maximum(self._avg_power, 1e-30))  # never log10(0)
        if self._hold_db is None:
            self._hold_db = db.copy()
        else:
            np.maximum(self._hold_db, db, out=self._hold_db)

        pf, pdb = peak(freq, db, 1.0)
        shown, g = decimate_max(db)
        spec = {"df": float(freq[1] - freq[0]) * g, "y": np.round(shown, 1).tolist()}
        if s["hold"]:
            spec["hold"] = np.round(decimate_max(self._hold_db)[0], 1).tolist()
        if raw is not None:
            # The spectrum of the unfiltered signal: underneath for comparison, and what the readouts
            # (SNR, noise floor, ENOB) measure, since those are about the ADC and not the filter.
            _, db_raw = spectrum(raw_xs, fs, s["window"])
            p_raw = 10 ** (db_raw / 10)
            if self._avg_raw is None:
                self._avg_raw = p_raw
            else:
                self._avg_raw += (p_raw - self._avg_raw) / s["avg"]
            db_raw = 10 * np.log10(np.maximum(self._avg_raw, 1e-30))
            if s["filt_raw"]:
                spec["raw"] = np.round(decimate_max(db_raw)[0], 1).tolist()
            pf_raw, pdb_raw = peak(freq, db_raw, 1.0)
            measures = measure(raw_xs, freq, db_raw, fs, pf_raw, pdb_raw)
        else:
            measures = measure(xs, freq, db, fs, pf, pdb)
        if s["filt"] != "off":
            resp_key = (self._fir[0], n)
            if self._resp is None or self._resp[0] != resp_key:
                self._resp = (
                    resp_key,
                    np.round(decimate_max(response_db(self._fir[1], n))[0], 1).tolist(),
                )
            spec["resp"] = self._resp[1]
        lines = find_lines(freq, db, pf)
        scope = self._scope(y, raw, length, s)
        stands_out = pdb > float(np.median(db)) + 15 and pf > 0
        data = {
            "harm": harmonic_series(freq, db, pf, pdb, fs) if stands_out else None,
            "peak": {"f": pf, "db": pdb},
            "metrics": measures,
            "lines": lines,
            "spec": spec,
            "scope": scope,
            "hist": histogram(raw_xs),
        }
        self.last_samples = raw_xs.copy()
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

    def __init__(
        self, analyzer: Analyzer, host: str, port: int, fps: int, prefs: Prefs | None = None
    ):
        self.analyzer = analyzer
        self.prefs = prefs or Prefs(None)
        self.started = time.monotonic()
        self._prefs_problem: str | None = None
        self.hub = Hub()
        self.fps = fps
        self.fake: FakeBoard | None = (
            None  # the simulated board, in the demo: its signal is editable
        )
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
        self.prefs.flush(force=True)

    def page(self) -> bytes:
        """The page, with the saved view settings written into it (so it draws right at once)."""
        saved = {
            "path": str(self.prefs.path) if self.prefs.path else None,
            "ui": self.prefs.get("ui", {}),
            "theme": self.prefs.get("theme"),
        }
        blob = json.dumps(saved).replace("<", "\\u003c")  # never a `</script>` in the data
        return PAGE.read_text("utf-8").replace("/*PREFS*/null", blob, 1).encode()

    def app_info(self) -> dict:
        """What the page's status box says about this tool (not about the board or the signal)."""
        prefs = self.prefs
        return {
            "demo": self.fake is not None,
            "pages": self.hub.count(),  # open pages
            "fps": self.fps,
            "uptime": round(time.monotonic() - self.started, 1),  # seconds
            "now": time.time(),
            "prefs": {
                "path": str(prefs.path) if prefs.path else None,
                "saved_at": prefs.saved_at,
                "problem": prefs.problem,
            },
        }

    def _save_prefs(self) -> None:
        problem = self.prefs.flush()
        if problem and problem != self._prefs_problem:
            self.analyzer.stream.host_log("warn", problem)
        self._prefs_problem = problem

    def _produce(self) -> None:
        period = 1.0 / self.fps
        stream = self.analyzer.stream
        while not self.stop_flag.is_set():
            t0 = time.monotonic()
            self._watch_board(stream)
            self._save_prefs()
            entries, self._log_cursor = stream.logs_since(self._log_cursor)
            if entries and self.hub.count():
                self.hub.publish_logs(json.dumps(entries, separators=(",", ":")).encode())
            if self.hub.count():
                frame = self.analyzer.frame()
                frame["app"] = self.app_info()
                self.hub.publish(json.dumps(frame, separators=(",", ":")).encode())
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
                        server.page(),
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
                elif url.path == "/audio":
                    self._audio(url.query)
                elif url.path == "/api/gen" and server.fake is not None:
                    self._gen(server.fake.signal)
                elif url.path == "/api/snapshot.csv":
                    kind = urllib.parse.parse_qs(url.query).get("kind", ["spectrum"])[0]
                    self._csv(kind)
                else:
                    self._send(404, b"not found", "text/plain")

            def do_POST(self) -> None:
                # application/json from a foreign page would need a CORS preflight, which we never grant.
                ctype = self.headers.get("Content-Type", "").split(";")[0]
                known = ("/api/config", "/api/cmd", "/api/prefs", "/api/reset") + (
                    ("/api/gen",) if server.fake else ()
                )
                if self.path not in known or ctype != "application/json":
                    self._send(400, b"bad request", "text/plain")
                    return
                try:
                    limit = 16384 if self.path == "/api/prefs" else 4096
                    length = min(int(self.headers.get("Content-Length", "0")), limit)
                    patch = json.loads(self.rfile.read(length) or b"{}")
                    if not isinstance(patch, dict):
                        raise ValueError
                except ValueError:
                    self._send(400, b"bad json", "text/plain")
                    return
                if self.path == "/api/gen":
                    spec = server.fake.set_signal(patch)
                    server.prefs.set("gen", spec)
                    self._gen(spec)
                    return
                if self.path == "/api/prefs":
                    ui, theme = patch.get("ui"), patch.get("theme")
                    if isinstance(ui, dict):
                        server.prefs.set("ui", ui)
                    if theme in ("auto", "light", "dark"):
                        server.prefs.set("theme", theme)
                    self._send(200, b"{}", "application/json")
                    return
                if self.path == "/api/reset":
                    settings = server.analyzer.reset()
                    server.prefs.clear()
                    self._send(200, json.dumps(settings).encode(), "application/json")
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
                server.prefs.set("settings", server.analyzer.persistent())
                self._send(200, json.dumps(settings).encode(), "application/json")

            def _gen(self, spec: dict) -> None:
                body = {
                    "spec": spec,
                    "presets": list(GEN_PRESETS),
                    "kinds": list(GEN_KINDS),
                    "max": GEN_TONES_MAX,
                }
                self._send(200, json.dumps(body).encode(), "application/json")

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

            def _audio(self, query: str) -> None:
                """The samples as they arrive, for the page's player (records of `AUDIO`, no end): raw, or with
                `?filtered=1` after the page's digital filter (as it is set at each moment)."""
                stream = server.analyzer.stream
                filtered = AudioFilter(server.analyzer) if "filtered=1" in query else None
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Cache-Control", "no-store")
                self.send_header("X-Content-Type-Options", "nosniff")
                self.end_headers()
                q = stream.audio.subscribe()
                try:
                    while not server.stop_flag.is_set():
                        try:
                            items = [q.get(timeout=1.0)]
                        except (
                            queue.Empty
                        ):  # nothing came: a heartbeat, so the page knows we are alive
                            self.wfile.write(AUDIO.pack(stream.rate_mhz, AUDIO_IDLE, 0))
                            self.wfile.flush()
                            continue
                        time.sleep(0.01)  # let a few datagrams pile up: fewer, larger records
                        while True:
                            try:
                                items.append(q.get_nowait())
                            except queue.Empty:
                                break
                        for rate, gap, body in join_samples(items):
                            out, fresh = filtered.run(rate, gap, body) if filtered else (None, gap)
                            if out is None:
                                self.wfile.write(audio_record(rate, gap or fresh, body))
                            else:
                                self.wfile.write(audio_record(rate, gap or fresh, out, True))
                        self.wfile.flush()
                except OSError:
                    pass  # the page went away
                finally:
                    stream.audio.unsubscribe(q)

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


# The signal generator of the simulated board: up to eight components (added and removed in the page), noise, mains hum, an offset.
GEN_KINDS = ("sine", "square", "pwm", "sawtooth", "triangle", "sweep", "noise")
GEN_TONES_MAX = 8  # and the most the page can add
# field: (lowest, highest) of a component
GEN_TONE_LIMITS = {
    "freq": (0.1, 200000.0),
    "freq2": (0.1, 200000.0),
    "amp": (0.0, 4000.0),
    "duty": (1.0, 99.0),
    "period": (0.1, 60.0),
}
GEN_GLOBAL_LIMITS = {"dc": (-2200.0, 2200.0), "noise": (0.0, 1000.0), "hum": (0.0, 1500.0)}


def gen_tone(kind: str = "sine", freq: float = 1000.0, amp: float = 800.0, on: bool = True, **more):
    """One component: `amp` is the peak in ADC codes (the rms for `noise`); `duty` (% of the period at the
    top) is for `pwm`; `phase` in degrees; `sweep` goes from `freq` to `freq2` in `period` seconds.
    """
    tone = {
        "on": on,
        "kind": kind,
        "freq": freq,
        "amp": amp,
        "duty": 50.0,
        "phase": 0.0,
        "freq2": 5000.0,
        "period": 2.0,
    }
    tone.update(more)
    return tone


def gen_spec(*tones: dict, dc=0.0, noise=0.0, hum=0.0, mains=50, breathe=False) -> dict:
    """A generator setting: the given components and the global parts."""
    return {
        "tones": list(tones),
        "dc": dc,
        "noise": noise,
        "hum": hum,
        "mains": mains,
        "breathe": breathe,
    }


GEN_PRESETS: dict[str, dict] = {
    "Demo mix": gen_spec(
        gen_tone(freq=1000, amp=800),
        gen_tone(freq=3300, amp=200),
        noise=8,
        hum=150,
        breathe=True,
    ),
    "Perfect sine": gen_spec(gen_tone(freq=1000, amp=1000)),
    "Noisy sine": gen_spec(gen_tone(freq=1000, amp=800), noise=60),
    "Square": gen_spec(gen_tone("square", 1000, 1000), noise=3),
    "PWM 20 %": gen_spec(gen_tone("pwm", 1000, 1000, duty=20.0), noise=3),
    "Sawtooth": gen_spec(gen_tone("sawtooth", 1000, 1000), noise=3),
    "Triangle": gen_spec(gen_tone("triangle", 1000, 1000), noise=3),
    "Square from 4 sines": gen_spec(  # the first odd harmonics of a square wave: 4/pi * 1/k
        *(gen_tone(freq=1000 * k, amp=1000 * 4 / math.pi / k) for k in (1, 3, 5, 7)), noise=3
    ),
    "Two tones (beats)": gen_spec(
        gen_tone(freq=1000, amp=700), gen_tone(freq=1100, amp=700), noise=3
    ),
    "Chord (A4 C#5 E5)": gen_spec(
        gen_tone(freq=440, amp=500), gen_tone(freq=554.37, amp=500), gen_tone(freq=659.25, amp=500)
    ),
    "Sweep 100 Hz - 10 kHz": gen_spec(gen_tone("sweep", 100, 1000, freq2=10000.0, period=2.0)),
    "White noise": gen_spec(gen_tone("noise", 0, 300)),
    "Overloaded sine": gen_spec(gen_tone(freq=1000, amp=3000)),
    "Mains hum": gen_spec(hum=400, noise=5),
}


def gen_signal(spec: dict, t: np.ndarray, rng: np.random.Generator) -> np.ndarray:
    """The simulated input, in ADC codes around the zero code, at the instants `t` (seconds).

    It is an ideal analog signal sampled as it is, with no anti-alias filter, like the real ADC: a
    component above half the sample rate folds back, and so do the harmonics of a square wave."""
    volts = np.zeros(len(t))
    for tn in spec["tones"]:
        if not tn["on"] or tn["amp"] <= 0:
            continue
        kind, f = tn["kind"], tn["freq"]
        if kind == "noise":
            volts += tn["amp"] * rng.normal(0, 1, len(t))
            continue
        if kind == "sweep":
            tau = t % tn["period"]
            phase = f * tau + (tn["freq2"] - f) * tau**2 / (2 * tn["period"])
            volts += tn["amp"] * np.sin(2 * np.pi * (phase + tn["phase"] / 360))
            continue
        p = (f * t + tn["phase"] / 360) % 1.0
        if kind == "sine":
            w = np.sin(2 * np.pi * p)
        elif kind == "square":
            w = np.where(p < 0.5, 1.0, -1.0)
        elif kind == "pwm":
            w = np.where(p < tn["duty"] / 100, 1.0, -1.0)
        elif kind == "sawtooth":
            w = 2 * p - 1
        else:  # triangle
            w = 1 - 4 * np.abs(p - 0.5)
        volts += tn["amp"] * w
    if spec["breathe"]:
        volts *= FakeBoard.gain(t)
    if spec["hum"]:
        f = 50.04 if spec["mains"] == 50 else 60.05  # the mains is never exactly 50 or 60
        volts += spec["hum"] * (
            np.sin(2 * np.pi * f * t + 0.7)
            + 0.4 * np.sin(2 * np.pi * 3 * f * t + 1.9)
            + 0.2 * np.sin(2 * np.pi * 5 * f * t + 0.3)
        )
    if spec["noise"]:
        volts += rng.normal(0, spec["noise"], len(t))
    return volts + spec["dc"]


def _number(v, lo: float, hi: float) -> float | None:
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v):
        return None
    return float(min(max(v, lo), hi))


def sanitize_gen(patch: dict, base: dict) -> dict:
    """`base` with the valid parts of `patch` applied: a preset by name, or any of the global values
    and `tones` (a list, component by component, each a partial dict). Anything else is ignored."""
    if patch.get("preset") in GEN_PRESETS:
        return copy.deepcopy(GEN_PRESETS[patch["preset"]])
    out = copy.deepcopy(base)
    for key, (lo, hi) in GEN_GLOBAL_LIMITS.items():
        v = _number(patch.get(key), lo, hi)
        if v is not None:
            out[key] = v
    if patch.get("mains") in (50, 60):
        out["mains"] = patch["mains"]
    if isinstance(patch.get("breathe"), bool):
        out["breathe"] = patch["breathe"]
    if isinstance(patch.get("tones"), list):
        # The list is the new set of components, in order: shorter removes, longer adds (a new one starts from
        # a default sine). Each entry changes the component at its place, so a partial entry is fine.
        tones = []
        for i, new in enumerate(patch["tones"][:GEN_TONES_MAX]):
            known = i < len(out["tones"])
            tone = out["tones"][i] if known else gen_tone(freq=1000.0, amp=300.0)
            if not isinstance(new, dict):
                if known:
                    tones.append(tone)
                continue
            if isinstance(new.get("on"), bool):
                tone["on"] = new["on"]
            if new.get("kind") in GEN_KINDS:
                tone["kind"] = new["kind"]
            for key, (lo, hi) in GEN_TONE_LIMITS.items():
                v = _number(new.get(key), lo, hi)
                if v is not None:
                    tone[key] = v
            v = _number(new.get("phase"), -3600.0, 3600.0)
            if v is not None:
                tone["phase"] = v % 360
            tones.append(tone)
        out["tones"] = tones
    return out


class FakeBoard:
    """Speaks the firmware's protocol on `127.0.0.1`: 1 kHz and 3.3 kHz tones plus noise, or whatever
    `set_signal` asks for (see `gen_signal`).

    `drop_every=N` skips every Nth datagram (a lost one), to exercise the gap handling. `hum` adds mains
    hum (50.04 Hz and two odd harmonics) of that amplitude in codes, for the demo of the hum removal.
    `breathe` makes the amplitude swell and burst over time (see `gain`), for the demo of the autoscale.
    `wired` is the breadboard of the docs: the generator's pin is wired to the ADC input (what the generator makes
    is the signal while it is on) and a potentiometer on GPIO44 (`knob`, 0 to 1: it wanders unless a test sets it).
    """

    def __init__(
        self, drop_every: int = 0, hum: float = 0.0, breathe: bool = False, wired: bool = False
    ):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.settimeout(0.01)
        self.port = self.sock.getsockname()[1]
        self.drop_every = drop_every
        self.signal = gen_spec(
            gen_tone(freq=1000, amp=800),
            gen_tone(freq=3300, amp=200),
            noise=8,
            hum=hum,
            breathe=breathe,
        )
        self.verbose = False
        self.reboots = 0
        self.pwm = {
            "on": False,
            "freq": 1000.0,
            "duty": 50.0,
        }  # the generator on GPIO20 (the signal only when `wired`)
        self.wired = wired
        self.knob: float | None = (
            None  # a fixed position of the potentiometer; None: it wanders slowly
        )
        self.pot = {"mode": "off", "freq": (100.0, 5000.0), "duty": (0.0, 100.0)}
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

    def set_signal(self, patch: dict) -> dict:
        """Changes the simulated signal (see `sanitize_gen`); returns the new setting."""
        self.signal = sanitize_gen(patch, self.signal)
        return self.signal

    @staticmethod
    def gain(t: np.ndarray) -> np.ndarray:
        """The demo's amplitude at stream time `t` (s): a slow swell between 25 % and 100 % every 14 s,
        and a loud burst (x1.8) for 1.5 s every 20 s, to watch the autoscale follow and settle."""
        swell = 0.625 + 0.375 * np.sin(2 * np.pi * t / 14.0 - np.pi / 2)
        return swell * np.where(t % 20.0 < 1.5, 1.8, 1.0)

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
        elif words[0] == "pwm":
            self._pwm_command(words[1:], reply)
        elif words[0] == "pot":
            self._pot_command(words[1:], reply)
        elif words[0] == "reboot":
            reply("rebooting…")
            self.reboots += 1
            self.verbose = False
            self.pwm["on"] = False
            self.pot["mode"] = "off"
            return True
        elif words[0] == "help":
            reply(
                "commands: info · verbose [on|off] · pwm [freq=HZ] [duty=%] [on|off] · "
                "pot [off|freq|duty] [min=N] [max=N] · reboot · help"
            )
        else:
            reply(f"{' '.join(words)!r}: not a command (try `help`)", 2)
        return False

    def _pwm_command(self, args: list[str], reply) -> None:
        """The firmware's `pwm [freq=HZ] [duty=%] [on|off]` (or `pwm 1000 25`), without the hardware."""
        freq, duty, on, bare = None, None, None, 0
        for word in args:
            key, eq, value = word.partition("=")
            try:
                if word in ("on", "off"):
                    on = word == "on"
                elif eq and key in ("freq", "f", "hz"):
                    freq = float(value)
                elif eq and key in ("duty", "d"):
                    duty = float(value)
                elif not eq and bare < 2:
                    freq, duty = (float(word), duty) if bare == 0 else (freq, float(word))
                    bare += 1
                else:
                    raise ValueError
            except ValueError:
                reply("pwm [freq=HZ] [duty=%] [on|off] (or `pwm 1000 25`)", 2)
                return
        changes = freq is not None or duty is not None
        if changes or on is not None:
            freq = self.pwm["freq"] if freq is None else freq
            if not 0.2 <= freq <= 1e6:
                reply("pwm: frequency out of range (0.2 Hz to 1000 kHz)", 3)
                return
            self.pwm["freq"] = freq
            if duty is not None:
                self.pwm["duty"] = min(max(duty, 0.0), 100.0)
            self.pwm["on"] = on if on is not None else True
        p = self.pwm
        if p["on"]:
            reply(f"pwm on · {p['freq']:.4f} Hz · duty {p['duty']:.3f} % · GPIO20 · simulated")
        else:
            reply(f"pwm off · GPIO20 low · set to {p['freq']:.4f} Hz, duty {p['duty']:.3f} %")

    def _pot_command(self, args: list[str], reply) -> None:
        """The firmware's `pot [off|freq|duty] [min=N] [max=N]`, without the hardware."""
        mode, low, high = None, None, None
        for word in args:
            key, eq, value = word.partition("=")
            try:
                if word in POT_MODES:
                    mode = word
                elif eq and key == "min":
                    low = float(value)
                elif eq and key == "max":
                    high = float(value)
                else:
                    raise ValueError
            except ValueError:
                reply("pot [off|freq|duty] [min=N] [max=N] (Hz for freq, % for duty)", 2)
                return
        now = mode or self.pot["mode"]
        if (low is not None or high is not None) and now == "off":
            reply("pot: pick freq or duty before a range", 3)
            return
        if low is not None or high is not None:
            key = "duty" if now == "duty" else "freq"
            old = self.pot[key]
            new = (old[0] if low is None else low, old[1] if high is None else high)
            floor, ceiling = (0.0, 100.0) if key == "duty" else (PWM_MIN_HZ, PWM_MAX_HZ)
            if not all(floor <= v <= ceiling for v in new):
                reply(
                    "pot: duty is 0 to 100 %" if key == "duty" else "pot: frequency out of range", 3
                )
                return
            if new[0] == new[1]:
                reply("pot: min and max are the same", 3)
                return
            self.pot[key] = new
        if mode is not None:
            self.pot["mode"] = mode
            if mode != "off":
                self.pwm["on"] = True
        p = self.pot
        if p["mode"] == "off":
            reply("pot off · GPIO44 not read · `pot freq` or `pot duty` turns it on")
        elif p["mode"] == "freq":
            reply(
                f"pot freq · {p['freq'][0]:g} to {p['freq'][1]:g} Hz · GPIO44 · "
                f"the duty stays {self.pwm['duty']:.1f} %"
            )
        else:
            reply(
                f"pot duty · {p['duty'][0]:g} to {p['duty'][1]:g} % · GPIO44 · "
                f"the frequency stays {self.pwm['freq']:.1f} Hz"
            )

    def knob_position(self) -> float:
        """Where the simulated potentiometer is: fixed by `knob`, or wandering between 10 % and 90 %."""
        if self.knob is not None:
            return self.knob
        return 0.5 + 0.4 * math.sin(2 * math.pi * (time.monotonic() - self.t0) / 24.0)

    def _follow_knob(self) -> float:
        """What the firmware does a few times a second: the generator takes the knob's value. Returns the position."""
        position = self.knob_position()
        mode = self.pot["mode"]
        if mode != "off" and self.pwm["on"]:
            if mode == "freq":
                low, high = self.pot["freq"]
                self.pwm["freq"] = low * (high / low) ** position
            else:
                low, high = self.pot["duty"]
                self.pwm["duty"] = low + (high - low) * position
        return position

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
        next_pot = 0.0
        running_pot = False  # whether the conversions in progress include the potentiometer's pin
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
                    pot_on = self.pot["mode"] != "off"
                    if pot_on:
                        new = (min(new[0], POT_MAX_RATE_HZ), new[1])
                    if (addr, new) != (client, request) or pot_on != running_pot:
                        running_pot = pot_on
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
                    reply += f" pwm={int(self.pwm['on'])} pwm_hz={self.pwm['freq']:.4f} pwm_duty={self.pwm['duty']:.3f} pwm_pin=20"
                    low, high = self.pot["duty" if self.pot["mode"] == "duty" else "freq"]
                    reply += f" pot={self.pot['mode']} pot_pin=44 pot_lo={low:g} pot_hi={high:g}"
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
            if running_pot and time.monotonic() >= next_pot:
                next_pot = time.monotonic() + 0.1
                position = self._follow_knob()
                given = self.pwm["duty" if self.pot["mode"] == "duty" else "freq"]
                code = ZERO_CODE * (
                    1 - position
                )  # GPIO44 is an even channel: 2198 at the GND end, 0 at 3V3
                self.sock.sendto(
                    POT.pack(
                        POT_MAGIC,
                        POT_MODES.index(self.pot["mode"]),
                        round(position * 10000),
                        round(code),
                        given,
                    ),
                    client,
                )
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
                if self.wired and self.pwm["on"]:
                    # GPIO20 wired to GPIO43: a 0 to 3.3 V rectangle (0 V is ZERO_CODE), with a little noise.
                    half = HALF_SCALE / 2
                    wave = gen_tone("pwm", self.pwm["freq"], half, duty=self.pwm["duty"])
                    volts = gen_signal(gen_spec(wave, dc=half, noise=3), t, rng)
                else:
                    volts = gen_signal(self.signal, t, rng)
                codes = np.clip(ZERO_CODE + volts, 0, FULL_SCALE).astype("<u2")
                index += count
                due += count / rate_hz
                seq += 1
                if self.drop_every and seq % self.drop_every == 0:
                    continue
                head = HEADER.pack(MAGIC, seq - 1, rate_hz * 1000, 0, count, 43)
                self.sock.sendto(head + codes.tobytes(), client)


# --------------------------------------------------------------------------------------------


def lan_address() -> str | None:
    """This computer's address on the network used by default (Wi-Fi at home). A UDP "connect" only picks the
    route: nothing is sent."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("10.255.255.255", 1))
        return s.getsockname()[0]
    except OSError:
        return None
    finally:
        s.close()


def selftest() -> int:
    """Whole chain on this computer: a simulated board, the receiver, the FFT, the web server."""
    failures = []

    def check(ok: bool, what: str) -> None:
        print(("ok    " if ok else "FAIL  ") + what)
        if not ok:
            failures.append(what)

    g = FakeBoard.gain(np.linspace(0, 40, 4001))
    check(0.2 < g.min() < 0.3 and g.max() > 1.6, "the demo's amplitude swells and bursts")

    # The generator: each kind has the right shape, the right level, and folds back above half the rate.
    gt = np.arange(40000) / 40000.0  # 1 s at 40 kS/s
    rg = np.random.default_rng(0)

    def shape(**kw) -> np.ndarray:
        tone = gen_tone(**kw)
        return gen_signal(gen_spec(tone), gt, rg)

    sq = shape(kind="square", freq=100, amp=500)
    check(set(np.unique(sq)) == {-500.0, 500.0}, "square: two levels")
    check(
        abs(float(shape(kind="pwm", freq=100, amp=500, duty=25.0).mean()) + 250) < 5,
        "PWM 25 %: mean",
    )
    saw = shape(kind="sawtooth", freq=100, amp=500)
    check(abs(float(saw.max()) - 500) < 6 and abs(float(saw.min()) + 500) < 6, "sawtooth: +-amp")
    tri = shape(kind="triangle", freq=100, amp=500)
    check(
        abs(float(tri.max()) - 500) < 6 and abs(float(tri.mean())) < 2, "triangle: +-amp, zero mean"
    )
    sin = shape(freq=1000, amp=800)
    check(abs(float(np.sqrt((sin**2).mean())) - 800 / math.sqrt(2)) < 2, "sine: rms = amp/sqrt(2)")
    al = shape(freq=30000, amp=800)  # fs = 40 kHz: 30 kHz folds to 10 kHz
    fa, da = spectrum(ZERO_CODE + al, 40000.0, "hann")
    check(
        abs(peak(fa, da, 1.0)[0] - 10000) < 20,
        "a tone above half the rate folds back (30 kHz -> 10 kHz)",
    )
    sw = shape(kind="sweep", freq=100, amp=500, freq2=5000.0, period=1.0)
    check(abs(float(np.abs(sw).max()) - 500) < 1, "sweep: constant amplitude")
    nz = shape(kind="noise", freq=0, amp=300)
    check(abs(float(nz.std()) - 300) < 10, "noise component: rms = amp")
    check(
        len(GEN_PRESETS) >= 10
        and all(len(p["tones"]) <= GEN_TONES_MAX for p in GEN_PRESETS.values()),
        "presets are complete",
    )
    base = GEN_PRESETS["Perfect sine"]
    # Components come and go: the list sent is the list kept.
    two = sanitize_gen(
        {"tones": [{"freq": 440}, {"kind": "square", "freq": 880, "amp": 100}]}, base
    )
    check(
        [x["freq"] for x in two["tones"]] == [440, 880] and two["tones"][1]["kind"] == "square",
        "a longer list adds components",
    )
    one = sanitize_gen({"tones": [{"freq": 440}]}, two)
    check(
        len(one["tones"]) == 1 and len(sanitize_gen({"tones": []}, one)["tones"]) == 0,
        "a shorter one removes them",
    )
    many = sanitize_gen({"tones": [{}] * 20}, base)
    check(len(many["tones"]) == GEN_TONES_MAX, f"at most {GEN_TONES_MAX} components")
    silent = sanitize_gen({"tones": []}, base)
    check(
        gen_signal(silent, np.arange(100) / 1e4, np.random.default_rng(1)).std() == 0,
        "no component, no signal (noise, offset and hum are separate)",
    )
    bad = sanitize_gen(
        {
            "noise": "loud",
            "dc": 1e9,
            "mains": 55,
            "tones": [{"kind": "tuba", "freq": -5, "amp": float("nan")}, 3],
        },
        base,
    )
    check(
        bad["noise"] == 0
        and bad["dc"] == 2200
        and bad["mains"] == 50
        and bad["tones"][0]["kind"] == "sine"
        and bad["tones"][0]["freq"] == 0.1
        and bad["tones"][0]["amp"] == 1000,
        "bad generator values are clamped or ignored",
    )
    check(
        sanitize_gen({"preset": "Square"}, base)["tones"][0]["kind"] == "square", "preset by name"
    )

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

    check(get("/api/gen")[0] == 404, "no generator without a simulated board")
    server.fake = board
    st, body = get("/api/gen")
    check(st == 200 and json.loads(body)["spec"]["tones"][0]["freq"] == 1000, "GET /api/gen")
    c = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    c.request(
        "POST", "/api/gen", json.dumps({"preset": "Square"}), {"Content-Type": "application/json"}
    )
    r = c.getresponse()
    out = json.loads(r.read())
    check(
        r.status == 200 and out["spec"]["tones"][0]["kind"] == "square", "POST /api/gen: a preset"
    )
    check(board.signal["tones"][0]["kind"] == "square", "the simulated board follows")
    board.set_signal({"preset": "Demo mix"})
    board.set_signal({"hum": 0, "breathe": False, "noise": 8})

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
    # The audio feed (the page's player): raw samples, in order, at the board's exact rate.
    audio = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    audio.request("GET", "/audio")
    audio_resp = audio.getresponse()
    heard: list[np.ndarray] = []
    heard_rates, heard_gaps = set(), 0
    until = time.monotonic() + 5
    while sum(len(a) for a in heard) < 4000 and time.monotonic() < until:
        head = audio_resp.read(AUDIO.size)
        if len(head) < AUDIO.size:
            break
        rate_mhz, flags, count = AUDIO.unpack(head)
        body = audio_resp.read(2 * count)
        if count:
            heard.append(np.frombuffer(body, dtype="<u2"))
            heard_rates.add(rate_mhz)
            heard_gaps += bool(flags & AUDIO_GAP)
    audio.close()
    codes = np.concatenate(heard) if heard else np.zeros(0)
    check(
        audio_resp.status == 200 and len(codes) >= 4000 and heard_rates == {20_000_000},
        f"GET /audio: {len(codes)} samples in {len(heard)} records at {sorted(heard_rates)} mHz",
    )
    check(heard_gaps <= 1, f"and in one piece ({heard_gaps} gap flagged, the start at most)")
    check(
        len(codes) > 0 and abs(codes.mean() - ZERO_CODE) < 60 and abs(codes.std() - 598) < 80,
        f"the codes are the signal's (mean {codes.mean():.0f}, rms {codes.std():.0f})",
    )
    # The same feed after the page's digital filter: float32 samples, the tone kept or removed.
    lp = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    server.analyzer.configure({"filt": "highpass", "filt_f1": 3000.0, "filt_sharp": "soft"})
    lp.request("GET", "/audio?filtered=1")
    lresp = lp.getresponse()
    rms_hp: list[float] = []
    until = time.monotonic() + 5
    while len(rms_hp) < 40 and time.monotonic() < until:
        head = lresp.read(AUDIO.size)
        if len(head) < AUDIO.size:
            break
        rate_mhz, flags, count = AUDIO.unpack(head)
        body = lresp.read((4 if flags & AUDIO_F32 else 2) * count)
        if count and flags & AUDIO_F32:
            rms_hp.append(float(np.frombuffer(body, dtype="<f4").std()))
    lp.close()
    server.analyzer.configure({"filt": "off"})
    left = float(np.median(rms_hp[5:])) if len(rms_hp) > 5 else -1.0
    check(
        len(rms_hp) >= 10 and left < 300,
        f"/audio?filtered=1 sends float32 after the filter (a 3 kHz high-pass leaves {left:.0f} rms of the 1 kHz tone)",
    )
    flt = AudioFilter(server.analyzer)
    check(
        flt.run(20_000_000, False, b"\x00\x08" * 10)[0] is None,
        "filter off: the raw samples go as they are",
    )
    joined = coalesce(
        [(1000, False, b"ab"), (1000, False, b"cd"), (1000, True, b"ef"), (2000, False, b"gh")]
    )
    check(
        [AUDIO.unpack_from(r) for r in joined] == [(1000, 0, 2), (1000, AUDIO_GAP, 1), (2000, 0, 1)]
        and joined[0].endswith(b"abcd"),
        "a burst is joined into one record, a gap or a new rate starts another",
    )
    hub = AudioHub()
    backlog = hub.subscribe()
    for i in range(600):
        hub.publish(1000, False, bytes([i % 256, 0]))
    first = backlog.get_nowait()
    check(
        backlog.qsize() < 512 and first[1] is True and hub.count() == 1,
        "a page that falls behind loses its backlog and is told (a gap)",
    )
    hub.unsubscribe(backlog)

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
    found = find_trigger(sine, 400, pos=0.0)
    i = found[0] if found else -1
    check(
        found is not None
        and i > 0
        and sine[i - 1] < sine[i]
        and sine[i] > sine.mean()
        and sine[i - 1] < sine.mean() + 200,
        f"the trigger finds a rising edge through the middle (sample {i})",
    )
    fall = find_trigger(sine, 400, slope="fall", pos=0.0)
    check(
        fall is not None and sine[fall[0]] <= sine[fall[0] - 1] < sine.mean() + 300,
        "falling slope: the view starts on a falling edge",
    )
    up = find_trigger(sine, 400, level=50, pos=0.0)
    check(
        up is not None
        and abs(up[1] - (sine.mean() + 400)) < 40
        and sine[up[0]] >= up[1] > sine[up[0] - 1],
        "level +50 %: the edge crossing half of the upper half-swing",
    )
    mid = find_trigger(sine, 400, pos=0.5)
    c = mid[0] + round(0.5 * 399) if mid else 0
    check(
        mid is not None and mid[0] >= 0 and sine[c] >= mid[1] > sine[c - 1],
        "position 50 %: the crossing is in the middle of the view",
    )
    noise = np.random.default_rng(0).normal(0, 1, 2000)
    check(find_trigger(np.zeros(2000), 400) is None, "a flat signal never triggers")
    check(find_trigger(sine[:300], 400) is None, "too few samples: no trigger")
    # Noise around the level mustn't trigger on every wiggle: crossings are spaced by the hysteresis.
    nz = find_trigger(ZERO_CODE + 100 * np.sin(2 * np.pi * 1000 * t) + noise, 400, pos=0.0)
    check(nz is not None, "noise on the signal still triggers once per period")
    # Sub-sample view: a sine seen from a fractional start is the sine shifted, not a blurred copy.
    tt = np.arange(2000) / 20000
    for fr in (0.0, 0.3, 0.5, 0.9):
        v = view_at(np.sin(2 * np.pi * 3300 * tt), 10 + fr, 300)
        err = np.abs(v - np.sin(2 * np.pi * 3300 * (tt[:300] + (10 + fr) / 20000))).max()
        check(err < 0.01, f"fractional start {fr}: error {err:.4f} on a 3.3 kHz sine")
    f_tone = strongest_tone(ZERO_CODE + 800 * np.sin(2 * np.pi * 1234.5 * tt), 20000)
    check(f_tone is not None and abs(f_tone - 1234.5) < 1.0, f"strongest tone {f_tone} Hz")
    check(
        strongest_tone(np.random.default_rng(2).normal(0, 1, 4096), 20000) is None, "noise: no tone"
    )
    # Several tones and hum: with the lock on a tone and its pattern, consecutive pictures match.
    rng_t = np.random.default_rng(5)

    def lock_pictures(lock: str) -> float:
        last, same = None, 0
        for k in range(40):
            tk = (50000 + 1000 * k + np.arange(8192)) / 20000
            xk = (
                800 * np.sin(2 * np.pi * 1000 * tk)
                + 200 * np.sin(2 * np.pi * 3300 * tk)
                + 150 * np.sin(2 * np.pi * 50.04 * tk + 0.7)
                + rng_t.normal(0, 8, tk.size)
            )
            seg_k = xk[-1200:]
            if lock == "edge":
                fd = find_trigger(seg_k, 400, 0, "rise", 0.2)
                v = view_at(seg_k, fd[2], 400) if fd else None
            else:
                cand = tone_crossings(seg_k, 20000, strongest_tone(xk, 20000), 400, 0, "rise", 0.2)
                v = pick_view(seg_k, cand[0], 400, last if lock == "pattern" else None)[1]
            if v is None:
                continue
            v = v - v.mean()
            if last is not None and np.corrcoef(v, last)[0, 1] > 0.98:
                same += 1
            last = v
        return same / 39

    for lock, want in (("edge", 0.0), ("tone", 0.0), ("pattern", 0.95)):
        got = lock_pictures(lock)
        check(
            got >= want if want else True,
            f"trigger lock {lock}: {got * 100:.0f} % of the pictures alike",
        )
    post(
        {
            "trigger": True,
            "trig_slope": "fall",
            "trig_level": -30,
            "trig_pos": 50,
            "trig_mode": "normal",
        }
    )
    f = next_frame(
        resp,
        lambda f: "scope" in f
        and f["settings"]["trig_pos"] == 50
        and f["scope"].get("trig", {}).get("state") == "triggered",
    )
    check(
        f is not None
        and f["settings"]["trig_slope"] == "fall"
        and f["settings"]["trig_pos"] == 50
        and f["scope"]["trig"]["i"] == 50,
        "frames carry the trigger state, marker position and level",
    )
    check(
        post({"trig_level": 900, "trig_pos": -3, "trig_slope": "sideways"})[0] == 200
        and json.loads(post({})[1])["trig_level"] == -30,
        "invalid trigger settings are ignored",
    )
    # Mains hum removal, FIR filters, histogram of the codes.
    tt = np.arange(16000) / 20000
    hum = 300 * np.sin(2 * np.pi * 50.07 * tt + 0.4) + 120 * np.sin(2 * np.pi * 150.21 * tt + 1.1)
    tone = 200 * np.sin(2 * np.pi * 1000 * tt)
    clean, f0 = remove_hum(ZERO_CODE + hum + tone, 20000.0, 50.0)
    resid = (clean - ZERO_CODE) - tone
    check(abs(f0 - 50.07) < 0.01, f"hum removal finds the mains frequency ({f0:.3f} Hz for 50.07)")
    check(
        np.std(resid) < 0.03 * np.std(hum) and abs(clean.mean() - ZERO_CODE) < 2,
        f"hum removal: 50 Hz + harmonics down {20 * np.log10(np.std(hum) / np.std(resid)):.0f} dB, tone and DC kept",
    )
    # FIR filters: gains at known frequencies, no edge effects, delay of half the length.
    fsr = 20000.0
    tq = np.arange(40000) / fsr
    two = 1000 * np.sin(2 * np.pi * 1000 * tq) + 1000 * np.sin(2 * np.pi * 3300 * tq)

    def gain_db(h: np.ndarray, f: float) -> float:
        return float(
            20 * np.log10(abs(np.sum(h * np.exp(-2j * np.pi * f / fsr * np.arange(len(h))))))
        )

    taps = fir_taps("medium", fsr)
    lp = fir_design("lowpass", 2000, 0, fsr, taps)
    check(
        abs(gain_db(lp, 1000)) < 0.5 and gain_db(lp, 3300) < -60,
        f"low-pass 2 kHz: 1 kHz at {gain_db(lp, 1000):.2f} dB, 3.3 kHz at {gain_db(lp, 3300):.0f} dB",
    )
    hp = fir_design("highpass", 2000, 0, fsr, taps)
    check(gain_db(hp, 1000) < -60 and abs(gain_db(hp, 3300)) < 0.5, "high-pass 2 kHz")
    bp = fir_design("bandpass", 800, 1200, fsr, taps)
    check(
        abs(gain_db(bp, 1000)) < 1 and gain_db(bp, 3300) < -60 and gain_db(bp, 100) < -60,
        "band-pass 0.8 to 1.2 kHz",
    )
    bs = fir_design("bandstop", 3000, 3600, fsr, fir_taps("sharp", fsr))
    check(gain_db(bs, 3300) < -60 and abs(gain_db(bs, 1000)) < 0.5, "band-stop around 3.3 kHz")
    yf = fir_valid(two, lp)
    check(
        len(yf) == len(two) - taps + 1 and abs(np.std(yf) - 1000 / 2**0.5) < 15,
        "FIR output has no edge effect: only the 1 kHz tone is left, full amplitude",
    )
    check(
        np.allclose(response_db(lp, 4096)[205], gain_db(lp, 205 * fsr / 4096), atol=0.01),
        "response_db agrees with the gain at a bin",
    )
    check(
        fir_taps("extreme", 2000.0) <= 501 and fir_taps("soft", 2000.0) >= 15,
        "taps capped for slow rates",
    )
    h = histogram(np.array([10.0, 10, 11, 14, 15]))
    check(
        h["lo"] == 10 and h["step"] == 1 and h["counts"] == [2, 1, 0, 0, 1, 1] and h["clip"] == 0,
        "histogram: one bar per code",
    )
    check(
        histogram(np.array([0.0, 5, 2000, 4392]))["clip"] == 0.5,
        "histogram: share of samples at the range ends",
    )
    check(
        histogram(np.arange(0, 4394.0))["step"] > 1, "histogram groups codes when the range is wide"
    )
    post({"hum": "50", "trigger": False})
    f = next_frame(resp, lambda f: f["settings"]["hum"] == "50" and "scope" in f)
    check(
        f is not None and "dc" in f["scope"] and abs(f["scope"]["dc"] - ZERO_CODE) < 30,
        "frames with the filters on carry the waveform and its DC level",
    )
    check(
        f is not None and len(f["hist"]["counts"]) > 10 and f["hist"]["clip"] == 0,
        "frames carry the code histogram",
    )
    post({"hum": "off"})
    # The filter in the whole chain: spectrum, raw underlay, response, waveform, readouts of the raw signal.
    post({"filt": "lowpass", "filt_f1": 2000, "filt_sharp": "medium", "filt_raw": True, "avg": 1})
    f = next_frame(
        resp, lambda f: f["settings"]["filt"] == "lowpass" and "resp" in f.get("spec", {})
    )
    if f is not None:
        df = f["spec"]["df"]
        y = np.array(f["spec"]["y"])
        raw = np.array(f["spec"]["raw"])
        i33, i1 = round(3300 / df), round(1000 / df)
        check(
            raw[i33 - 2 : i33 + 3].max() - y[i33 - 2 : i33 + 3].max() > 40,
            "the 3.3 kHz line is gone from the filtered spectrum, still there in the raw one",
        )
        check(
            abs(y[i1 - 2 : i1 + 3].max() - raw[i1 - 2 : i1 + 3].max()) < 1,
            "the 1 kHz line is untouched",
        )
        check(
            f["spec"]["resp"][i1] > -1 and f["spec"]["resp"][i33] < -50,
            "response curve: passband and stopband",
        )
        check(
            "raw" in f["scope"] and len(f["scope"]["raw"]) == len(f["scope"]["y"]),
            "the waveform carries its unfiltered twin",
        )
        check(
            f["filter"]["taps"] == 511 and f["filter"]["delay_ms"] == 12.8,
            f"filter facts for the page: {f['filter']}",
        )
        check(
            f["metrics"]["rms"] > 590 - 40,
            f"readouts stay on the raw signal (AC rms {f['metrics']['rms']:.0f})",
        )
    else:
        check(False, "filtered frames arrive")
    post({"filt": "bandstop", "filt_f1": 900, "filt_f2": 1100, "filt_raw": False})
    f = next_frame(
        resp, lambda f: f["settings"]["filt"] == "bandstop" and "resp" in f.get("spec", {})
    )
    check(
        f is not None and "raw" not in f["spec"] and "raw" not in f["scope"],
        "without the comparison, no raw traces are sent",
    )
    check(
        post({"filt": "wobble", "filt_f1": -5, "filt_sharp": "huge"})[0] == 200
        and json.loads(post({})[1])["filt"] == "bandstop",
        "invalid filter settings are ignored",
    )
    post({"filt": "off", "filt_raw": True})
    f = next_frame(resp, lambda f: f["settings"]["filt"] == "off" and "spec" in f)
    check(
        f is not None and "resp" not in f["spec"] and "raw" not in f["spec"],
        "filter off: plain frames",
    )

    # Waveform averaging and envelope, without and with the trigger.
    for trigger in (False, True):
        post({"trigger": trigger, "scope_avg": 8, "scope_hold": True, "reset_hold": True})
        f = next_frame(
            resp,
            lambda f: "min" in f.get("scope", {})
            and f["settings"]["scope_avg"] == 8
            and f["settings"]["trigger"] == trigger,
        )
        sc = f["scope"] if f else {"y": [0], "min": [1], "max": [-1]}
        ok = f is not None and len(sc["min"]) == len(sc["y"]) == len(sc["max"])
        ok = ok and all(
            lo - 1e-6 <= v <= hi + 1e-6 for lo, v, hi in zip(sc["min"], sc["y"], sc["max"])
        )
        check(
            ok,
            f"waveform average x8 + envelope, trigger {'on' if trigger else 'off'}: min <= trace <= max",
        )
    # Sliding window of the envelope: a spike leaves it after the window, never with "forever".
    quiet, spike = np.zeros(8), np.zeros(8)
    spike[3] = 9.0
    for window, gone in ((5.0, True), (0.0, False)):
        env = SlidingExtrema()
        env.push(spike, window, 0.0)
        for k in range(1, 15):
            env.push(quiet, window, float(k))
        lo, hi = env.extrema()
        check(
            (hi[3] == 0.0) == gone and lo.min() == 0.0,
            f"envelope window {window or 'forever'}: a 9-code spike {'left' if gone else 'stays in'} after 14 s",
        )
    env = SlidingExtrema()
    env.push(spike, 10.0, 0.0)
    env.push(quiet, 10.0, 1.0)
    env.push(quiet, 0.0, 2.0)  # switched to forever: the slices merge and keep the spike
    check(
        len(env._slices) == 1 and env.extrema()[1][3] == 9.0,
        "envelope: windowed to forever keeps what was seen",
    )
    post({"scope_hold_s": 7})
    check(json.loads(post({})[1])["scope_hold_s"] == 0, "an unknown envelope window is ignored")
    check(
        json.loads(post({"scope_hold_s": 0.2})[1])["scope_hold_s"] == 0.2,
        "envelope window of 0.2 s",
    )
    post({"scope_hold_s": 5, "trigger": False, "scope_hold": True, "reset_hold": True})
    f = next_frame(
        resp, lambda f: f["settings"]["scope_hold_s"] == 5 and "min" in f.get("scope", {})
    )
    check(f is not None, "frames carry the envelope with a 5 s window")
    post({"scope_hold_s": 0, "scope_hold": False})
    # Averaging frames of a noisy periodic signal that line up (the trigger's job) lowers the noise.
    tt = np.arange(400) / 20000
    frames = [
        100 * np.sin(2 * np.pi * 1000 * tt) + np.random.default_rng(i).normal(0, 30, 400)
        for i in range(16)
    ]
    check(
        np.std(np.mean(frames, axis=0) - 100 * np.sin(2 * np.pi * 1000 * tt))
        < 0.4 * np.std(frames[0] - 100 * np.sin(2 * np.pi * 1000 * tt)),
        "averaging aligned frames lowers the noise",
    )
    post({"scope_avg": 1, "scope_hold": False})
    post(
        {
            "scope_ms": 20,
            "trigger": False,
            "trig_slope": "rise",
            "trig_level": 0,
            "trig_mode": "auto",
        }
    )

    st, body = post({"fft": 2048, "window": "blackman", "avg": 4, "hold": True})
    check(st == 200 and json.loads(body)["fft"] == 2048, "POST /api/config applies settings")
    f = next_frame(
        resp, lambda f: f["settings"]["fft"] == 2048 and "spec" in f and "hold" in f["spec"]
    )
    check(
        f is not None and len(f["spec"]["y"]) == 1025 and len(f["spec"]["hold"]) == 1025,
        "next frames use them (1025 bins, peak hold)",
    )
    app = (f or {}).get("app", {})
    stats = (f or {}).get("stats", {})
    check(
        app.get("demo") is True
        and app.get("pages", 0) >= 1
        and app.get("fps") == 25
        and app.get("uptime", -1) >= 0
        and app["prefs"]["path"] is None,
        "frames say what this tool is doing (pages, fps, uptime, settings file)",
    )
    check(
        0 <= stats.get("age", -1) < 1.0
        and stats.get("dgram", 0) >= 16 + 2 * 256
        and stats["count"] == 256,
        "and about the link (age of the last datagram, its size)",
    )
    check(
        f.get("harm") and f["harm"][0]["k"] == 1 and abs(f["harm"][0]["fa"] - 1000) < 5,
        "frames carry the harmonics of the strongest line",
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

    # The PWM generator: commands, the state in the board's `ok`, refusals, off after a reboot.
    check(command("pwm") == 200, "pwm without argument asks for the state")
    f = next_frame(resp, lambda f: f["stats"]["pwm"] is not None)
    check(
        f is not None and f["stats"]["pwm"]["on"] is False and f["stats"]["pwm"]["pin"] == 20,
        "the board's `ok` reports the generator (off, GPIO20)",
    )
    check(command("pwm freq=2500 duty=30") == 200, "pwm freq=2500 duty=30")
    f = next_frame(resp, lambda f: (f["stats"]["pwm"] or {}).get("on") is True)
    check(
        f is not None and f["stats"]["pwm"]["freq"] == 2500 and f["stats"]["pwm"]["duty"] == 30,
        "the generator is on at 2500 Hz, 30 %, and the page sees it",
    )
    check(saw("pwm on · 2500.0000 Hz"), "the board answers with what it makes")
    check(command("pwm 440 75.5") == 200, "positional arguments: pwm 440 75.5")
    f = next_frame(resp, lambda f: (f["stats"]["pwm"] or {}).get("freq") == 440)
    check(f is not None and f["stats"]["pwm"]["duty"] == 75.5, "440 Hz, 75.5 %")
    check(command("pwm off") == 200, "pwm off")
    f = next_frame(resp, lambda f: (f["stats"]["pwm"] or {}).get("on") is False)
    check(f is not None and f["stats"]["pwm"]["freq"] == 440, "off keeps the settings")
    check(command("pwm on") == 200, "pwm on")
    f = next_frame(resp, lambda f: (f["stats"]["pwm"] or {}).get("on") is True)
    check(f is not None and f["stats"]["pwm"]["duty"] == 75.5, "on resumes with them")
    check(command("pwm freq=9e9") == 200, "an out-of-range frequency reaches the board")
    next_frame(resp, lambda f: saw("frequency out of range", "error"))
    check(saw("frequency out of range", "error"), "and the board refuses it")
    check(
        command("pwm freq=abc") == 400 and command("pwm ; reboot") == 400,
        "pwm arguments are checked before they are sent",
    )

    # The potentiometer: the board's mode and range, the live reading, what it does to the generator.
    def pot(f: dict) -> dict:
        return f["stats"]["pot"] or {}

    board.knob = 0.25
    check(command("pot") == 200, "pot without argument asks for the state")
    f = next_frame(resp, lambda f: pot(f).get("mode") == "off")
    check(
        f is not None and pot(f)["pin"] == 44 and not pot(f).get("live"),
        "the board's `ok` reports the potentiometer (off, GPIO44, no reading)",
    )
    check(command("pwm off") == 200 and command("pot freq min=200 max=2000") == 200, "pot freq")
    f = next_frame(resp, lambda f: pot(f).get("live") and pot(f)["mode"] == "freq")
    check(
        f is not None and (pot(f)["lo"], pot(f)["hi"]) == (200, 2000) and pot(f)["position"] == 25,
        "the page gets the mode, the range and the knob's live position",
    )
    expected = (
        200 * 10**0.25
    )  # a logarithmic range: a quarter of the way up is a quarter of the decades
    check(
        f is not None and abs(pot(f)["value"] - expected) < 0.5,
        f"it gave the generator {expected:.1f} Hz",
    )
    f = next_frame(
        resp,
        lambda f: (f["stats"]["pwm"] or {}).get("on") is True
        and abs(f["stats"]["pwm"]["freq"] - expected) < 0.5,
    )
    check(f is not None, "turning the knob on starts the generator, at the knob's frequency")
    stream.set_rate(60000)
    deadline = time.monotonic() + 5
    while abs(stream.fs - POT_MAX_RATE_HZ) > 1 and time.monotonic() < deadline:
        time.sleep(0.05)
    check(
        abs(stream.fs - POT_MAX_RATE_HZ) <= 1,
        f"the conversions alternate: the signal's rate stops at {POT_MAX_RATE_HZ} Hz",
    )
    stream.set_rate(20000)
    check(command("pot duty min=10 max=60") == 200, "pot duty min=10 max=60")
    f = next_frame(resp, lambda f: pot(f).get("live") and pot(f)["mode"] == "duty")
    check(
        f is not None and pot(f)["value"] == 22.5, "a linear range: a quarter of 10 to 60 is 22.5 %"
    )
    f = next_frame(resp, lambda f: (f["stats"]["pwm"] or {}).get("duty") == 22.5)
    check(
        f is not None and abs(f["stats"]["pwm"]["freq"] - expected) < 0.5,
        "the duty follows the knob, the frequency stays what `pot freq` left",
    )
    board.knob = 1.0
    f = next_frame(resp, lambda f: (f["stats"]["pwm"] or {}).get("duty") == 60)
    check(f is not None, "the end of the travel gives the end of the range")
    check(command("pot freq max=9e9") == 200, "an out-of-range limit reaches the board")
    next_frame(resp, lambda f: saw("frequency out of range", "error"))
    check(saw("frequency out of range", "error"), "and the board refuses it")
    check(
        command("pot freq min=abc") == 400
        and command("pot ; reboot") == 400
        and command("pot fast") == 400,
        "pot arguments are checked before they are sent",
    )
    check(command("pot off") == 200, "pot off")
    f = next_frame(resp, lambda f: pot(f).get("mode") == "off" and not pot(f).get("live"))
    check(f is not None, "off: no more readings")
    check(command("pot freq") == 200, "pot freq again")
    next_frame(resp, lambda f: pot(f).get("mode") == "freq")

    check(command("reboot") == 200, "reboot")
    f = next_frame(
        resp,
        lambda f: board.reboots == 2
        and (f["stats"]["pwm"] or {}).get("on") is False
        and pot(f).get("mode") == "off",
    )
    check(f is not None, "the generator and the potentiometer are off again after a reboot")

    resp.close()
    events.close()
    server.close()
    stream.close()
    board.close()

    # 2a. harmonics of a square wave, folded back when they pass half the sample rate
    def square_spectrum(f0: float, fs: float) -> tuple[np.ndarray, np.ndarray]:
        t = np.arange(16384) / fs
        return spectrum(np.where((t * f0) % 1.0 < 0.5, 1098.0, -1098.0) + 3296, fs, "hann")

    freq, db = square_spectrum(200.0, 40000.0)
    pf, pdb = peak(freq, db, 1.0)
    series = harmonic_series(freq, db, pf, pdb, 40000.0, 32)
    by_k = {h["k"]: h for h in series}
    check(
        len(series) == 32 and not any(h["alias"] for h in series),
        "32 harmonics, none above Nyquist",
    )
    check(
        all(abs(by_k[k]["db"] - (pdb - 20 * math.log10(k))) < 1.5 for k in (3, 5, 9, 15, 25, 31)),
        "odd harmonics of a square wave fall as 1/n: "
        + ", ".join(f"{k}: {by_k[k]['db'] - pdb:.1f}" for k in (3, 5, 9)),
    )
    check(all(by_k[k]["db"] < pdb - 60 for k in (2, 4, 8, 16)), "and the even ones are missing")
    check(abs(by_k[31]["f"] - 6200) < 2, f"f0 follows the harmonics (31st at {by_k[31]['f']} Hz)")
    freq, db = square_spectrum(3000.0, 10000.0)
    pf, pdb = peak(freq, db, 1.0)
    series = harmonic_series(freq, db, pf, pdb, 10000.0, 8)
    check(
        [h["alias"] for h in series] == [False, True, True, True, True, True, True, True]
        and abs(series[2]["fa"] - 1000) < 15,
        "above half the sample rate a harmonic is flagged as folded (9 kHz reads as 1 kHz)",
    )

    # 2b. settings kept between runs: written, read back by the next run, bad files and resets survived
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "sub" / "settings.json"
        board = FakeBoard()
        stream = Stream(("127.0.0.1", board.port), 20000, 256)
        stream.start()
        prefs = Prefs(path)
        server = Server(Analyzer(stream), "127.0.0.1", 0, fps=25, prefs=prefs)
        server.fake = board
        server.start()

        def call(method: str, url: str, body: dict | None = None) -> tuple[int, bytes]:
            c = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
            c.request(
                method,
                url,
                json.dumps(body) if body is not None else None,
                {"Content-Type": "application/json"},
            )
            r = c.getresponse()
            return r.status, r.read()

        check(not path.exists(), "nothing is written before a change")
        call("POST", "/api/config", {"fft": 8192, "window": "blackman", "trig_level": -30})
        call("POST", "/api/config", {"filt": "lowpass", "filt_f1": 1234.5, "paused": True})
        call("POST", "/api/gen", {"preset": "Square"})
        call("POST", "/api/prefs", {"ui": {"palette": "mintrose", "col": {"spec": "#123456"}}})
        call("POST", "/api/prefs", {"theme": "dark", "ui": None})
        check(call("POST", "/api/prefs", {"ui": [1]})[0] == 200, "a bad ui is ignored, not fatal")
        time.sleep(0.9)  # written half a second after the last change
        saved = json.loads(path.read_text()) if path.exists() else {}
        check(
            saved.get("settings", {}).get("fft") == 8192
            and saved["settings"]["window"] == "blackman"
            and saved["settings"]["filt_f1"] == 1234.5
            and saved["settings"]["trig_level"] == -30,
            "the settings are written to the file",
        )
        check("paused" not in saved.get("settings", {}), "the pause is not kept")
        check(
            saved.get("ui", {}).get("palette") == "mintrose" and saved.get("theme") == "dark",
            "the page's view settings and theme are written",
        )
        check(
            saved.get("gen", {}).get("tones", [{}])[0].get("kind") == "square",
            "the demo's signal is written",
        )
        st, html = call("GET", "/")
        check(
            st == 200
            and b"/*PREFS*/" not in html
            and b'"palette": "mintrose"' in html
            and b'"theme": "dark"' in html
            and json.dumps(str(path)).encode() in html,
            "the page is served with the saved settings in it",
        )
        # Tags in the data can't close the script.
        call("POST", "/api/prefs", {"ui": {"x": "</script><b>"}})
        server.prefs.flush(force=True)
        check(b"</script><b>" not in call("GET", "/")[1], "the saved data can't end the script tag")
        call("POST", "/api/prefs", {"ui": {"palette": "mintrose"}})
        server.close()
        stream.close()
        board.close()

        # the next run
        board = FakeBoard()
        stream = Stream(("127.0.0.1", board.port), 20000, 256)
        stream.start()
        again = Analyzer(stream)
        prefs = Prefs(path)
        again.load(prefs.get("settings", {}))
        check(
            again.s["fft"] == 8192
            and again.s["window"] == "blackman"
            and again.s["filt"] == "lowpass",
            "the next run starts from the saved settings",
        )
        check(not again.s["paused"], "and not paused")
        board.set_signal(prefs.get("gen"))
        check(board.signal["tones"][0]["kind"] == "square", "the demo's signal comes back")
        again.load({"fft": 7, "avg": "x", "nonsense": 1, "trig_pos": 500})
        check(
            again.s["fft"] == 8192 and again.s["avg"] == 1 and "nonsense" not in again.s,
            "a file edited by hand can't break the analyzer",
        )

        # a damaged file starts from the defaults, says so, and is replaced on the next change
        path.write_text("{ not json")
        broken = Prefs(path)
        check(
            broken.get("settings") is None and broken.problem is not None,
            "a damaged file is noticed",
        )
        path.write_text(json.dumps({"version": 99, "settings": {"fft": 2048}}))
        check(Prefs(path).get("settings") is None, "a file of another version is ignored")
        path.write_text(
            json.dumps({"version": PREFS_VERSION, "settings": {"fft": 2048}, "junk": 1})
        )
        check(Prefs(path).data == {"settings": {"fft": 2048}}, "unknown sections are dropped")

        # no-save: nothing read, nothing written
        silent = Prefs(None)
        silent.set("ui", {"a": 1})
        check(
            silent.flush(force=True) is None and silent.get("ui") is None, "no path: nothing kept"
        )

        # reset: defaults again, file gone
        server = Server(again, "127.0.0.1", 0, fps=25, prefs=Prefs(path))
        server.start()
        call("POST", "/api/config", {"fft": 16384, "rate": 40000, "hold": True})
        st, body = call("POST", "/api/reset", {})
        out = json.loads(body)
        check(
            st == 200
            and out["fft"] == 4096
            and out["window"] == "hann"
            and out["rate"] == DEFAULT_RATE_HZ
            and out["hold"] is False
            and out["filt"] == "off",
            "reset brings the defaults back",
        )
        check(stream.requested_hz == DEFAULT_RATE_HZ, "and the sample rate with them")
        check(not path.exists(), "and forgets the file")
        server.close()
        check(not path.exists(), "(still nothing at exit)")
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
        default=None,
        metavar="HZ",
        help=f"sample rate, Hz: 611 to 83333 (default: the saved one, else {DEFAULT_RATE_HZ})",
    )
    ap.add_argument(
        "--count", type=int, default=256, help="samples per datagram, 16-700 (default: %(default)s)"
    )
    ap.add_argument(
        "--fft",
        type=int,
        default=None,
        choices=Analyzer.FFT_SIZES,
        help="initial FFT length (default: the saved one, else 4096)",
    )
    ap.add_argument(
        "--avg", type=int, default=None, help="initial averaging, 1 = none (default: the saved one)"
    )
    ap.add_argument(
        "--scope-ms",
        type=int,
        default=None,
        help="time span of the waveform, ms (default: the saved one, else 20)",
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
    ap.add_argument(
        "--lan",
        action="store_true",
        help="also show the page to other devices on your network (a phone on the same Wi-Fi): "
        "same as --host 0.0.0.0, and prints the address to type there",
    )
    ap.add_argument(
        "--settings",
        type=Path,
        metavar="FILE",
        help="where your settings are kept between runs (default: %s)" % default_prefs_path(),
    )
    ap.add_argument(
        "--no-save", action="store_true", help="start from the defaults and keep nothing"
    )
    ap.add_argument(
        "--reset-settings", action="store_true", help="forget the saved settings, then start"
    )
    ap.add_argument("--no-browser", action="store_true", help="don't open the page")
    ap.add_argument("--demo", action="store_true", help="use a simulated board on this computer")
    ap.add_argument(
        "--wire",
        action="store_true",
        help="with --demo: the simulated PWM pin is wired to the input and a potentiometer wanders (docs/book/wiring.md)",
    )
    ap.add_argument(
        "--selftest", action="store_true", help="test receiver, FFT and web server, no browser"
    )
    args = ap.parse_args()

    args.count = min(max(args.count, 16), 700)

    if args.selftest:
        return selftest()
    if args.lan:
        args.host = "0.0.0.0"

    # Settings: the defaults, then what was saved, then what the command line says.
    prefs = Prefs(None if args.no_save else args.settings or default_prefs_path())
    if args.reset_settings:
        prefs.clear()
    chosen = {**prefs.get("settings", {})}
    for key, value in (
        ("rate", args.rate),
        ("fft", args.fft),
        ("avg", args.avg),
        ("scope_ms", args.scope_ms),
    ):
        if value is not None:
            chosen[key] = value
    rate = chosen.get("rate")
    if not isinstance(rate, int) or isinstance(rate, bool):
        rate = DEFAULT_RATE_HZ
    rate = min(max(rate, MIN_RATE_HZ), MAX_RATE_HZ)

    fake = FakeBoard(hum=150.0, breathe=True, wired=args.wire) if args.demo else None
    if fake and isinstance(prefs.get("gen"), dict):
        fake.set_signal(prefs.get("gen"))
    board = ("127.0.0.1", fake.port) if fake else (args.board, args.port)
    stream = Stream(board, rate, args.count)
    stream.start()
    analyzer = Analyzer(stream)
    analyzer.load(chosen)
    server = Server(analyzer, args.host, args.http_port, args.fps, prefs)
    server.fake = fake
    if prefs.problem:
        stream.host_log("warn", prefs.problem)
    server.start()
    url = f"http://{'127.0.0.1' if args.host == '0.0.0.0' else args.host}:{server.port}"
    print(f"ADC FFT at {url}   (board {board[0]}:{board[1]}, Ctrl-C to stop)")
    print(f"Settings kept in {prefs.path}" if prefs.path else "Settings are not kept (--no-save)")
    if args.host == "0.0.0.0":
        lan = lan_address()
        print(
            f"On a phone or tablet on the same network: http://{lan}:{server.port}"
            if lan
            else "Shown to the network (no network address found)."
        )
        print(
            "  Anyone on that network can open the page, change the settings and reboot the board."
        )
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
