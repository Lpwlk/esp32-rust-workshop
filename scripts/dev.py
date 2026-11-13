#!/usr/bin/env python3
"""Dev tasks for this repository: ESP32-S31 boards, tests, docs site, Python tooling, git.

    python3 scripts/dev.py setup                 # once: .venv (Python 3.13) + requirements
    python3 scripts/dev.py doctor                # tools, Rust target, boards: what's missing

  Boards (node = a | b)
    python3 scripts/dev.py boards                # plugged boards: MAC, port, probe, node
    python3 scripts/dev.py boards assign a 6C:1A # this board is node A (remembered)
    python3 scripts/dev.py build [a|b]           # firmware, release (both by default)
    python3 scripts/dev.py size [a|b]            # flash / RAM used (also after each build)
    python3 scripts/dev.py flash a [-m]          # build + flash with espflash (+ monitor)
    python3 scripts/dev.py flash all             # both nodes on their boards
    python3 scripts/dev.py monitor a             # serial console + CLI, backtraces decoded
    python3 scripts/dev.py rtt a                 # RTT build, flash, cargo-embed console
    python3 scripts/dev.py erase a               # erase the whole flash

  Tests and quality
    python3 scripts/dev.py test host [-u]        # core tests on this computer (-u: regen docs)
    python3 scripts/dev.py test target [a]       # firmware/uart/tests/hil.rs on a board, then reflash it
    python3 scripts/dev.py check                 # what CI runs, locally
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
"""

from __future__ import annotations

# Standard library only up to `main()`: `setup` must work before rich is installed.
import argparse
import json
import os
import shutil
import subprocess
import sys
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"
VENV = ROOT / ".venv"
VENV_BIN = VENV / ("Scripts" if os.name == "nt" else "bin")
VENV_PY = VENV_BIN / "python"
REQUIREMENTS = SCRIPTS / "requirements.txt"
# Zensical's dependencies don't support Python 3.15 yet: take the first one found.
PYTHONS = ["python3.13", "python3.12", "python3.14"]
COMMIT_TYPES = ["feat", "fix", "chore", "refactor", "test", "docs"]

# Firmware (see Cargo.toml and .cargo/config.toml).
CHIP = "esp32s31"
TARGET = "riscv32imafc-unknown-none-elf"
NODES = ("a", "b")


def in_venv() -> bool:
    return Path(sys.prefix).resolve() == VENV.resolve()


# --------------------------------------------------------------------------------------
# setup (standard library only)
# --------------------------------------------------------------------------------------


def setup(python: str | None) -> None:
    candidates = [python] if python else PYTHONS
    interpreter = next((shutil.which(p) for p in candidates if p and shutil.which(p)), None)
    if not interpreter:
        sys.exit(f"No suitable Python found (tried {', '.join(candidates)}).")
    print(f"• creating {VENV.relative_to(ROOT)} with {interpreter}")
    subprocess.run([interpreter, "-m", "venv", "--clear", str(VENV)], check=True)
    pip = [str(VENV_PY), "-m", "pip", "--disable-pip-version-check"]
    print("• upgrading pip")
    subprocess.run([*pip, "install", "--quiet", "--upgrade", "pip"], check=True)
    print(f"• installing {REQUIREMENTS.relative_to(ROOT)}")
    subprocess.run([*pip, "install", "--quiet", "-r", str(REQUIREMENTS)], check=True)
    print("✓ ready: python3 scripts/dev.py doctor")


# --------------------------------------------------------------------------------------
# Python tooling (inside .venv, rich available)
# --------------------------------------------------------------------------------------


def requirement_names() -> list[str]:
    """Package names from requirements.txt (version specifiers and comments dropped)."""
    names = []
    for line in REQUIREMENTS.read_text().splitlines():
        line = line.split("#")[0].strip()
        if line:
            for sep in "<>=!~[ ;":
                line = line.split(sep)[0]
            names.append(line)
    return names


