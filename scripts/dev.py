#!/usr/bin/env python3
"""Dev tasks for this repository: ESP32-S31 boards, tests, docs site, Python tooling, git.

    python3 scripts/dev.py setup                 # once: .venv (Python 3.13) + requirements
    python3 scripts/dev.py doctor                # tools, Rust target, boards: what's missing

  Three firmwares, chosen with `--app`, an option of the command like any other (`dev flash all --app eth --rtt`;
  the command always comes first, and nobody needs to `cd` anywhere):
    eth   board-to-board over Ethernet: nodes a and b (default)        firmware/ethernet/
    uart  board-to-board over a UART: nodes a and b                    firmware/uart/
    adc   board-to-computer: the ADC streamed over Ethernet (adc-stream): nodes a or b   firmware/adc-stream/

  Boards (node = a | b, the boards registered with `boards assign`; every app runs on either of them):
    python3 scripts/dev.py flash a -m --app uart  # the UART app instead
    python3 scripts/dev.py flash b -m --app adc  # the ADC streamer on node B's board, then its console
    python3 scripts/dev.py fft                   # the live spectrum page (Plotly) for adc-stream, in the browser
    python3 scripts/dev.py fft --demo            # the same with a simulated board
    python3 scripts/dev.py dash                  # the dashboard of both boards (eth or uart app): their consoles, as controls and charts
    python3 scripts/dev.py dash --app uart       # the same for the UART app (an option of the page: the page reads the app from the boards anyway)
    python3 scripts/dev.py dash --demo [uart]    # the same with two simulated nodes
    python3 scripts/dev.py boards                # plugged boards: MAC, port, probe, node
    python3 scripts/dev.py boards assign a 6C:1A # this board is node A (remembered)
    python3 scripts/dev.py build [a|b] [-v]      # firmware, release (both by default); -v / -vv: cargo's verbose
    python3 scripts/dev.py size [a|b]            # flash / RAM used (also after each build)
    python3 scripts/dev.py flash a [-m]          # build + flash with espflash (+ monitor)
    python3 scripts/dev.py flash all [--rtt]     # both nodes: one build, both boards flashed at the same time
    python3 scripts/dev.py watch [a|b] [-f]      # rebuild on every saved source file (-f: reflash node a|b too)
    python3 scripts/dev.py monitor a [--keep]    # serial console + CLI, backtraces decoded; resets the board so its boot card shows
    python3 scripts/dev.py reset a               # restart the firmware: no build, no flashing
    python3 scripts/dev.py rtt a                 # RTT build, flash, cargo-embed console
    python3 scripts/dev.py erase a [--settings]  # erase the whole flash (--settings: only the saved settings)

  Tests and quality
    python3 scripts/dev.py test host [-u]        # core tests on this computer (-u: regen docs)
    python3 scripts/dev.py test target [a]       # firmware/uart/tests/hil.rs on a board, then reflash it
    python3 scripts/dev.py check [--fast]        # what CI runs, locally, with the time of each step (--fast: no firmware builds, no site)
    python3 scripts/dev.py format [--check]      # cargo fmt + black

  Documentation
    python3 scripts/dev.py docs serve [--open]   # http://localhost:3000, live reload
    python3 scripts/dev.py docs build [-r]       # docs/site/ (-r: regenerate reference pages)
    python3 scripts/dev.py docs api [--open]     # rustdoc of the core and the firmware

  Tooling
    python3 scripts/dev.py update [--dry-run]    # upgrade what's outdated in .venv
    python3 scripts/dev.py git branch|commit|push|squash ...

Everything after `setup` runs inside `.venv`: the script relaunches itself there.
That keeps the system Python out of it (a Python 3.15 alpha breaks pip 26.3 and
Zensical's dependencies). Logs: scripts/logs/dev.log.

What you see: while a command runs, a progress bar at the bottom of the terminal (a pulsing bar, the
elapsed time, the line the command is on). When it ends the bar goes and a log line takes its place:
`14:03:21 DONE  build node-a  6.2 s  156 crates compiled`, then the command it ran (`$ cargo build ...`).
A failure (ERROR) or warnings (WARNING) print the command's whole output, in color; scripts/logs/dev.log
keeps all of it. Consoles (`monitor`, `flash -m`, `rtt`) and dev servers (`fft`, `dash`, `docs serve`)
keep the terminal. The colors are a theme: set ACTIVE_THEME in scripts/devthemes.py.

`-v` / `-vv` (after the command: build, flash, watch, rtt, test, check, docs build|api) goes to the
cargo commands (the rustc and linker command lines, to see what a build really runs) and shows
cargo's own output, live, instead of the bar.

Needs a released Python 3.13 or newer to start; the first command creates .venv by itself.

Shorter to type: `pipx install --editable .` from the repository (or `uv tool install --editable .`) makes
a `dev` command that runs this script from any folder: `dev flash all --app eth --rtt`, `dev check --fast`...
without pipx, `scripts/dev` does the same on macOS and Linux: link it once onto your PATH
(`ln -s "$PWD/scripts/dev" ~/.local/bin/dev`). The command comes first, its options (`--app`) after it.

A command that ends by itself after 30 s or more (build, flash, check, test, ...) raises a desktop
notification, so you can look away; `DEV_NOTIFY=0` turns that off.
"""

from __future__ import annotations

# Standard library only up to `main()`: `setup` must work before rich is installed.
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"
TOOLS = ROOT / "tools"
FFT_TOOL = TOOLS / "adc-fft" / "adc_fft.py"
DASH_TOOL = TOOLS / "node-dash" / "node_dash.py"
VENV = ROOT / ".venv"
VENV_BIN = VENV / ("Scripts" if os.name == "nt" else "bin")
VENV_PYTHON = VENV_BIN / "python"
REQUIREMENTS = SCRIPTS / "requirements.txt"
MINIMUM_PYTHON = (3, 13)
# Zensical's dependencies don't support Python 3.15 yet (an alpha): the first released one found.
PYTHON_CANDIDATES = ["python3.13", "python3.14", "python3"]
COMMIT_TYPES = ["feat", "fix", "chore", "refactor", "test", "docs"]

CHIP = "esp32s31"
TARGET = "riscv32imafc-unknown-none-elf"
NODES = ("a", "b")

NOTIFY_COMMANDS = ("build", "flash", "erase", "check", "test", "update")
NOTIFY_AFTER_SECONDS = 30

