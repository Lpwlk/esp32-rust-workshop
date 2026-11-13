"""Shared helpers for `scripts/dev.py`: rich console + log file, running commands."""

from __future__ import annotations

import logging
import os
import signal
import subprocess
from pathlib import Path

from rich.console import Console
from rich.logging import RichHandler
from rich.markup import escape
from rich.theme import Theme

ROOT = Path(__file__).resolve().parent.parent
LOG_FILE = Path(__file__).resolve().parent / "logs" / "dev.log"

# Prefixes to draw a small tree under a log line.
BRANCH = " ├── "
LAST = " └── "

THEME = Theme(
    {
        "path": "italic yellow",
        "cmd": "italic grey62",
        "pkg": "italic yellow",
        "ver": "bold green",
        "old": "bold red",
        "logging.level.info": "yellow",
        "log.time": "steel_blue3",
    }
)
console = Console(theme=THEME, highlight=False)


def _make_logger() -> logging.Logger:
    """Info and above on screen; everything (command output included) in the log file."""
    LOG_FILE.parent.mkdir(exist_ok=True)
    logger = logging.getLogger("dev")
    if logger.handlers:  # imported twice: don't add the handlers again
        return logger
    logger.setLevel(logging.DEBUG)

    screen = RichHandler(
        console=console,
        markup=True,
        show_path=False,
        omit_repeated_times=True,
        log_time_format="%H:%M:%S",
        rich_tracebacks=True,
    )
    screen.setLevel(logging.INFO)

    file = RichHandler(
        console=Console(
            file=LOG_FILE.open("a", encoding="utf-8"), theme=THEME, no_color=True, width=120
        ),
        markup=True,
        show_path=False,
        omit_repeated_times=False,
        log_time_format="%Y-%m-%d %H:%M:%S",
    )
    file.setLevel(logging.DEBUG)

    logger.addHandler(screen)
    logger.addHandler(file)
    return logger


log = _make_logger()


def shown(cmd: list[str]) -> str:
    """A command as a human would type it: tools of `.venv` by their bare name."""
    first = Path(cmd[0])
    if first.is_absolute() and (ROOT / ".venv") in first.parents:
        first = Path(first.name)
    return " ".join([str(first), *cmd[1:]])


def run(
    cmd: list[str],
    *,
    check: bool = True,
    extra_env: dict[str, str] | None = None,
    prefix: str = "",
    quiet: bool = False,
) -> subprocess.CompletedProcess[str]:
    """Runs `cmd` with a spinner, logs it, keeps its output in the log file.

    With `check`, a failure prints the end of its output and stops the script
    with the command's exit code. `quiet` keeps the command line out of the
    screen (it still goes to the log file).
    """
    (log.debug if quiet else log.info)(f"{prefix}[cmd]$ {escape(shown(cmd))}[/]")
    env = {**os.environ, **extra_env} if extra_env else None
    with console.status(f"[cmd]{escape(shown(cmd))}[/]", spinner="dots"):
        result = subprocess.run(cmd, cwd=ROOT, env=env, text=True, capture_output=True)

    output = (result.stdout + result.stderr).strip()
    if output:
        log.debug(f"[dim]{escape(output)}[/]")
    if check and result.returncode != 0:
        tail = "\n".join(output.splitlines()[-15:])
        log.error(f"Command failed (exit {result.returncode}):\n[dim]{escape(tail)}[/]")
        raise SystemExit(result.returncode)
    return result


def live(
    cmd: list[str],
    *,
    check: bool = True,
    extra_env: dict[str, str] | None = None,
    cwd: Path | None = None,
) -> int:
    """Runs `cmd` in the terminal as if typed there: its output (cargo's colors and
    errors, a serial monitor, the RTT console) isn't captured, and it reads the
    keyboard. Ctrl+C goes to the command only, which decides how to stop.

    With `check`, a failure stops the script with the command's exit code."""
    shown_env = "".join(f"{k}={v} " for k, v in (extra_env or {}).items())
    log.info(f"[cmd]$ {escape(shown_env + shown(cmd))}[/]")
    env = {**os.environ, **extra_env} if extra_env else None
    previous = signal.signal(signal.SIGINT, signal.SIG_IGN)
    try:
        # The child gets the default Ctrl+C handling back.
        code = subprocess.call(
            cmd,
            cwd=cwd or ROOT,
            env=env,
            preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL),
        )
    finally:
        signal.signal(signal.SIGINT, previous)
    log.debug(f"exit {code}: {shown(cmd)}")
    if check and code != 0:
        log.error(f"Command failed (exit {code}): [cmd]{escape(shown(cmd))}[/]")
        raise SystemExit(code)
    return code


def stream(cmd: list[str]) -> int:
    """Runs a long command (dev server): its output goes to the log line by line,
    and Ctrl+C stops it cleanly. Returns its exit code."""
    log.info(f"[cmd]$ {escape(shown(cmd))}[/]  (Ctrl+C to stop)")
    proc = subprocess.Popen(
        cmd, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1
    )
    try:
        assert proc.stdout is not None
        for line in proc.stdout:
            log.info(f"[dim]{escape(line.rstrip())}[/]")
    except KeyboardInterrupt:
        log.info("Ctrl+C: stopping …")
        proc.terminate()
    try:
        return proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        return proc.wait()
