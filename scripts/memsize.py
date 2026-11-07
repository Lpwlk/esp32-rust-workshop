"""Flash and RAM used by a firmware ELF, like GCC's `--print-memory-usage`.

The linker can print that table itself (`-C link-arg=--print-memory-usage`), but
on esp-hal it's misleading: one line per region *alias* (ROM, RAM repeated a dozen
times), and RAM always at 100 %, because the linker script gives every byte left
to the stack. So this reads the ELF section headers instead and sums what matters:

- **flash**: code and constants (mapped from flash), plus the initial values of
  the RAM sections that are copied at boot (`.data`, `.trap`, `.rwtext`, …);
- **RAM**: what the firmware reserves statically (`.data`, `.bss`, RAM code);
  the **stack** is the rest, shown apart;
- **RTC fast**: the low-power RAM (unused by this firmware).

Standard library only: works before `.venv` has rich, and without llvm-tools.
"""

from __future__ import annotations

import json
import struct
from dataclasses import dataclass
from pathlib import Path

# ESP32-S31 memory map, as in esp-hal's linker script (`memory.x`) and what lld's
# `--print-memory-usage` reports for these regions.
RAM_BASE = 0x2F00_0000
RTC_FAST_BASE, RTC_FAST_SIZE = 0x2E00_0000, 32 * 1024
FLASH_BASE, FLASH_WINDOW = 0x4000_0000, 4_194_272  # flash mapped through the MMU

SHF_ALLOC = 0x2
SHT_NOBITS = 8


@dataclass
class Section:
    name: str
    addr: int
    size: int
    loaded: bool  # has content in the image (not .bss-like)


@dataclass
class Usage:
    flash: int
    ram: int
    ram_size: int  # static RAM + stack: the whole RAM region
    stack: int
    rtc: int

    def as_dict(self) -> dict[str, int]:
        return self.__dict__.copy()


def sections(elf: Path) -> list[Section]:
    """Allocated sections of a 32-bit little-endian ELF."""
    data = elf.read_bytes()
    if data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
        raise ValueError(f"{elf}: not a 32-bit little-endian ELF")
    shoff = struct.unpack_from("<I", data, 0x20)[0]
    shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 0x2E)
    headers = [struct.unpack_from("<IIIIII", data, shoff + i * shentsize) for i in range(shnum)]
    strtab_off = headers[shstrndx][4]

    def name(off: int) -> str:
        end = data.index(b"\0", strtab_off + off)
        return data[strtab_off + off : end].decode()

    return [
        Section(name(n), addr, size, typ != SHT_NOBITS)
        for n, typ, flags, addr, _off, size in headers
        if flags & SHF_ALLOC and size
    ]


def usage(elf: Path) -> Usage:
    flash = ram = stack = rtc = 0
    ram_end = RAM_BASE
    for s in sections(elf):
        if FLASH_BASE <= s.addr < FLASH_BASE + FLASH_WINDOW:
            flash += s.size
        elif s.addr >= RAM_BASE and s.addr < FLASH_BASE:
            ram_end = max(ram_end, s.addr + s.size)
            if s.name == ".stack":
                stack += s.size
            else:
                ram += s.size
                # Initialised RAM sections are stored in flash and copied at boot.
                flash += s.size if s.loaded else 0
        elif RTC_FAST_BASE <= s.addr < RTC_FAST_BASE + RTC_FAST_SIZE:
            rtc += s.size
            flash += s.size if s.loaded else 0
    return Usage(flash=flash, ram=ram, ram_size=ram_end - RAM_BASE, stack=stack, rtc=rtc)


def human(n: int) -> str:
    if n >= 1024 * 1024:
        return f"{n / 1024 / 1024:.2f} MB"
    if n >= 1024:
        return f"{n / 1024:.1f} KB"
    return f"{n} B"


def delta(now: int, before: int | None) -> str:
    if before is None or now == before:
        return ""
    d = now - before
    return f"{'+' if d > 0 else '-'}{human(abs(d))}"


def report(elf: Path, previous: Path | None = None) -> list[tuple[str, str, str, str, str]]:
    """Rows `(region, used, size, %, change)`; `previous`: a JSON file where the last
    result is kept, to show what changed since the previous build."""
    u = usage(elf)
    before = None
    if previous is not None:
        try:
            before = json.loads(previous.read_text())
        except (OSError, ValueError):
            before = None
        previous.parent.mkdir(parents=True, exist_ok=True)
        previous.write_text(json.dumps(u.as_dict()))

    def row(region: str, used: int, size: int, key: str) -> tuple[str, str, str, str, str]:
        pct = f"{100 * used / size:.2f}%" if size else "-"
        return (
            region,
            human(used),
            human(size),
            pct,
            delta(used, before.get(key) if before else None),
        )

    return [
        row("FLASH", u.flash, FLASH_WINDOW, "flash"),
        row("RAM", u.ram, u.ram_size, "ram"),
        row("  stack", u.stack, u.ram_size, "stack"),
        row("RTC_FAST", u.rtc, RTC_FAST_SIZE, "rtc"),
    ]


def main() -> None:
    import sys

    for path in sys.argv[1:]:
        print(f"{path}:")
        print(f"{'Memory region':>14}  {'Used Size':>10}  {'Region Size':>11}  {'%age Used':>9}")
        for region, used, size, pct, change in report(Path(path)):
            print(f"{region + ':':>14}  {used:>10}  {size:>11}  {pct:>9}  {change}")


if __name__ == "__main__":
    main()