# `--app` and `--preset`: the firmware that build / flash / monitor work on, and the preset built
# instead of node-a / node-b (firmware/<app>/presets/NAME.toml).
APP = "eth"
PRESET: str | None = None
VERBOSE = 0
APP_DIRS = {
    "uart": ROOT / "firmware" / "uart",
    "eth": ROOT / "firmware" / "ethernet",
    "adc": ROOT / "firmware" / "adc-stream",
}
# The UART app is a member of the root workspace (one `target/` at the root); the others are their own.
TARGET_DIRS = {
    "uart": ROOT / "target",
    "eth": ROOT / "firmware" / "ethernet" / "target",
    "adc": ROOT / "firmware" / "adc-stream" / "target",
}
# `cargo ... --all` at the root doesn't reach the apps that are their own Cargo workspaces.
OWN_WORKSPACES = ("eth", "adc")

# Not `run -q` (cargo refuses -v with -q), `presets` (an alias that ends in `--`: the flag would
# reach that program), `fmt` or `embed`.
VERBOSE_SUBCOMMANDS = ("build", "clippy", "doc", "test", "test-host", "test-target")


def in_venv() -> bool:
    return Path(sys.prefix).resolve() == VENV.resolve()


def usable_python(name: str) -> str | None:
    """The path of interpreter `name` when it is a released Python of the minimum version or newer."""
    path = shutil.which(name)
    if not path:
        return None
    released = f"sys.version_info >= {MINIMUM_PYTHON} and sys.version_info.releaselevel == 'final'"
    check = subprocess.run(
        [path, "-c", f"import sys; sys.exit(not ({released}))"], capture_output=True
    )
    return path if check.returncode == 0 else None


def setup(python: str | None) -> None:
    candidates = [python] if python else PYTHON_CANDIDATES
    interpreter = next(filter(None, map(usable_python, candidates)), None)
    if not interpreter:
        sys.exit(
            f"dev needs Python {MINIMUM_PYTHON[0]}.{MINIMUM_PYTHON[1]} or newer, released "
            f"(tried {', '.join(candidates)}): https://www.python.org/downloads/"
        )
    print(f"• creating {VENV.relative_to(ROOT)} with {interpreter}")
    subprocess.run([interpreter, "-m", "venv", "--clear", str(VENV)], check=True)
    pip_command = [str(VENV_PYTHON), "-m", "pip", "--disable-pip-version-check"]
    print("• upgrading pip")
    subprocess.run([*pip_command, "install", "--quiet", "--upgrade", "pip"], check=True)
    print(f"• installing {REQUIREMENTS.relative_to(ROOT)}")
    subprocess.run([*pip_command, "install", "--quiet", "-r", str(REQUIREMENTS)], check=True)
    print("✓ .venv ready (`dev doctor` lists what else is missing)")


def requirement_names() -> list[str]:
    """Package names from requirements.txt (version specifiers and comments dropped)."""
    names = []
    for line in REQUIREMENTS.read_text().splitlines():
        requirement = line.split("#")[0].strip()
        if requirement:
            for separator in "<>=!~[ ;":
                requirement = requirement.split(separator)[0]
            names.append(requirement)
    return names


def update(dry_run: bool) -> None:
    from devlib import TREE_BRANCH, TREE_LAST, done, log, run

    wanted_packages = ["pip", *requirement_names()]
    log.info(f"Checking {', '.join(f'[package]{name}[/]' for name in wanted_packages)} …")
    pip_command = [str(VENV_PYTHON), "-m", "pip", "--disable-pip-version-check"]
    installed_names = {
        package["name"].lower()
        for package in json.loads(run([*pip_command, "list", "--format=json"]).stdout)
    }
    missing_packages = [name for name in wanted_packages if name.lower() not in installed_names]
    if missing_packages:
        missing_list = ", ".join(f"[package]{name}[/]" for name in missing_packages)
        log.info(f"{TREE_BRANCH}missing: {missing_list}")
        if not dry_run:
            run([*pip_command, "install", "--quiet", "-r", str(REQUIREMENTS)])
    outdated_packages = json.loads(
        run([*pip_command, "list", "--outdated", "--format=json"]).stdout
    )
    wanted_lowercase = {name.lower() for name in wanted_packages}
    packages_to_upgrade = [p for p in outdated_packages if p["name"].lower() in wanted_lowercase]

    if not packages_to_upgrade:
        log.info(f"{TREE_LAST}everything is up to date")
        return
    for index, package in enumerate(packages_to_upgrade):
        tree_prefix = TREE_LAST if index == len(packages_to_upgrade) - 1 else TREE_BRANCH
        log.info(
            f"{tree_prefix}[package]{package['name']}[/] [loss]{package['version']}[/] → "
            f"[gain]{package['latest_version']}[/]"
        )
    if dry_run:
        log.info("dry run: nothing installed")
        return
    run(
        [
            *pip_command,
            "install",
            "--upgrade",
            *(package["name"] for package in packages_to_upgrade),
        ]
    )
    done("updated")


def format_code(check_only: bool) -> None:
    from devlib import done, task

    action = "check" if check_only else "format"
    task(
        f"{action} Rust (workspace)",
        ["cargo", "fmt", "--all"] + (["--", "--check"] if check_only else []),
    )
    for app in OWN_WORKSPACES:
        task(
            f"{action} Rust ({APP_DIRS[app].name})",
            ["cargo", "fmt"] + (["--", "--check"] if check_only else []),
            working_directory=APP_DIRS[app],
        )
    task(
        f"{action} Python (black)",
        [str(VENV_BIN / "black"), "--line-length", "100"]
        + (["--check"] if check_only else [])
        + [str(SCRIPTS.relative_to(ROOT)), str(TOOLS.relative_to(ROOT))],
    )
    done("already formatted" if check_only else "formatted")


def app_dir() -> Path:
    return APP_DIRS[APP]


def with_verbose(command: list[str]) -> list[str]:
    """`command` with cargo's `-v` (or `-vv`) right after the subcommand, when asked for.
    Not at the end: after a `--` (clippy's `-D warnings`) it would belong to another program."""
    if not VERBOSE or command[0] != "cargo" or command[1] not in VERBOSE_SUBCOMMANDS:
        return command
    return [*command[:2], "-" + "v" * min(VERBOSE, 2), *command[2:]]