def update(dry_run: bool) -> None:
    from devlib import BRANCH, LAST, log, run

    wanted = ["pip", *requirement_names()]
    log.info(f"Checking {', '.join(f'[pkg]{n}[/]' for n in wanted)} …")
    pip = [str(VENV_PY), "-m", "pip", "--disable-pip-version-check"]
    # Packages missing from .venv (requirements.txt grew since `setup`).
    installed = {p["name"].lower() for p in json.loads(run([*pip, "list", "--format=json"]).stdout)}
    missing = [n for n in wanted if n.lower() not in installed]
    if missing:
        log.info(f"{BRANCH}missing: {', '.join(f'[pkg]{n}[/]' for n in missing)}")
        if not dry_run:
            run([*pip, "install", "--quiet", "-r", str(REQUIREMENTS)])
    # One call, structured output: no parsing of human-readable text.
    outdated = json.loads(run([*pip, "list", "--outdated", "--format=json"]).stdout)
    to_upgrade = [p for p in outdated if p["name"].lower() in {w.lower() for w in wanted}]

    if not to_upgrade:
        log.info(f"{LAST}everything is up to date")
        return
    for i, p in enumerate(to_upgrade):
        tree = LAST if i == len(to_upgrade) - 1 else BRANCH
        log.info(
            f"{tree}[pkg]{p['name']}[/] [old]{p['version']}[/] → [ver]{p['latest_version']}[/]"
        )
    if dry_run:
        log.info("dry run: nothing installed")
        return
    run([*pip, "install", "--upgrade", *(p["name"] for p in to_upgrade)])
    log.info("✓ updated")


def format_code(check: bool) -> None:
    from devlib import live, log, run

    live(["cargo", "fmt", "--all"] + (["--", "--check"] if check else []))
    # firmware/ethernet is its own Cargo workspace: `--all` at the root doesn't reach it.
    live(["cargo", "fmt"] + (["--", "--check"] if check else []), cwd=APP_DIRS["eth"])
    run(
        [str(VENV_BIN / "black"), "--line-length", "100"]
        + (["--check"] if check else [])
        + [str(SCRIPTS.relative_to(ROOT))]
    )
    log.info("✓ formatted" if not check else "✓ already formatted")


# --------------------------------------------------------------------------------------
# Boards and firmware
# --------------------------------------------------------------------------------------


# Which firmware the build / flash / monitor commands work on: `--app uart` (the repository root, the
# default) or `--app eth` (firmware/ethernet/, the Ethernet app: its own Cargo workspace). `--preset NAME`
# builds that preset (firmware/<app>/presets/NAME.toml) instead of node-a / node-b.
APP = "uart"
PRESET: str | None = None
APP_DIRS = {"uart": ROOT / "firmware" / "uart", "eth": ROOT / "firmware" / "ethernet"}
# The UART app is a member of the root workspace (one `target/` at the root); the Ethernet app is its own.
TARGET_DIRS = {"uart": ROOT / "target", "eth": ROOT / "firmware" / "ethernet" / "target"}


def app_dir() -> Path:
    return APP_DIRS[APP]


def preset_env() -> dict[str, str] | None:
    return {"PRESET": PRESET} if PRESET else None


def elf(node: str, release: bool = True) -> Path:
    return TARGET_DIRS[APP] / TARGET / ("release" if release else "debug") / f"node-{node}"


def board_for(node: str | None, selector: str | None):
    """The board to use, or a clear message and exit."""
    from boards import BoardError, resolve
    from devlib import log

    try:
        board = resolve(node, selector)
    except BoardError as e:
        log.error(str(e))
        raise SystemExit(1)
    label = f"node {node.upper()} → " if node else ""
    log.info(f"{label}board [pkg]{board.short}[/] on [path]{board.port}[/]")
    return board


def boards_list() -> None:
    from boards import detect, load_assignments, node_of
    from devlib import console, log
    from rich.table import Table

    found = detect()
    if not found:
        log.warning("No ESP32-S31 on USB. Plug it by its USB Serial/JTAG port.")
    else:
        table = Table(box=None, header_style="bold", pad_edge=False)
        for col in ("#", "node", "MAC", "port", "probe (probe-rs)"):
            table.add_column(col)
        for i, b in enumerate(found):
            node = node_of(b)
            table.add_row(str(i), node.upper() if node else "[dim]-[/]", b.mac, b.port, b.probe)
        console.print(table)
    plugged = {b.mac for b in found}
    for node, mac in load_assignments().items():
        if mac not in plugged:
            log.info(f"node {node.upper()}: board {mac} not plugged in")
    if found and not load_assignments():
        log.info("Tell them apart: `dev.py boards assign a <MAC end>`, e.g. " + found[0].short)


