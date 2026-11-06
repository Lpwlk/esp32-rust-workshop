"""ESP32-S31 boards plugged into this computer, for `scripts/dev.py`.

Each ESP32-S31-Function-CoreBoard-1 shows up once on USB, through the chip's
built-in USB Serial/JTAG (Espressif 303a:1001). That single device is both:

- a serial port (espflash: flash, monitor), whose USB serial number is the chip's
  MAC address, e.g. `1C:29:04:D0:6C:1A`;
- a debug probe (probe-rs: on-target tests, RTT), whose id is
  `303a:1001:<the same MAC>`.

So one MAC gives both, and no board ever has to be reset to be identified.

Which board plays node A or node B is remembered in `.boards.json` at the root of
the repository (not committed: it describes your desk, not the project):

    python3 scripts/dev.py boards assign a 6C:1A
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

from serial.tools import list_ports

ROOT = Path(__file__).resolve().parent.parent
ASSIGNMENTS = ROOT / ".boards.json"
ESPRESSIF_USB_JTAG = (0x303A, 0x1001)
NODES = ("a", "b")


@dataclass(frozen=True)
class Board:
    mac: str  # "1C:29:04:D0:6C:1A"
    port: str  # "/dev/cu.usbmodem11301"

    @property
    def probe(self) -> str:
        """The probe-rs selector of this board (`--probe` / PROBE_RS_PROBE)."""
        return f"{ESPRESSIF_USB_JTAG[0]:04x}:{ESPRESSIF_USB_JTAG[1]:04x}:{self.mac}"

    @property
    def short(self) -> str:
        """Last two bytes of the MAC: enough to tell boards apart on a desk."""
        return self.mac[-5:]


def detect() -> list[Board]:
    """Boards currently plugged in, sorted by MAC (stable across replugs)."""
    boards = {}
    for p in list_ports.comports():
        if (p.vid, p.pid) != ESPRESSIF_USB_JTAG or not p.serial_number:
            continue
        # macOS lists each device twice (cu.* and tty.*): cu.* is the one to open.
        if "/tty." in p.device and p.serial_number in boards:
            continue
        boards[p.serial_number.upper()] = Board(p.serial_number.upper(), p.device)
    return sorted(boards.values(), key=lambda b: b.mac)


def load_assignments() -> dict[str, str]:
    """{"a": MAC, "b": MAC}, for the nodes that have a board assigned."""
    try:
        data = json.loads(ASSIGNMENTS.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return {}
    return {n: str(m).upper() for n, m in data.items() if n in NODES}


def save_assignment(node: str, mac: str) -> None:
    data = load_assignments()
    # One board can't be both nodes.
    data = {n: m for n, m in data.items() if m != mac}
    data[node] = mac
    ASSIGNMENTS.write_text(json.dumps(dict(sorted(data.items())), indent=2) + "\n")


def node_of(board: Board) -> str | None:
    return next((n for n, m in load_assignments().items() if m == board.mac), None)


class BoardError(Exception):
    """No board, or no way to tell which one: the message says what to do."""


def match(boards: list[Board], selector: str) -> Board:
    """A board from part of its MAC (`6C:1A`, `6c1a`), its port, or its index."""
    sel = selector.strip()
    if sel.isdigit() and int(sel) < len(boards):
        return boards[int(sel)]
    norm = sel.replace(":", "").upper()
    found = [b for b in boards if b.port == sel or (norm and b.mac.replace(":", "").endswith(norm))]
    if len(found) == 1:
        return found[0]
    if not found:
        raise BoardError(f"no plugged board matches '{selector}'.{listing(boards)}")
    raise BoardError(f"'{selector}' matches several boards: give more of the MAC.")


def resolve(node: str | None, selector: str | None) -> Board:
    """The board to use.

    1. `--board` given: that one.
    2. A node given and a board assigned to it: that one (if plugged in).
    3. A single board plugged in: that one.
    """
    boards = detect()
    if not boards:
        raise BoardError(
            "no ESP32-S31 found on USB (Espressif 303a:1001). Plug the board by its "
            "USB Serial/JTAG port (not the UART one)."
        )
    if selector:
        return match(boards, selector)
    if node:
        mac = load_assignments().get(node)
        if mac:
            board = next((b for b in boards if b.mac == mac), None)
            if board:
                return board
            raise BoardError(
                f"node {node.upper()}'s board ({mac}) isn't plugged in.{listing(boards)}"
            )
    if len(boards) == 1:
        return boards[0]
    hint = (
        f"assign one with `dev.py boards assign {node} <mac>`"
        if node
        else "choose one with --board <mac>"
    )
    raise BoardError(f"{len(boards)} boards plugged in: {hint}.{listing(boards)}")


def listing(boards: list[Board]) -> str:
    if not boards:
        return ""
    return "\nPlugged in: " + ", ".join(f"{b.short} ({b.port})" for b in boards)