def app_nodes() -> tuple[str, ...]:
    """Every app runs on either board (adc-stream is one binary for both)."""
    return NODES


def bin_name(node: str) -> str:
    return "adc-stream" if APP == "adc" else f"node-{node}"


def preset_environment() -> dict[str, str] | None:
    return {"PRESET": PRESET} if PRESET else None


def elf_path(node: str, release: bool = True) -> Path:
    return TARGET_DIRS[APP] / TARGET / ("release" if release else "debug") / bin_name(node)


def board_for(node: str | None, selector: str | None):
    """The board to use, or a clear message and exit."""
    from boards import BoardError, resolve
    from devlib import log

    try:
        board = resolve(node, selector)
    except BoardError as error:
        log.error(str(error))
        raise SystemExit(1)
    node_prefix = f"node {node.upper()} → " if node else ""
    log.info(f"{node_prefix}board [package]{board.short}[/] on [path]{board.port}[/]")
    return board


def boards_list() -> None:
    from boards import detect, load_assignments, node_of
    from devlib import console, log
    from rich.table import Table

    plugged_boards = detect()
    if not plugged_boards:
        log.warning("No ESP32-S31 on USB. Plug it by its USB Serial/JTAG port.")
    else:
        table = Table(box=None, header_style="table_header", pad_edge=False)
        for column in ("#", "node", "MAC", "port", "probe (probe-rs)"):
            table.add_column(column)
        for index, board in enumerate(plugged_boards):
            node = node_of(board)
            table.add_row(
                str(index),
                node.upper() if node else "[muted]-[/]",
                board.mac,
                board.port,
                board.probe,
            )
        console.print(table)
    plugged_macs = {board.mac for board in plugged_boards}
    for node, mac in load_assignments().items():
        if mac not in plugged_macs:
            log.info(f"node {node.upper()}: board {mac} not plugged in")
    if plugged_boards and not load_assignments():
        log.info(
            "Tell them apart: `dev.py boards assign a <MAC end>`, e.g. " + plugged_boards[0].short
        )


def boards_assign(node: str, selector: str) -> None:
    from boards import BoardError, detect, match, save_assignment
    from devlib import done, log

    try:
        board = match(detect(), selector)
    except BoardError as error:
        log.error(str(error))
        raise SystemExit(1)
    save_assignment(node, board.mac)
    done(f"node {node.upper()} = board [package]{board.mac}[/] ([path]{board.port}[/])")


def cargo_build(nodes: list[str], rtt: bool, release: bool) -> None:
    from devlib import log, task

    if rtt and APP == "adc":
        log.error("The RTT console is for the node apps: `--rtt` doesn't apply to adc-stream.")
        raise SystemExit(1)
    binaries = list(dict.fromkeys(bin_name(node) for node in nodes))
    task(
        f"build {', '.join(binaries)}" + ("" if release else " [muted](debug)[/]"),
        with_verbose(
            ["cargo", "build"]
            + (["--release"] if release else [])
            + (["--features", "rtt"] if rtt else [])
            + [argument for binary in binaries for argument in ("--bin", binary)]
        ),
        working_directory=app_dir(),
        extra_environment=preset_environment(),
    )
    memory_usage(nodes, release)


def memory_usage(nodes: list[str], release: bool) -> None:
    """Flash / RAM table of each node, like GCC's `--print-memory-usage`, with the change since
    the previous build (scripts/memsize.py)."""
    from devlib import console, log
    from memsize import report
    from rich.table import Table

    # adc-stream is one binary for both nodes: show it once.
    for node in {bin_name(n): n for n in nodes}.values():
        built_elf = elf_path(node, release)
        if not built_elf.exists():
            log.warning(f"{bin_name(node)}: not built yet (`dev.py build {node}`)")
            continue
        previous_report = built_elf.parent / f".memsize-{bin_name(node)}.json"
        table = Table(
            title=f"{bin_name(node)} memory",
            title_justify="left",
            box=None,
            pad_edge=False,
            header_style="table_header",
        )
        for column in ("Memory region", "Used Size", "Region Size", "%age Used", "Change"):
            table.add_column(column, justify="right")
        for region, used, region_size, percent_used, change in report(built_elf, previous_report):
            style = "loss" if change.startswith("+") else "gain" if change else ""
            table.add_row(
                region, used, region_size, percent_used, f"[{style}]{change}[/]" if style else ""
            )
        console.print(table)


def espflash(subcommand: str, board, *arguments: str) -> list[str]:
    """An espflash command on this board (chip, port; no update check)."""
    return [
        "espflash",
        subcommand,
        "--chip",
        CHIP,
        "--port",
        board.port,
        "--skip-update-check",
        *arguments,
    ]


def flash_command(board, node: str, monitor: bool, release: bool) -> list[str]:
    """Writes the node's ELF (espflash skips the blocks the board already holds)."""
    return espflash(
        "flash", board, *(["--monitor"] if monitor else []), str(elf_path(node, release))
    )


def flash_label(node: str, board) -> str:
    return f"flash node {node.upper()} [muted]→ {board.short}[/]"


def flash(node: str, selector: str | None, monitor: bool, rtt: bool, release: bool) -> None:
    from devlib import live, task

    board = board_for(node, selector)
    cargo_build([node], rtt, release)
    if monitor:
        live(flash_command(board, node, True, release))
    else:
        task(flash_label(node, board), flash_command(board, node, False, release))


def flash_all(release: bool, rtt: bool) -> None:
    from boards import BoardError, resolve
    from devlib import done, fmt_secs, log, print_output, run_parallel

    if APP == "adc":
        log.error(
            "`flash all` is for the two nodes of the eth / uart apps: the ADC app runs on one board, `dev.py flash a|b --app adc`."
        )
        raise SystemExit(1)

    try:
        # Both boards first: don't flash A and then find out B isn't there.
        boards = {node: resolve(node, None) for node in NODES}
    except BoardError as error:
        log.error(
            f"{error}\n`flash all` needs both nodes assigned: `dev.py boards assign a|b <MAC>`."
        )
        raise SystemExit(1)
    if boards["a"] == boards["b"]:
        log.error("Nodes A and B are the same board: assign the other one.")
        raise SystemExit(1)
    # One cargo run for both: the shared crates compile once and the two links run side by side.
    cargo_build(list(NODES), rtt=rtt, release=release)
    labels = {node: flash_label(node, boards[node]) for node in NODES}
    started_at = time.monotonic()
    results = run_parallel(
        {node: flash_command(boards[node], node, False, release) for node in NODES}, labels
    )
    any_failed = False
    for exit_code, output, _seconds in results.values():
        if exit_code != 0:
            print_output(output)
            any_failed = True
    if any_failed:
        raise SystemExit(1)
    done(
        f"both boards flashed side by side in {fmt_secs(time.monotonic() - started_at)}: "
        "`dev.py monitor a` / `dev.py monitor b` (two terminals)"
    )