def boards_assign(node: str, selector: str) -> None:
    from boards import BoardError, detect, match, save_assignment
    from devlib import log

    try:
        board = match(detect(), selector)
    except BoardError as e:
        log.error(str(e))
        raise SystemExit(1)
    save_assignment(node, board.mac)
    log.info(f"✓ node {node.upper()} = board [pkg]{board.mac}[/] ([path]{board.port}[/])")


def cargo_build(nodes: list[str], rtt: bool, release: bool) -> None:
    from devlib import live

    live(
        ["cargo", "build"]
        + (["--release"] if release else [])
        + (["--features", "rtt"] if rtt else [])
        + [arg for n in nodes for arg in ("--bin", f"node-{n}")],
        cwd=app_dir(),
        extra_env=preset_env(),
    )
    memory_usage(nodes, release)


def memory_usage(nodes: list[str], release: bool) -> None:
    """Flash / RAM table of each node, like GCC's `--print-memory-usage`, with the
    change since the previous build (scripts/memsize.py)."""
    from devlib import console, log
    from memsize import report
    from rich.table import Table

    for node in nodes:
        path = elf(node, release)
        if not path.exists():
            log.warning(f"node-{node}: not built yet (`dev.py build {node}`)")
            continue
        last = path.parent / f".memsize-node-{node}.json"
        table = Table(title=f"node-{node} memory", title_justify="left", box=None, pad_edge=False)
        for col in ("Memory region", "Used Size", "Region Size", "%age Used", "Change"):
            table.add_column(col, justify="right")
        for region, used, size, pct, change in report(path, last):
            style = "red" if change.startswith("+") else "green" if change else ""
            table.add_row(region, used, size, pct, f"[{style}]{change}[/]" if style else "")
        console.print(table)


def flash(node: str, selector: str | None, monitor: bool, rtt: bool, release: bool) -> None:
    from devlib import live

    board = board_for(node, selector)
    cargo_build([node], rtt, release)
    live(
        ["espflash", "flash", "--chip", CHIP, "--port", board.port, "--skip-update-check"]
        + (["--monitor"] if monitor else [])
        + [str(elf(node, release))]
    )


def flash_all(release: bool) -> None:
    from boards import BoardError, resolve
    from devlib import log

    try:
        # Both boards first: don't flash A and then find out B isn't there.
        targets = {n: resolve(n, None) for n in NODES}
    except BoardError as e:
        log.error(f"{e}\n`flash all` needs both nodes assigned: `dev.py boards assign a|b <MAC>`.")
        raise SystemExit(1)
    if targets["a"] == targets["b"]:
        log.error("Nodes A and B are the same board: assign the other one.")
        raise SystemExit(1)
    for node in NODES:
        flash(node, targets[node].mac, monitor=False, rtt=False, release=release)
    log.info("✓ both nodes flashed: `dev.py monitor a` / `dev.py monitor b` (two terminals)")


def monitor(node: str | None, selector: str | None) -> None:
    from devlib import live, log

    board = board_for(node, selector)
    cmd = ["espflash", "monitor", "--chip", CHIP, "--port", board.port, "--skip-update-check"]
    if node and elf(node).exists():
        # Lets espflash turn backtrace addresses into function names.
        cmd += ["--elf", str(elf(node))]
    elif node:
        log.info(f"no {elf(node).relative_to(ROOT)} yet: backtraces won't be decoded")
    log.info("Ctrl+C to quit")
    live(cmd, check=False)


def rtt(node: str, selector: str | None) -> None:
    """RTT console through the probe. Flashing goes through espflash (the probe-rs 0.32
    revision trap, see the Rust guide); cargo-embed only attaches (firmware/uart/Embed.toml)."""
    from devlib import live

    board = board_for(node, selector)
    flash(node, board.mac, monitor=False, rtt=True, release=True)
    live(
        ["cargo", "embed", "--release", "--features", "rtt", "--bin", f"node-{node}"]
        + ["--probe", board.probe],
        cwd=app_dir(),
        check=False,
    )


def erase(node: str | None, selector: str | None) -> None:
    from devlib import live

    board = board_for(node, selector)
    live(["espflash", "erase-flash", "--chip", CHIP, "--port", board.port, "--skip-update-check"])


# --------------------------------------------------------------------------------------
# Tests and checks
# --------------------------------------------------------------------------------------


def test_host(update_docs: bool) -> None:
    from devlib import live, log

    live(["cargo", "test-host"], extra_env={"UPDATE_DOCS": "1"} if update_docs else None)
    if update_docs:
        log.info("reference pages regenerated: review and commit docs/book/reference")


