#!/usr/bin/env python3
"""Records the Ethernet console demos of the README (and the docs) from two real boards.

Each scene flashes both boards with the preset it shows, then records node A's console with
`scripts/record.py` (a pseudo-terminal, so the colors are kept; commands are typed one key at a
time) and renders a GIF with agg:

    boot    the boot card, the cable coming up at 1 Gbit/s, the live line, then `verbose 1` and `verbose`
    cli     `link`, `config`, `led 2 blink 300`, `pattern ocean` (UDP presets)
    burst   back-to-back and 1 kHz bursts, then round-trip times (`tx on-rx` on node B)
    tcp     the TCP presets: the client connects, `link`, a burst

    python3 scripts/record_demos.py                  # every scene
    python3 scripts/record_demos.py boot burst       # some of them
    python3 scripts/record_demos.py --font-size 12   # with another look (see render.toml)

The look of the GIFs (font size, theme, speed...) is in docs/book/assets/demo/render.toml. To
change it for recordings that exist, without a board: python3 scripts/render_demos.py.

Needs both boards plugged in and assigned (`python3 scripts/dev.py boards assign a 6C:1A`), the
Ethernet cable between them, no other program holding their serial ports, and
`brew install agg gifsicle`. The boards end up flashed with the default (UDP) presets.
Output: docs/book/assets/demo/eth-<scene>.cast and .gif.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
import tty
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

from gifrender import add_style_arguments, overrides_from, style_flags  # noqa: E402

ETH = ROOT / "firmware" / "ethernet"
OUT = ROOT / "docs" / "book" / "assets" / "demo"
ELF = ETH / "target" / "riscv32imafc-unknown-none-elf" / "release"
CHIP = "esp32s31"

# What the console prints before the firmware does: the ROM and bootloader logs, and espflash's
# banner (which also shows the local serial port).
TRIM = (
    r"^(ESP-ROM|Build:|rst:|Core\d|waiting for download|I \(|\[20|Commands:|Serial port|Chip type"
    r"|Crystal|Flash size|Features|MAC address|Opening|Connecting|.*CTRL\+R)"
)
RESET = r"0.8:\x12"  # Ctrl+R: `espflash monitor` doesn't reset the chip by itself

#                preset A       preset B      seconds, --start, extra record.py arguments
SCENES = {
    "boot": (
        "node-a",
        "node-b",
        20,
        0.7,
        [
            "--key", RESET,
            "--type", "7:verbose 1",  # from the live line to every frame...
            "--type", "13:verbose",  # ...and the next level: raw frames too
        ],
    ),  # fmt: skip
    "cli": (
        "node-a",
        "node-b",
        30,
        6.0,
        [
            "--key", RESET,
            "--type", '7:link',
            "--type", '12:config',
            "--type", '20:led 2 blink 300',
            "--type", '24:pattern ocean',
        ],
    ),  # fmt: skip
    "burst": (
        "node-a",
        "node-b",
        36,
        6.0,
        [
            "--key", RESET,
            "--type", '7:burst 2000 max',
            "--type", '13:burst 20000 max',
            "--type", '21:burst 5000 1kHz',
            "--type", '29:period 500ms',
        ],
    ),  # fmt: skip
    "tcp": (
        "node-a-tcp",
        "node-b-tcp",
        23,
        0.7,
        [
            "--key", RESET,
            "--type", '9:link',
            "--type", '13:burst 2000 max',
        ],
    ),  # fmt: skip
}


def run(cmd: list[str], **kwargs) -> None:
    print("$", " ".join(str(c) for c in cmd), flush=True)
    subprocess.run(cmd, check=True, **kwargs)


def flash(preset_a: str, preset_b: str, ports: dict[str, str]) -> None:
    env = {**os.environ, "PRESET_NODE_A": preset_a, "PRESET_NODE_B": preset_b}
    run(["cargo", "build", "--release", "--locked"], cwd=ETH, env=env)
    for node in "ab":
        run(
            ["espflash", "flash", "--chip", CHIP, "--port", ports[node], "--skip-update-check"]
            + [str(ELF / f"node-{node}")]
        )
    time.sleep(6)  # both boots, the cable negotiating, the first frames


def type_on(port: str, line: str) -> None:
    """Types one command on a board's console without recording it."""
    fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    try:
        tty.setraw(fd)
        os.write(fd, line.encode() + b"\r")
        time.sleep(0.5)
    finally:
        os.close(fd)


def record(scene: str, ports: dict[str, str], look: list[str]) -> None:
    preset_a, preset_b, seconds, start, extra = SCENES[scene]
    flash(preset_a, preset_b, ports)
    if scene == "burst":
        type_on(ports["b"], "tx on-rx")  # B answers every frame: A shows the round-trip times
    cast = OUT / f"eth-{scene}.cast"
    run(
        [sys.executable, str(SCRIPTS / "record.py"), str(cast), "--gif"]
        + ["--cols", "100", "--rows", "34", "--seconds", str(seconds), "--start", str(start)]
        + ["--trim", TRIM]
        + extra
        + look
        + ["--", "espflash", "monitor", "--chip", CHIP, "--port", ports["a"], "--skip-update-check"]
    )
    if scene == "burst":
        type_on(ports["b"], "tx periodic")


def main() -> None:
    from boards import BoardError, resolve

    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "scenes", nargs="*", metavar="SCENE", help=f"{', '.join(SCENES)} (default: all)"
    )
    add_style_arguments(parser)
    args = parser.parse_args()
    # The look: render.toml, and whatever was given here on top (passed on to record.py).
    look = style_flags(overrides_from(args))
    wanted = args.scenes or list(SCENES)
    unknown = [s for s in wanted if s not in SCENES]
    if unknown:
        sys.exit(f"unknown scene {unknown}: choose from {', '.join(SCENES)}")
    try:
        ports = {n: resolve(n, None).port for n in "ab"}
    except BoardError as e:
        sys.exit(
            f"{e}\nAssign both boards first: python3 scripts/dev.py boards assign a|b <MAC end>"
        )
    if ports["a"] == ports["b"]:
        sys.exit("nodes A and B are the same board: assign the other one")

    for scene in wanted:
        print(f"\n=== {scene}", flush=True)
        record(scene, ports, look)
    # Leave the boards on the default presets.
    if any(SCENES[s][0].endswith("-tcp") for s in wanted):
        flash("node-a", "node-b", ports)
    print(f"\nDone: {OUT.relative_to(ROOT)}/eth-*.gif")


if __name__ == "__main__":
    main()