def monitor(node: str | None, selector: str | None, keep: bool) -> None:
    from devlib import live, live_pressing, log

    board = board_for(node, selector)
    command = espflash("monitor", board)
    if node and elf_path(node).exists():
        command += ["--elf", str(elf_path(node))]
    elif node:
        log.info(f"no {elf_path(node).relative_to(ROOT)} yet: backtraces won't be decoded")
    if keep or not sys.stdin.isatty() or not sys.stdout.isatty():
        log.info("Ctrl+C to quit")
        live(command, exit_on_failure=False)
        return
    # espflash doesn't reset the board when it attaches, and a board that is quiet (no dashboard, no trace)
    # prints nothing until its next boot: Ctrl+R, once the monitor is up, shows the boot card at once.
    log.info(
        "Ctrl+C to quit · the board is reset so that its boot card shows (`--keep` to attach as it is)"
    )
    live_pressing(command, after_seeing=b"CTRL+C", press=b"\x12")


def rtt(node: str, selector: str | None) -> None:
    """RTT console through the probe. Flashing goes through espflash (the probe-rs 0.32
    revision trap, see the Rust guide); cargo-embed only attaches (firmware/uart/Embed.toml)."""
    from devlib import live, log

    if APP == "adc":
        log.error("The RTT console is for the node apps, not adc-stream.")
        raise SystemExit(1)
    board = board_for(node, selector)
    flash(node, board.mac, monitor=False, rtt=True, release=True)
    live(
        ["cargo", "embed", "--release", "--features", "rtt", "--bin", f"node-{node}"]
        + ["--probe", board.probe]
        + ["--config", str(APP_DIRS["uart"] / "Embed.toml")],
        working_directory=app_dir(),
        exit_on_failure=False,
    )


def erase(node: str | None, selector: str | None, settings_only: bool) -> None:
    """The whole flash, or with `settings_only` only the `nvs` partition, where `save` keeps the
    node's settings (a few seconds less than the whole chip, and the firmware stays)."""
    from devlib import log, task

    board = board_for(node, selector)
    if settings_only:
        task(
            f"erase saved settings [muted](nvs) → {board.short}[/]",
            espflash("erase-parts", board, "nvs"),
        )
        log.info("[muted]the firmware boots on its build config: `dev.py reset` restarts it[/]")
    else:
        task(f"erase the whole flash [muted]→ {board.short}[/]", espflash("erase-flash", board))


def reset(node: str | None, selector: str | None) -> None:
    """Restarts the board's firmware: no build, no flashing."""
    from devlib import task

    board = board_for(node, selector)
    task(f"reset [muted]→ {board.short}[/]", espflash("reset", board))


def watched_files_mtimes() -> dict[Path, int]:
    """Modification times of what a build of the chosen app depends on."""
    mtimes: dict[Path, int] = {}
    watched_paths: list[Path] = []
    watched_roots = [
        app_dir(),
        ROOT / "firmware" / "shared",
        ROOT / "firmware" / "ferrite-esp32s31",
    ]
    for watched_root in watched_roots:
        for directory, subdirectories, file_names in os.walk(watched_root):
            # target/ holds thousands of build files that change on every build.
            subdirectories[:] = [
                name for name in subdirectories if name not in ("target", ".git", "node_modules")
            ]
            for file_name in file_names:
                if Path(file_name).suffix in (".rs", ".toml", ".x"):
                    watched_paths.append(Path(directory, file_name))
    watched_paths += [
        ROOT / "Cargo.toml",
        ROOT / "esp-config.toml",
        ROOT / ".cargo" / "config.toml",
    ]
    for path in watched_paths:
        try:
            mtimes[path] = path.stat().st_mtime_ns
        except OSError:  # an editor's temporary file, gone since the walk
            pass
    return mtimes


def watch(node: str | None, selector: str | None, flash_after_build: bool, release: bool) -> None:
    """Rebuilds whenever a source file is saved (and reflashes the board with `--flash`).
    A failed build is shown and the loop waits for the next save."""
    from boards import BoardError, resolve
    from devlib import console, log, task

    nodes = [node] if node else list(app_nodes())
    if flash_after_build:
        board_for(node, selector)
    known_mtimes = watched_files_mtimes()
    log.info(
        f"watching [path]{app_dir().relative_to(ROOT)}[/] and the shared crates (Ctrl+C to stop)"
    )
    try:
        while True:
            try:
                cargo_build(nodes, False, release)
                if flash_after_build:
                    board = resolve(node, selector)
                    task(flash_label(node, board), flash_command(board, node, False, release))
            except BoardError as error:
                log.warning(f"not flashed: {error}")
            except SystemExit as exit_request:
                if exit_request.code in (130, -2):
                    raise KeyboardInterrupt
                log.warning("failed: fix it and save, the next save builds again")
            # The snapshot predates the build: a save made meanwhile triggers the next round.
            while (current_mtimes := watched_files_mtimes()) == known_mtimes:
                time.sleep(0.5)
            time.sleep(0.3)  # an editor or a formatter writes several files: let it finish
            changed_paths = sorted(
                path
                for path in {*current_mtimes, *known_mtimes}
                if current_mtimes.get(path) != known_mtimes.get(path)
            )
            known_mtimes = watched_files_mtimes()
            changed_names = ", ".join(path.name for path in changed_paths[:3])
            changed_names += " …" if len(changed_paths) > 3 else ""
            console.rule(
                f"[muted]{time.strftime('%H:%M:%S')} {changed_names} changed[/]", style="muted"
            )
    except KeyboardInterrupt:
        log.info("stopped")


def tests_summary(output: str) -> str:
    """`251 tests passed`, from the `test result:` line of each cargo test binary."""
    passed = sum(int(count) for count in re.findall(r"(\d+) passed", output))
    return f"{passed} tests passed" if passed else ""