def test_target(node: str | None, selector: str | None, keep: bool) -> None:
    from devlib import live, log

    board = board_for(node, selector)
    code = live(["cargo", "test-target"], extra_env={"PROBE_RS_PROBE": board.probe}, check=False)
    # probe-rs left the test image in flash, with the wrong minimum chip revision:
    # the board would boot-loop. Put the node's firmware back with espflash.
    if node and not keep:
        log.info(f"reflashing node {node.upper()}'s firmware (the tests replaced it)")
        flash(node, board.mac, monitor=False, rtt=False, release=True)
    elif not keep:
        log.warning(
            "The board now holds the test image: reflash it (`dev.py flash a|b --board "
            f"{board.short}`)."
        )
    if code != 0:
        raise SystemExit(code)


def check() -> None:
    """The CI pipeline (.github/workflows/ci.yml), in the same order, stopping at the
    first failure. On-target tests aren't in CI (no board there): `test target`."""
    from devlib import live, log, run

    steps = [
        ("host tests + reference pages up to date", ["cargo", "test-host"]),
        (
            "clippy, core (host)",
            "cargo clippy -p workshop-core --target host-tuple --all-targets -- -D warnings".split(),
        ),
        *[
            (
                f"example {ex.stem} (shown in the docs)",
                f"cargo run -q -p workshop-core --target host-tuple --example {ex.stem}".split(),
            )
            for ex in sorted((ROOT / "firmware/shared/core/examples").glob("*.rs"))
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
    ]
    for i, (name, cmd, *where) in enumerate(steps, 1):
        log.info(f"[bold]{i}/{len(steps) + 1}[/] {name}")
        live(cmd, cwd=where[0] if where else None)
    log.info(f"[bold]{len(steps) + 1}/{len(steps) + 1}[/] docs site")
    run([str(VENV_BIN / "zensical"), "build", "--clean", "-f", "docs/zensical.toml"])
    log.info("✓ all green")


def doctor() -> None:
    """What's installed, what's missing, what's plugged in."""
    from devlib import console, log, run
    from rich.table import Table

    def version(cmd: list[str]) -> str | None:
        if not shutil.which(cmd[0]):
            return None
        out = run(cmd, check=False, quiet=True)
        return (out.stdout or out.stderr).strip().splitlines()[0] if out.returncode == 0 else None

    rows = [
        ("cargo", version(["cargo", "--version"]), "https://rustup.rs"),
        ("espflash", version(["espflash", "--version"]), "cargo install espflash --locked"),
        ("probe-rs", version(["probe-rs", "--version"]), "cargo install probe-rs-tools --locked"),
        ("cargo-embed", version(["cargo", "embed", "--version"]), "(with probe-rs-tools)"),
        ("zensical", version([str(VENV_BIN / "zensical"), "--version"]), "dev.py setup"),
    ]
    targets = run(["rustup", "target", "list", "--installed"], check=False, quiet=True)
    has_target = TARGET in targets.stdout.split()
    rows.append(
        (TARGET, "installed" if has_target else None, "rustup show (reads rust-toolchain.toml)")
    )

    table = Table(box=None, header_style="bold", pad_edge=False)
    for col in ("", "tool", "version", "to install"):
        table.add_column(col)
    for name, ver, how in rows:
        ok = ver is not None
        table.add_row(
            "[green]✓[/]" if ok else "[red]✗[/]",
            name,
            ver or "[red]missing[/]",
            "" if ok else f"[cmd]{how}[/]",
        )
    console.print(table)
    console.print()
    boards_list()
    if any(ver is None for _, ver, _ in rows):
        log.warning("Some tools are missing (see above).")


# --------------------------------------------------------------------------------------
# Documentation
# --------------------------------------------------------------------------------------


def docs_serve(port: int, open_browser: bool) -> None:
    from devlib import stream

    stream(
        [str(VENV_BIN / "zensical"), "serve", "-f", "docs/zensical.toml", "-a", f"localhost:{port}"]
        + (["--open"] if open_browser else [])
    )


def docs_build(reference: bool) -> None:
    from devlib import live, log, run

    if reference:
        # The reference pages are generated from the Rust code (and checked by CI).
        live(["cargo", "test-host"], extra_env={"UPDATE_DOCS": "1"})
    run([str(VENV_BIN / "zensical"), "build", "--clean", "-f", "docs/zensical.toml"])
    log.info(f"✓ site in [path]{(ROOT / 'docs' / 'site').relative_to(ROOT)}/[/]")


def docs_api(open_browser: bool) -> None:
    from devlib import live

    live(["cargo", "doc", "-p", "workshop-core", "--target", "host-tuple", "--no-deps"])
    live(
        ["cargo", "doc", "-p", "esp32-rust-workshop", "--no-deps"]
        + (["--open"] if open_browser else [])
    )


# --- git workflow ------------------------------------------------------------------------


def git(*args: str) -> str:
    """Runs a git command (stops the script if it fails); returns its output."""
    from devlib import run

    return run(["git", *args]).stdout.strip()


def current_branch() -> str:
    return git("branch", "--show-current")


def ensure_clean() -> None:
    from devlib import log

    if git("status", "--porcelain"):
        log.error("Working tree not clean: commit or stash first.")
        raise SystemExit(1)


def git_command(args: argparse.Namespace) -> None:
    from devlib import log

    match args.git_command:
        case "branch":
            ensure_clean()
            git("switch", args.base)
            git("pull", "--ff-only")
            git("switch", "-c", args.name)
            log.info(f"✓ on new branch [pkg]{args.name}[/] (from {args.base})")
        case "commit":
            if args.all:
                git("add", "-A")
            message = f"{args.type}: {args.message}" if args.type else args.message
            git("commit", "-m", message)
            log.info(f"✓ committed: {message}")
        case "push":
            branch = current_branch()
            git("push", *(["-u", "origin", branch] if args.set_upstream else []))
            log.info(f"✓ pushed [pkg]{branch}[/]")
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
            log.info(f"✓ squash-merged [pkg]{branch}[/] into [pkg]{args.target}[/]")


# --------------------------------------------------------------------------------------


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="dev.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument(
        "--app",
        choices=("uart", "eth"),
        default="uart",
        help="firmware that build / size / flash / monitor / rtt work on: uart (default) or eth (firmware/ethernet/)",
    )
    p.add_argument(
        "--preset",
        metavar="NAME",
        help="build firmware/<app>/presets/NAME.toml instead of node-a / node-b",
    )
    sub = p.add_subparsers(dest="command", required=True, metavar="command")

    def node_arg(s: argparse.ArgumentParser, optional: bool = False) -> None:
        if optional:
            s.add_argument("node", nargs="?", choices=NODES, help="a or b")
        else:
            s.add_argument("node", choices=NODES, help="a or b")
        s.add_argument(
            "--board", metavar="MAC|PORT|#", help="this board instead of the node's assigned one"
        )

    def debug_arg(s: argparse.ArgumentParser) -> None:
        s.add_argument("--debug", action="store_true", help="debug profile instead of release")

    s = sub.add_parser("setup", help="create .venv and install scripts/requirements.txt")
    s.add_argument("--python", help="interpreter to use (default: python3.13, 3.12, 3.14)")

    sub.add_parser("doctor", help="check tools, Rust target and boards")

    # --- boards and firmware ---
    s = sub.add_parser("boards", help="list plugged boards, or assign them to nodes")
    b = s.add_subparsers(dest="boards_command", metavar="assign")
    s = b.add_parser("assign", help="remember which board is node A or B")
    s.add_argument("node", choices=NODES)
    s.add_argument("board", help="end of its MAC (6C:1A), its port, or # from `boards`")

    s = sub.add_parser("build", help="build the firmware (both nodes by default)")
    s.add_argument("node", nargs="?", choices=NODES, help="a or b (default: both)")
    s.add_argument("--rtt", action="store_true", help="with the RTT console (feature rtt)")
    debug_arg(s)

    s = sub.add_parser("size", help="flash and RAM used by the firmware (GCC-style table)")
    s.add_argument("node", nargs="?", choices=NODES, help="a or b (default: both)")
    debug_arg(s)

    s = sub.add_parser("flash", help="build and flash a node with espflash (or `all`)")
    s.add_argument("node", choices=[*NODES, "all"], help="a, b, or all (both, no monitor)")
    s.add_argument("--board", metavar="MAC|PORT|#", help="this board instead of the assigned one")
    s.add_argument("-m", "--monitor", action="store_true", help="open the console afterwards")
    s.add_argument("--rtt", action="store_true", help="with the RTT console (feature rtt)")
    debug_arg(s)

    s = sub.add_parser("monitor", help="serial console + CLI of a board (no flashing)")
    node_arg(s, optional=True)

    s = sub.add_parser("rtt", help="RTT build + flash, then the cargo-embed console")
    node_arg(s)

    s = sub.add_parser("erase", help="erase a board's whole flash")
    node_arg(s, optional=True)

    # --- tests ---
    t = sub.add_parser("test", help="host or on-target tests").add_subparsers(
        dest="test_command", required=True
    )
    s = t.add_parser("host", help="workshop-core tests on this computer")
    s.add_argument(
        "-u", "--update-docs", action="store_true", help="regenerate the reference pages"
    )
    s = t.add_parser(
        "target", help="firmware/uart/tests/hil.rs on a board (probe-rs), then reflash it"
    )
    node_arg(s, optional=True)
    s.add_argument("--keep", action="store_true", help="leave the test image on the board")

    sub.add_parser("check", help="run what CI runs (tests, clippy, build, docs)")

    s = sub.add_parser("format", help="cargo fmt + black on scripts/")
    s.add_argument("--check", action="store_true", help="only check, change nothing")

    # --- docs ---
    d = sub.add_parser("docs", help="documentation site and API docs").add_subparsers(
        dest="docs_command", required=True
    )
    s = d.add_parser("serve", help="docs site with live reload")
    s.add_argument("-p", "--port", type=int, default=3000, help="default: 3000")
    s.add_argument("-o", "--open", action="store_true", help="open it in the browser")
    s = d.add_parser("build", help="build the docs site into docs/site/")
    s.add_argument(
        "-r", "--reference", action="store_true", help="regenerate the reference pages first"
    )
    s = d.add_parser("api", help="rustdoc of workshop-core and the firmware")
    s.add_argument("-o", "--open", action="store_true", help="open it in the browser")

    # --- tooling ---
    s = sub.add_parser("update", help="upgrade pip and the requirements that are outdated")
    s.add_argument("--dry-run", action="store_true", help="only show what's outdated")

    g = sub.add_parser("git", help="git workflow helpers").add_subparsers(
        dest="git_command", required=True
    )
    s = g.add_parser("branch", help="new branch from an up-to-date base")
    s.add_argument("name")
    s.add_argument("--base", default="main", help="default: main")
    s = g.add_parser("commit", help="commit, optionally with a conventional type")
    s.add_argument("message")
    s.add_argument("-a", "--all", action="store_true", help="stage everything first")
    s.add_argument("-t", "--type", choices=COMMIT_TYPES, help="prefix: `<type>: message`")
    s = g.add_parser("push", help="push the current branch")
    s.add_argument("-u", "--set-upstream", action="store_true")
    s = g.add_parser("squash", help="squash-merge the current branch into TARGET")
    s.add_argument("target")
    return p


