"""Record a terminal session as an asciicast (and a GIF) for the docs.

Runs a command (usually `espflash monitor` or `probe-rs attach`) in a
pseudo-terminal of a fixed size, so it prints exactly what a user sees, colors
included. It types scripted input like a person would (one key at a time), and
writes an asciicast v2 file; `agg` then turns it into a GIF.

    python3 scripts/record.py docs/book/assets/demo/cli.cast --gif \\
        --cols 100 --rows 30 --seconds 25 \\
        --type 4:"help" --type 9:"pattern ocean" \\
        -- espflash monitor --chip esp32s31 --port /dev/cu.usbmodem11201

`--type T:TEXT` types TEXT then Enter, starting T seconds after the start.
`--key T:KEYS` sends raw keys (no Enter), e.g. `--key 1:'\x12'` for Ctrl+R.
`--start S` drops what was printed before S seconds (the boot, after a Ctrl+R).
`--trim REGEX` drops output lines matching REGEX (tool banners, local paths).

Standard library only (pty), plus `agg` for `--gif` (brew install agg), and
gifsicle to shrink the GIF if it's installed (brew install gifsicle).
"""

from __future__ import annotations

import argparse
import json
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time
from fcntl import ioctl
from pathlib import Path

TYPE_DELAY = 0.07  # seconds between typed keys


def parse_type(spec: str) -> tuple[float, str]:
    at, _, text = spec.partition(":")
    return float(at), text


def record(cmd: list[str], seconds: float, cols: int, rows: int, typed, raw, trim) -> list:
    pid, fd = pty.fork()
    if pid == 0:  # child: the command, in the pseudo-terminal
        os.environ.update(TERM="xterm-256color", COLUMNS=str(cols), LINES=str(rows))
        os.execvp(cmd[0], cmd)
    ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    # Keys to send: (time, bytes), one per character, then Enter.
    keys = []
    for at, text in sorted(typed):
        for i, ch in enumerate(text):
            keys.append((at + i * TYPE_DELAY, ch.encode()))
        keys.append((at + len(text) * TYPE_DELAY + 0.15, b"\r"))
    keys += [(at, text.encode().decode("unicode_escape").encode("latin-1")) for at, text in raw]
    keys.sort(key=lambda k: k[0])

    events, start, pending = [], time.monotonic(), b""
    while (now := time.monotonic() - start) < seconds:
        while keys and keys[0][0] <= now:
            os.write(fd, keys.pop(0)[1])
        ready, _, _ = select.select([fd], [], [], 0.05)
        if not ready:
            # Quiet for 50 ms: an unfinished line is the prompt (or typed keys),
            # not half of a line still arriving. Show it.
            if pending:
                events.append([round(now, 3), "o", pending.decode("utf-8", "replace")])
                pending = b""
            continue
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data:
            break
        pending += data
        # Emit whole lines only, so --trim sees each line in one piece.
        *lines, pending = pending.split(b"\n")
        out = "".join(
            line.decode("utf-8", "replace") + "\n"
            for line in lines
            if not trim or not trim.search(line.decode("utf-8", "replace"))
        )
        if out:
            events.append([round(now, 3), "o", out])
    # Stop it like a user would (Ctrl+C), then for good if it doesn't listen:
    # probe-rs can hang while releasing the probe.
    os.kill(pid, signal.SIGINT)
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        if os.waitpid(pid, os.WNOHANG) != (0, 0):
            break
        time.sleep(0.1)
    else:
        # Don't wait for it: a process stuck in the USB driver may never be reaped.
        os.kill(pid, signal.SIGKILL)
    return events


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("cast", type=Path, help="asciicast file to write")
    p.add_argument("--gif", action="store_true", help="also render <cast>.gif with agg")
    p.add_argument("--cols", type=int, default=100)
    p.add_argument("--rows", type=int, default=30)
    p.add_argument("--seconds", type=float, default=20)
    p.add_argument("--type", action="append", default=[], type=parse_type, metavar="T:TEXT")
    p.add_argument(
        "--key",
        action="append",
        default=[],
        type=parse_type,
        metavar="T:KEYS",
        help="raw keys, no Enter; escapes allowed: --key 1:'\\x12' (Ctrl+R)",
    )
    p.add_argument("--trim", help="drop output lines matching this regex")
    p.add_argument(
        "--start",
        type=float,
        default=0.0,
        help="drop what was printed before this time, s (e.g. the boot)",
    )
    p.add_argument("--speed", type=float, default=1.0, help="GIF playback speed")
    argv = sys.argv[1:]
    if "--" not in argv:
        p.error("no command: put it after --")
    split = argv.index("--")
    args, cmd = p.parse_args(argv[:split]), argv[split + 1 :]

    trim = re.compile(args.trim) if args.trim else None
    events = record(cmd, args.seconds, args.cols, args.rows, args.type, args.key, trim)
    if args.start:
        events = [[round(t - args.start, 3), k, d] for t, k, d in events if t >= args.start]
    header = {
        "version": 2,
        "width": args.cols,
        "height": args.rows,
        "env": {"TERM": "xterm-256color"},
    }
    args.cast.parent.mkdir(parents=True, exist_ok=True)
    args.cast.write_text("\n".join([json.dumps(header)] + [json.dumps(e) for e in events]) + "\n")
    print(f"{args.cast}: {len(events)} events")

    if args.gif:
        if not shutil.which("agg"):
            sys.exit(
                "agg not found: brew install agg (or cargo install --git https://github.com/asciinema/agg)"
            )
        gif = args.cast.with_suffix(".gif")
        subprocess.run(
            [
                "agg",
                "--theme",
                "monokai",
                "--font-size",
                "14",
                "--idle-time-limit",
                "1.5",
                "--speed",
                str(args.speed),
                "--last-frame-duration",
                "4",
                str(args.cast),
                str(gif),
            ],
            check=True,
        )
        if shutil.which("gifsicle"):
            # Each scroll redraws the whole screen: lossy + a small palette divides
            # the size by 2-3 without visible loss on terminal colors.
            subprocess.run(
                ["gifsicle", "-O3", "--lossy=60", "--colors", "32", "-b", str(gif)], check=True
            )
        print(f"{gif}: {gif.stat().st_size // 1024} KB")


if __name__ == "__main__":
    main()