def test_host(update_docs: bool) -> None:
    from devlib import log, task

    task(
        "host tests",
        with_verbose(["cargo", "test-host"]),
        extra_environment={"UPDATE_DOCS": "1"} if update_docs else None,
        summarize=tests_summary,
    )
    if update_docs:
        log.info("reference pages regenerated: review and commit docs/book/reference")


def test_target(node: str | None, selector: str | None, keep_test_image: bool) -> None:
    from devlib import log, task

    board = board_for(node, selector)
    exit_code = task(
        f"on-target tests [muted]→ {board.short}[/]",
        with_verbose(["cargo", "test-target"]),
        extra_environment={"PROBE_RS_PROBE": board.probe},
        show_output="always",
        exit_on_failure=False,
    )
    # probe-rs left the test image in flash, with the wrong minimum chip revision: the board
    # would boot-loop. Put the node's firmware back with espflash.
    if node and not keep_test_image:
        log.info(f"reflashing node {node.upper()}'s firmware (the tests replaced it)")
        flash(node, board.mac, monitor=False, rtt=False, release=True)
    elif not keep_test_image:
        log.warning(
            "The board now holds the test image: reflash it (`dev.py flash a|b --board "
            f"{board.short}`)."
        )
    if exit_code != 0:
        raise SystemExit(exit_code)


def check(fast: bool) -> None:
    """The CI pipeline (.github/workflows/ci.yml), in the same order, stopping at the
    first failure. On-target tests aren't in CI (no board there): `test target`.

    `fast` leaves out the three firmware builds (fat LTO, the slow part) and the docs site:
    clippy compiles the same code in check mode, so type errors still stop it. What it can't see
    is a link error, which `build` or a full `check` shows."""
    from devlib import done, fmt_secs, task

    steps = [
        ("host tests + reference pages up to date", ["cargo", "test-host"]),
        (
            "clippy, core (host)",
            "cargo clippy -p workshop-core --target host-tuple --all-targets -- -D warnings".split(),
        ),
        *[
            (
                f"example {example.stem} (shown in the docs)",
                f"cargo run -q -p workshop-core --target host-tuple --example {example.stem}".split(),
            )
            for example in sorted((ROOT / "firmware/shared/core/examples").glob("*.rs"))
        ],
        ("presets load and validate", ["cargo", "presets", "check"]),
        ("firmware build (UART)", ["cargo", "build", "--release"]),
        ("clippy, firmware (UART)", "cargo clippy --release -- -D warnings".split()),
        ("firmware build (Ethernet)", ["cargo", "build", "--release"], APP_DIRS["eth"]),
        (
            "clippy, firmware (Ethernet)",
            "cargo clippy --release -- -D warnings".split(),
            APP_DIRS["eth"],
        ),
        ("firmware build (ADC stream)", ["cargo", "build", "--release"], APP_DIRS["adc"]),
        (
            "clippy, firmware (ADC stream)",
            "cargo clippy --release -- -D warnings".split(),
            APP_DIRS["adc"],
        ),
        (
            "ADC app: the PWM generator's arithmetic (host)",
            "rustc --edition 2024 --test src/pwm_plan.rs -o ../../target/pwm_plan_test".split(),
            APP_DIRS["adc"],
        ),
        ("ADC app: run those tests", [str(ROOT / "target" / "pwm_plan_test")]),
        (
            "ADC app: the potentiometer's arithmetic (host)",
            "rustc --edition 2024 --test src/pot.rs -o ../../target/pot_test".split(),
            APP_DIRS["adc"],
        ),
        ("ADC app: run those tests", [str(ROOT / "target" / "pot_test")]),
        (
            "adc-fft self test (receiver, FFT, web server, saved settings)",
            [str(VENV_PYTHON), str(FFT_TOOL), "--selftest"],
        ),
        (
            "node-dash self test (console parser, web server, simulated nodes)",
            [str(VENV_PYTHON), str(DASH_TOOL), "--selftest"],
        ),
    ]
    if fast:
        steps = [step for step in steps if not step[0].startswith("firmware build")]
    step_count = len(steps) + (0 if fast else 1)
    started_at = time.monotonic()
    for step_number, (name, command, *working_directory) in enumerate(steps, 1):
        task(
            f"[muted]{step_number}/{step_count}[/] {name}",
            with_verbose(command),
            working_directory=working_directory[0] if working_directory else None,
            summarize=tests_summary,
        )
    if not fast:
        task(
            f"[muted]{step_count}/{step_count}[/] docs site",
            [str(VENV_BIN / "zensical"), "build", "--clean", "-f", "docs/zensical.toml"],
        )
    done(
        f"all green in {fmt_secs(time.monotonic() - started_at)}"
        + (" (fast: no firmware builds, no site: `check` without --fast runs them)" if fast else "")
    )


def doctor() -> None:
    """What's installed, what's missing, what's plugged in."""
    from devlib import console, log, run
    from rich.table import Table

    def tool_version(command: list[str]) -> str | None:
        if not shutil.which(command[0]):
            return None
        result = run(command, exit_on_failure=False, quiet=True)
        output = result.stdout or result.stderr
        return output.strip().splitlines()[0] if result.returncode == 0 else None

    tools = [
        ("cargo", tool_version(["cargo", "--version"]), "https://rustup.rs"),
        ("espflash", tool_version(["espflash", "--version"]), "cargo install espflash --locked"),
        (
            "probe-rs",
            tool_version(["probe-rs", "--version"]),
            "cargo install probe-rs-tools --locked",
        ),
        ("cargo-embed", tool_version(["cargo", "embed", "--version"]), "(with probe-rs-tools)"),
        ("zensical", tool_version([str(VENV_BIN / "zensical"), "--version"]), "dev.py setup"),
    ]
    installed_targets = run(
        ["rustup", "target", "list", "--installed"], exit_on_failure=False, quiet=True
    )
    target_installed = TARGET in installed_targets.stdout.split()
    tools.append(
        (
            TARGET,
            "installed" if target_installed else None,
            "rustup show (reads rust-toolchain.toml)",
        )
    )

    table = Table(box=None, header_style="table_header", pad_edge=False)
    for column in ("", "tool", "version", "to install"):
        table.add_column(column)
    for name, version, install_hint in tools:
        is_installed = version is not None
        table.add_row(
            "[success]✓[/]" if is_installed else "[error]✗[/]",
            name,
            version or "[error]missing[/]",
            "" if is_installed else f"[command]{install_hint}[/]",
        )
    console.print(table)
    console.print()
    boards_list()
    if any(version is None for _name, version, _hint in tools):
        log.warning("Some tools are missing (see above).")