def main() -> None:
    args = parser().parse_args()
    if args.command == "setup":
        setup(args.python)
        return
    if not in_venv():
        if not VENV_PY.exists():
            sys.exit("No .venv yet: run `python3 scripts/dev.py setup` first.")
        os.execv(VENV_PY, [str(VENV_PY), __file__, *sys.argv[1:]])

    sys.path.insert(0, str(SCRIPTS))
    try:
        import serial  # noqa: F401  (pyserial, added to requirements.txt later)
    except ImportError:
        if args.command not in ("update", "git", "docs", "format"):
            sys.exit("pyserial is missing from .venv: run `python3 scripts/dev.py update`.")

    global APP, PRESET
    APP, PRESET = args.app, args.preset
    release = not getattr(args, "debug", False)
    match args.command:
        case "doctor":
            doctor()
        case "boards":
            if args.boards_command == "assign":
                boards_assign(args.node, args.board)
            else:
                boards_list()
        case "build":
            cargo_build([args.node] if args.node else list(NODES), args.rtt, release)
        case "size":
            memory_usage([args.node] if args.node else list(NODES), release)
        case "flash":
            if args.node == "all":
                flash_all(release)
            else:
                flash(args.node, args.board, args.monitor, args.rtt, release)
        case "monitor":
            monitor(args.node, args.board)
        case "rtt":
            rtt(args.node, args.board)
        case "erase":
            erase(args.node, args.board)
        case "test":
            if args.test_command == "host":
                test_host(args.update_docs)
            else:
                test_target(args.node, args.board, args.keep)
        case "check":
            check()
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