def docs_serve(port: int, open_browser: bool) -> None:
    from devlib import stream

    stream(
        [
            str(VENV_BIN / "zensical"),
            "serve",
            "-f",
            "docs/zensical.toml",
            "-a",
            f"localhost:{port}",
        ]
        + (["--open"] if open_browser else [])
    )


def docs_build(regenerate_reference: bool) -> None:
    from devlib import done, task

    if regenerate_reference:
        task(
            "regenerate the reference pages",
            with_verbose(["cargo", "test-host"]),
            extra_environment={"UPDATE_DOCS": "1"},
            summarize=tests_summary,
        )
    task(
        "build the docs site",
        [str(VENV_BIN / "zensical"), "build", "--clean", "-f", "docs/zensical.toml"],
    )
    done(f"site in [path]{(ROOT / 'docs' / 'site').relative_to(ROOT)}/[/]")


def docs_api(open_browser: bool) -> None:
    from devlib import task

    task(
        "rustdoc: workshop-core",
        with_verbose(
            ["cargo", "doc", "-p", "workshop-core", "--target", "host-tuple", "--no-deps"]
        ),
    )
    task(
        "rustdoc: UART firmware",
        with_verbose(["cargo", "doc", "-p", "esp32-rust-workshop", "--no-deps"]),
    )
    task(
        "rustdoc: Ethernet firmware",
        with_verbose(["cargo", "doc", "--no-deps"] + (["--open"] if open_browser else [])),
        working_directory=APP_DIRS["eth"],
    )


def git(*arguments: str) -> str:
    """Runs a git command (stops the script if it fails); returns its output."""
    from devlib import run

    return run(["git", *arguments]).stdout.strip()


def current_branch() -> str:
    return git("branch", "--show-current")


def ensure_clean() -> None:
    from devlib import log

    if git("status", "--porcelain"):
        log.error("Working tree not clean: commit or stash first.")
        raise SystemExit(1)


def git_command(args: argparse.Namespace) -> None:
    from devlib import done, log

    match args.git_command:
        case "branch":
            ensure_clean()
            git("switch", args.base)
            git("pull", "--ff-only")
            git("switch", "-c", args.name)
            done(f"on new branch [package]{args.name}[/] (from {args.base})")
        case "commit":
            if args.all:
                git("add", "-A")
            message = f"{args.type}: {args.message}" if args.type else args.message
            git("commit", "-m", message)
            done(f"committed: {message}")
        case "push":
            branch = current_branch()
            git("push", *(["-u", "origin", branch] if args.set_upstream else []))
            done(f"pushed [package]{branch}[/]")
        case "squash":
            branch = current_branch()
            if branch == args.target:
                log.error("Already on the target branch.")
                raise SystemExit(1)
            ensure_clean()
            git("switch", args.target)
            git("pull", "--ff-only")
            git("merge", "--squash", branch)
            git("commit", "-m", f"merge({branch}): squash merge on {date.today()}")
            done(f"squash-merged [package]{branch}[/] into [package]{args.target}[/]")


def require_node(args: argparse.Namespace) -> None:
    """`flash` and `rtt` need a node; the other commands default to both."""
    if not hasattr(args, "node") or args.node == "all" or args.command == "boards":
        return
    if args.node is None and args.command in ("flash", "rtt"):
        sys.exit(f"`{args.command}` needs a node: a or b.")
    if args.command == "watch" and args.flash and args.node is None:
        sys.exit("`watch --flash` needs a node: a or b.")


def fft(tool_arguments: list[str]) -> None:
    """The computer side of adc-stream: a web page with the live spectrum (tools/adc-fft/)."""
    from devlib import log, stream

    if tool_arguments[:1] == ["--"]:
        tool_arguments = tool_arguments[1:]
    log.info(
        "live spectrum: [path]tools/adc-fft/adc_fft.py[/] (Ctrl+C to stop; `dev.py fft --help` for options)"
    )
    stream([str(VENV_PYTHON), "-u", str(FFT_TOOL), *tool_arguments])


def dash(tool_arguments: list[str]) -> None:
    """The dashboard of both boards (eth or uart app): a web page over their USB consoles (tools/node-dash/)."""
    from devlib import log, stream

    if tool_arguments[:1] == ["--"]:
        tool_arguments = tool_arguments[1:]
    log.info(
        "node dashboard: [path]tools/node-dash/node_dash.py[/] (Ctrl+C to stop; `dev.py dash --help` for options). "
        "Close `monitor` first: one program per serial port."
    )
    stream([str(VENV_PYTHON), "-u", str(DASH_TOOL), *tool_arguments])


def add_app_argument(command_parser: argparse.ArgumentParser) -> None:
    command_parser.add_argument(
        "--app",
        choices=("uart", "eth", "adc"),
        default="eth",
        help="the firmware: eth (default, board-to-board over Ethernet), uart (board-to-board over a UART) or adc (adc-stream: one board to a computer)",
    )


def add_preset_argument(command_parser: argparse.ArgumentParser) -> None:
    command_parser.add_argument(
        "--preset",
        metavar="NAME",
        help="build firmware/<app>/presets/NAME.toml instead of node-a / node-b",
    )


def add_node_argument(command_parser: argparse.ArgumentParser, optional: bool = False) -> None:
    add_app_argument(command_parser)
    if optional:
        command_parser.add_argument("node", nargs="?", choices=NODES, help="a or b")
    else:
        command_parser.add_argument("node", choices=NODES, help="a or b")
    command_parser.add_argument(
        "--board", metavar="MAC|PORT|#", help="this board instead of the node's assigned one"
    )


def add_debug_argument(command_parser: argparse.ArgumentParser) -> None:
    command_parser.add_argument(
        "--debug", action="store_true", help="debug profile instead of release"
    )


def add_verbose_argument(command_parser: argparse.ArgumentParser) -> None:
    command_parser.add_argument(
        "-v",
        "--verbose",
        action="count",
        default=0,
        help="cargo's -v (-vv for more), and its own output instead of the bar",
    )


def build_parser() -> argparse.ArgumentParser:
    root_parser = argparse.ArgumentParser(
        prog="dev",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    commands = root_parser.add_subparsers(dest="command", required=True, metavar="command")

    setup_parser = commands.add_parser(
        "setup", help="create .venv and install scripts/requirements.txt"
    )
    setup_parser.add_argument(
        "--python", help="interpreter to use (default: python3.13, 3.12, 3.14)"
    )

    commands.add_parser("doctor", help="check tools, Rust target and boards")

    boards_parser = commands.add_parser(
        "boards", help="list plugged boards, or assign them to nodes"
    )
    boards_commands = boards_parser.add_subparsers(dest="boards_command", metavar="assign")
    assign_parser = boards_commands.add_parser(
        "assign", help="remember which board is node A or B (or the ADC board)"
    )
    assign_parser.add_argument("node", choices=NODES)
    assign_parser.add_argument("board", help="end of its MAC (6C:1A), its port, or # from `boards`")

    build_command_parser = commands.add_parser(
        "build", help="build the firmware (both nodes by default)"
    )
    build_command_parser.add_argument(
        "node", nargs="?", choices=NODES, help="a or b (default: both)"
    )
    build_command_parser.add_argument(
        "--rtt", action="store_true", help="with the RTT console (feature rtt)"
    )
    add_app_argument(build_command_parser)
    add_preset_argument(build_command_parser)
    add_debug_argument(build_command_parser)
    add_verbose_argument(build_command_parser)

    size_parser = commands.add_parser(
        "size", help="flash and RAM used by the firmware (GCC-style table)"
    )
    size_parser.add_argument("node", nargs="?", choices=NODES, help="a or b (default: both)")
    add_app_argument(size_parser)
    add_debug_argument(size_parser)

    flash_parser = commands.add_parser(
        "flash", help="build and flash a node with espflash (or `all`)"
    )
    flash_parser.add_argument(
        "node",
        nargs="?",
        choices=[*NODES, "all"],
        help="a, b, or all (both, no monitor; not for --app adc)",
    )
    flash_parser.add_argument(
        "--board", metavar="MAC|PORT|#", help="this board instead of the assigned one"
    )
    flash_parser.add_argument(
        "-m", "--monitor", action="store_true", help="open the console afterwards"
    )
    flash_parser.add_argument(
        "--rtt", action="store_true", help="with the RTT console (feature rtt)"
    )
    add_app_argument(flash_parser)
    add_preset_argument(flash_parser)
    add_debug_argument(flash_parser)
    add_verbose_argument(flash_parser)

    monitor_parser = commands.add_parser(
        "monitor", help="serial console + CLI of a board (no flashing)"
    )
    add_node_argument(monitor_parser, optional=True)
    monitor_parser.add_argument(
        "--keep",
        action="store_true",
        help="attach without resetting the board: only what it prints from now on",
    )

    rtt_parser = commands.add_parser("rtt", help="RTT build + flash, then the cargo-embed console")
    add_node_argument(rtt_parser)
    add_preset_argument(rtt_parser)
    add_verbose_argument(rtt_parser)

    erase_parser = commands.add_parser(
        "erase", help="erase a board's whole flash (or only its saved settings)"
    )
    add_node_argument(erase_parser, optional=True)
    erase_parser.add_argument(
        "--settings",
        action="store_true",
        help="only the saved settings (the nvs partition): quicker, the firmware stays",
    )

    reset_parser = commands.add_parser(
        "reset", help="restart a board's firmware (no build, no flashing)"
    )
    add_node_argument(reset_parser, optional=True)

    watch_parser = commands.add_parser(
        "watch",
        help="rebuild on every saved source file (and reflash with --flash)",
        description="Builds, then waits for a change in the app, the shared crates or the Cargo "
        "settings and builds again. A failed build is shown and the next save tries again. "
        "With --flash the board is flashed after each good build (one node: a or b).",
    )
    add_node_argument(watch_parser, optional=True)
    add_preset_argument(watch_parser)
    watch_parser.add_argument(
        "-f", "--flash", action="store_true", help="flash the node after each build"
    )
    add_debug_argument(watch_parser)
    add_verbose_argument(watch_parser)

    fft_parser = commands.add_parser(
        "fft",
        help="live spectrum page for adc-stream (Plotly, in the browser); options go to the tool",
        description="Starts tools/adc-fft/adc_fft.py in .venv. Everything after `fft` is passed to it: "
        "`--demo` (simulated board), `--rate 40000`, `--no-browser`, `--http-port 8051`, `--selftest`, `--help`...",
    )
    fft_parser.add_argument(
        "tool_args", nargs=argparse.REMAINDER, metavar="...", help="passed to adc_fft.py"
    )

    dash_parser = commands.add_parser(
        "dash",
        help="dashboard of both boards (Ethernet or UART app): consoles, controls and charts, in the browser",
        description="Starts tools/node-dash/node_dash.py in .venv. Everything after `dash` is passed to it: "
        "`--demo [uart]` (simulated nodes), `--a PORT --b PORT`, `--no-browser`, `--http-port 8061`, `--selftest`, `--help`...",
    )
    dash_parser.add_argument(
        "tool_args", nargs=argparse.REMAINDER, metavar="...", help="passed to node_dash.py"
    )

    test_commands = commands.add_parser("test", help="host or on-target tests").add_subparsers(
        dest="test_command", required=True
    )
    test_host_parser = test_commands.add_parser("host", help="workshop-core tests on this computer")
    test_host_parser.add_argument(
        "-u", "--update-docs", action="store_true", help="regenerate the reference pages"
    )
    add_verbose_argument(test_host_parser)
    test_target_parser = test_commands.add_parser(
        "target", help="firmware/uart/tests/hil.rs on a board (probe-rs), then reflash it"
    )
    add_node_argument(test_target_parser, optional=True)
    test_target_parser.add_argument(
        "--keep", action="store_true", help="leave the test image on the board"
    )
    add_verbose_argument(test_target_parser)

    check_parser = commands.add_parser(
        "check", help="run what CI runs (tests, clippy, build, docs)"
    )
    check_parser.add_argument(
        "--fast",
        action="store_true",
        help="without the three firmware builds and the docs site (clippy still type-checks)",
    )
    add_verbose_argument(check_parser)

    format_parser = commands.add_parser("format", help="cargo fmt + black on scripts/")
    format_parser.add_argument("--check", action="store_true", help="only check, change nothing")

    docs_commands = commands.add_parser(
        "docs", help="documentation site and API docs"
    ).add_subparsers(dest="docs_command", required=True)
    docs_serve_parser = docs_commands.add_parser("serve", help="docs site with live reload")
    docs_serve_parser.add_argument("-p", "--port", type=int, default=3000, help="default: 3000")
    docs_serve_parser.add_argument(
        "-o", "--open", action="store_true", help="open it in the browser"
    )
    docs_build_parser = docs_commands.add_parser(
        "build", help="build the docs site into docs/site/"
    )
    docs_build_parser.add_argument(
        "-r", "--reference", action="store_true", help="regenerate the reference pages first"
    )
    add_verbose_argument(docs_build_parser)
    docs_api_parser = docs_commands.add_parser(
        "api", help="rustdoc of workshop-core and the firmware"
    )
    docs_api_parser.add_argument("-o", "--open", action="store_true", help="open it in the browser")
    add_verbose_argument(docs_api_parser)

    update_parser = commands.add_parser(
        "update", help="upgrade pip and the requirements that are outdated"
    )
    update_parser.add_argument("--dry-run", action="store_true", help="only show what's outdated")

    git_commands = commands.add_parser("git", help="git workflow helpers").add_subparsers(
        dest="git_command", required=True
    )
    branch_parser = git_commands.add_parser("branch", help="new branch from an up-to-date base")
    branch_parser.add_argument("name")
    branch_parser.add_argument("--base", default="main", help="default: main")
    commit_parser = git_commands.add_parser(
        "commit", help="commit, optionally with a conventional type"
    )
    commit_parser.add_argument("message")
    commit_parser.add_argument("-a", "--all", action="store_true", help="stage everything first")
    commit_parser.add_argument(
        "-t", "--type", choices=COMMIT_TYPES, help="prefix: `<type>: message`"
    )
    push_parser = git_commands.add_parser("push", help="push the current branch")
    push_parser.add_argument("-u", "--set-upstream", action="store_true")
    squash_parser = git_commands.add_parser(
        "squash", help="squash-merge the current branch into TARGET"
    )
    squash_parser.add_argument("target")
    return root_parser


def notify_if_long(command: str, started_at: float, succeeded: bool) -> None:
    """Desktop notification for a command that took a while (silenced with DEV_NOTIFY=0)."""
    from devlib import fmt_secs, notify

    elapsed = time.monotonic() - started_at
    if command in NOTIFY_COMMANDS and elapsed >= NOTIFY_AFTER_SECONDS:
        notify(
            f"dev.py {command}", f"{'done' if succeeded else 'failed'} after {fmt_secs(elapsed)}"
        )


def main() -> None:
    # Everything after `fft` / `dash` belongs to the tool (argparse would choke on its options).
    command_line_arguments = sys.argv[1:]
    if command_line_arguments[:1] and command_line_arguments[0].startswith("-"):
        if command_line_arguments[0] not in ("-h", "--help"):
            sys.exit(
                "The command comes first, its options after it: dev <command> [options], e.g. dev flash all --app eth --rtt"
            )
    tool_arguments: list[str] = []
    for tool_command in ("fft", "dash"):
        if tool_command in command_line_arguments:
            position = command_line_arguments.index(tool_command)
            command_line_arguments, tool_arguments = (
                command_line_arguments[: position + 1],
                command_line_arguments[position + 1 :],
            )
            break
    args = build_parser().parse_args(command_line_arguments)
    if args.command in ("fft", "dash"):
        args.tool_args = tool_arguments
    if args.command == "setup":
        setup(args.python)
        return
    if not in_venv():
        if not VENV_PYTHON.exists():
            print("No .venv yet: creating it (once, about a minute).")
            setup(None)
        os.execv(VENV_PYTHON, [str(VENV_PYTHON), __file__, *sys.argv[1:]])

    sys.path.insert(0, str(SCRIPTS))
    try:
        import serial  # noqa: F401  (pyserial, added to requirements.txt later)
    except ImportError:
        if args.command not in ("update", "git", "docs", "format"):
            sys.exit("pyserial is missing from .venv: run `python3 scripts/dev.py update`.")

    global APP, PRESET, VERBOSE
    APP, PRESET = getattr(args, "app", "eth"), getattr(args, "preset", None)
    VERBOSE = getattr(args, "verbose", 0)
    import devlib

    devlib.RAW = VERBOSE > 0
    release = not getattr(args, "debug", False)
    require_node(args)
    started_at = time.monotonic()
    try:
        run_command(args, release)
    except SystemExit as exit_request:
        notify_if_long(args.command, started_at, succeeded=exit_request.code in (0, None))
        raise
    notify_if_long(args.command, started_at, succeeded=True)


def run_command(args: argparse.Namespace, release: bool) -> None:
    match args.command:
        case "doctor":
            doctor()
        case "boards":
            if args.boards_command == "assign":
                boards_assign(args.node, args.board)
            else:
                boards_list()
        case "build":
            cargo_build([args.node] if args.node else list(app_nodes()), args.rtt, release)
        case "size":
            memory_usage([args.node] if args.node else list(app_nodes()), release)
        case "fft":
            fft(args.tool_args)
        case "dash":
            dash(args.tool_args)
        case "flash":
            if args.node == "all":
                flash_all(release, args.rtt)
            else:
                flash(args.node, args.board, args.monitor, args.rtt, release)
        case "monitor":
            monitor(args.node, args.board, args.keep)
        case "rtt":
            rtt(args.node, args.board)
        case "reset":
            reset(args.node, args.board)
        case "watch":
            watch(args.node, args.board, args.flash, release)
        case "erase":
            erase(args.node, args.board, args.settings)
        case "test":
            if args.test_command == "host":
                test_host(args.update_docs)
            else:
                test_target(args.node, args.board, args.keep)
        case "check":
            check(args.fast)
        case "format":
            format_code(args.check)
        case "docs":
            match args.docs_command:
                case "serve":
                    docs_serve(args.port, args.open)
                case "build":
                    docs_build(args.reference)
                case "api":
                    docs_api(args.open)
        case "update":
            update(args.dry_run)
        case "git":
            git_command(args)


if __name__ == "__main__":
    main()
