"""Shared helpers for `scripts/dev.py`: the display, the log file, running commands.

What you see: a progress bar at the bottom of the terminal while a command runs (spinner, pulsing
bar since nobody knows the total, elapsed time, the line the command is printing). When the command
ends the bar goes and a log line takes its place: time, level (DONE, WARNING, ERROR), what it did,
how long it took, and the command that ran. The log is a rich `RichHandler`; the colors come from
the theme chosen in `devthemes.py`.

`scripts/logs/dev.log` gets everything with timestamps: the messages, the commands, and the full
output of every command, shown or not.

A command runs in one of four ways, by what it is for:

  task()     work with a result: build, flash, tests, formatting. Its output is captured. A failure,
             or warnings, print the captured output in full, in color.
  run()      the same, for a command whose output the script reads (pip's JSON, git). A `quiet` one
             (a version check) leaves nothing on screen.
  live()     a command that talks to you or never ends (a serial console): it owns the terminal.
  stream()   a dev server: its output scrolls until Ctrl+C.

`RAW = True` (dev.py sets it with `-v`) turns task() into live(): cargo's own output.
run_parallel() runs commands side by side, one bar each (two boards flashed at once).

The bar only appears on a terminal; piped (CI, files), only the log lines are printed.
"""

from __future__ import annotations

import logging
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from collections.abc import Callable, Iterator
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager, nullcontext
from pathlib import Path
from typing import Literal

from rich.console import Console
from rich.highlighter import NullHighlighter
from rich.logging import RichHandler
from rich.markup import escape
from rich.progress import BarColumn, Progress, ProgressColumn, SpinnerColumn
from rich.text import Text
from rich.theme import Theme

from devthemes import ACTIVE_THEME, THEMES

ROOT = Path(__file__).resolve().parent.parent
LOG_FILE = Path(__file__).resolve().parent / "logs" / "dev.log"

TREE_BRANCH = " ├── "
TREE_LAST = " └── "

RAW = False

ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")

DONE = 25
logging.addLevelName(DONE, "DONE")

Outcome = Literal["done", "warning", "error"]
LEVEL_OF_OUTCOME: dict[Outcome, int] = {
    "done": DONE,
    "warning": logging.WARNING,
    "error": logging.ERROR,
}


def build_theme(theme_name: str) -> Theme:
    if theme_name not in THEMES:
        sys.exit(f"devthemes.py: no theme '{theme_name}' (choose from {', '.join(THEMES)}).")
    colors = THEMES[theme_name]
    return Theme(
        {
            "accent": colors["accent"],
            "success": colors["success"],
            "warning": colors["warning"],
            "error": colors["error"],
            "muted": colors["muted"],
            "command": f"italic {colors['muted']}",
            "path": f"italic {colors['highlight']}",
            "package": f"italic {colors['highlight']}",
            "gain": colors["gain"],
            "loss": colors["loss"],
            "track": colors["track"],
            "table_header": f"bold {colors['accent']}",
            "logging.level.debug": colors["muted"],
            "logging.level.info": colors["accent"],
            "logging.level.done": colors["success"],
            "logging.level.warning": colors["warning"],
            "logging.level.error": colors["error"],
            "log.time": colors["muted"],
        }
    )


THEME = build_theme(ACTIVE_THEME)
console = Console(theme=THEME, highlight=False)


class LogFileHandler(logging.Handler):
    """Every record, with a timestamp and without colors, in scripts/logs/dev.log."""

    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.file_console = Console(
            file=LOG_FILE.open("a", encoding="utf-8"),
            theme=THEME,
            no_color=True,
            width=120,
            highlight=False,
        )

    def emit(self, record: logging.LogRecord) -> None:
        try:
            timestamp = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(record.created))
            level = record.levelname.ljust(7)
            self.file_console.print(f"{timestamp} {level} {record.getMessage()}", markup=True)
        except Exception:
            self.handleError(record)


def make_logger() -> logging.Logger:
    LOG_FILE.parent.mkdir(exist_ok=True)
    logger = logging.getLogger("dev")
    if logger.handlers:
        return logger
    logger.setLevel(logging.DEBUG)
    logger.addHandler(
        RichHandler(
            level=logging.INFO,
            console=console,
            show_path=False,
            markup=True,
            highlighter=NullHighlighter(),
            rich_tracebacks=True,
            log_time_format="[%H:%M:%S]",
            omit_repeated_times=False,
        )
    )
    logger.addHandler(LogFileHandler())
    return logger


log = make_logger()


def done(message: str) -> None:
    log.log(DONE, message)


def shown(command: list[str]) -> str:
    """The command as a person would type it: tools of `.venv` by their bare name, paths of this
    repository relative to it."""
    executable = Path(command[0])
    if executable.is_absolute() and (ROOT / ".venv") in executable.parents:
        executable = Path(executable.name)
    return " ".join([str(executable), *command[1:]]).replace(f"{ROOT}/", "")


def echo(
    command: list[str], extra_environment: dict[str, str] | None = None, suffix: str = ""
) -> None:
    environment_prefix = "".join(
        f"{name}={value} " for name, value in (extra_environment or {}).items()
    )
    log.info(f"[command]$ {escape(environment_prefix + shown(command))}[/]{suffix}")


def fmt_secs(seconds: float) -> str:
    """`6.2 s`, `1 min 05 s`."""
    if seconds < 60:
        return f"{seconds:.1f} s"
    return f"{int(seconds // 60)} min {int(seconds % 60):02d} s"


class SpacedLabelColumn(ProgressColumn):
    """What the bar is for (`flash node A`), padded to a common width so parallel bars line up."""

    def __init__(self, width: int) -> None:
        super().__init__()
        self.width = width

    def render(self, task) -> Text:
        label = Text.from_markup(task.description)
        label.truncate(self.width, overflow="ellipsis", pad=True)
        return label


class ElapsedSecondsColumn(ProgressColumn):
    def render(self, task) -> Text:
        return Text(fmt_secs(task.elapsed or 0).rjust(9), style="accent")


class LatestOutputColumn(ProgressColumn):
    """The last line the command printed, as `Text`: compiler output is full of square brackets."""

    def render(self, task) -> Text:
        return Text(
            task.fields.get("latest_output", ""), style="muted", no_wrap=True, overflow="ellipsis"
        )


FIXED_COLUMNS_WIDTH = 30


def make_progress(label_width: int) -> Progress:
    return Progress(
        SpinnerColumn("dots", style="accent"),
        SpacedLabelColumn(label_width),
        BarColumn(bar_width=14, style="track", pulse_style="accent"),
        ElapsedSecondsColumn(),
        LatestOutputColumn(),
        console=console,
        transient=True,
    )


def fit_to_terminal(text: str, label_width: int) -> str:
    """One line of a command's output without colors, cut with … to what is left of the terminal's
    width after the other columns."""
    text = ANSI_ESCAPE.sub("", text).strip()
    room = max(console.width - FIXED_COLUMNS_WIDTH - label_width, 12)
    return text if len(text) <= room else text[: room - 1] + "…"


class LiveRow:
    """One command's bar at the bottom of the terminal. While it runs: `show_latest_output(line)`.
    When it ends: `finish(...)` removes the bar and logs the outcome in its place (the log line is
    all there is when output is piped and there is no bar)."""

    def __init__(
        self, label: str, progress: Progress | None = None, label_width: int | None = None
    ) -> None:
        self.label = label
        self.progress = progress
        self.label_width = label_width or Text.from_markup(label).cell_len
        self.started_at = time.monotonic()
        self.task_id = (
            progress.add_task(label, total=None, latest_output="") if progress is not None else None
        )

    def show_latest_output(self, line: str) -> None:
        if self.progress is not None:
            self.progress.update(
                self.task_id, latest_output=fit_to_terminal(line, self.label_width)
            )

    def remove_bar(self) -> None:
        if self.progress is not None:
            self.progress.remove_task(self.task_id)

    def finish(
        self, outcome: Outcome, details: list[str] | None = None, command_line: str | None = None
    ) -> None:
        elapsed = fmt_secs(time.monotonic() - self.started_at)
        message = f"{self.label}  [accent]{elapsed}[/]"
        if details:
            message += f"  [muted]{escape(' · '.join(details))}[/]"
        if command_line:
            message += f"\n[command]$ {escape(command_line)}[/]"
        log.log(LEVEL_OF_OUTCOME[outcome], message)
        self.remove_bar()


@contextmanager
def live_row(label: str) -> Iterator[LiveRow]:
    if not console.is_terminal:
        yield LiveRow(label)
        return
    with make_progress(Text.from_markup(label).cell_len) as progress:
        yield LiveRow(label, progress)


def command_environment(extra_environment: dict[str, str] | None) -> dict[str, str]:
    """The environment of a captured command: colors on a terminal (so a build error shown later
    keeps cargo's colors), no progress bar of its own."""
    colors = {"CARGO_TERM_COLOR": "always", "CLICOLOR_FORCE": "1"} if console.is_terminal else {}
    return {
        **os.environ,
        **colors,
        "CARGO_TERM_PROGRESS_WHEN": "never",
        **(extra_environment or {}),
    }


def print_output(output: str, limit: int = 150) -> None:
    """A command's captured output, in its own colors; the end of it when it is long."""
    lines = output.rstrip().splitlines()
    if len(lines) > limit:
        console.print(
            f"[muted]… {len(lines) - limit} earlier lines, all of it in {LOG_FILE.relative_to(ROOT)}[/]"
        )
        lines = lines[-limit:]
    console.print(Text.from_ansi("\n".join(lines)), soft_wrap=True)


def start_captured(command: list[str], working_directory: Path | None, environment: dict[str, str]):
    return subprocess.Popen(
        command,
        cwd=working_directory or ROOT,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        bufsize=1,
    )


def count_warnings(plain_output: str) -> int:
    return sum(
        1
        for line in plain_output.splitlines()
        if line.startswith("warning") and "generated" not in line
    )


def task(
    label: str,
    command: list[str],
    *,
    working_directory: Path | None = None,
    extra_environment: dict[str, str] | None = None,
    show_output: Literal["failure", "warnings", "always"] = "warnings",
    summarize: Callable[[str], str] | None = None,
    exit_on_failure: bool = True,
) -> int:
    """Runs `command` with a bar at the bottom of the terminal, then logs what it did:

        14:03:21 DONE  build node-a  6.2 s  1 crate compiled
                       $ cargo build --release --bin node-a

    Its output is captured (and goes to the log file). It is printed when the command fails, and
    with `show_output="warnings"` also when it printed warnings; "always" prints it every time (test
    results). `summarize(output)` adds a detail to the log line, e.g. the number of tests. With
    `exit_on_failure`, a failure stops the script with the command's exit code. Returns the exit
    code."""
    if RAW:
        log.info(f"[bold]{label}[/]")
        return live(
            command,
            exit_on_failure=exit_on_failure,
            extra_environment=extra_environment,
            working_directory=working_directory,
        )

    command_line = shown(command)
    log.debug(f"[bold]{label}[/]: [command]$ {escape(command_line)}[/]")
    captured_lines: list[str] = []
    crates_compiled = 0
    process = start_captured(command, working_directory, command_environment(extra_environment))
    assert process.stdout is not None
    with live_row(label) as row:
        try:
            for line in process.stdout:
                captured_lines.append(line)
                plain_line = ANSI_ESCAPE.sub("", line).strip()
                if crate := re.match(r"Compiling (\S+ v\S+)", plain_line):
                    crates_compiled += 1
                    row.show_latest_output(f"compiling {crate[1]}  ({crates_compiled})")
                elif plain_line.startswith("Compiling "):
                    crates_compiled += 1
                    row.show_latest_output(f"{plain_line[10:]}  ({crates_compiled})")
                elif plain_line:
                    row.show_latest_output(plain_line)
            exit_code = process.wait()
        except KeyboardInterrupt:
            process.terminate()
            process.wait()
            row.finish("error", ["interrupted"], command_line)
            log.info("Ctrl+C: stopped")
            raise SystemExit(130)

        output = "".join(captured_lines)
        plain_output = ANSI_ESCAPE.sub("", output).strip()
        log.debug(f"exit {exit_code}: {escape(command_line)}\n[muted]{escape(plain_output)}[/]")
        warnings = count_warnings(plain_output)
        if exit_code != 0:
            row.finish("error", [f"exit {exit_code}"], command_line)
        else:
            details = []
            if crates_compiled:
                details.append(
                    f"{crates_compiled} crate{'s' if crates_compiled > 1 else ''} compiled"
                )
            if summarize and (summary := summarize(output)):
                details.append(summary)
            if warnings:
                details.append(f"{warnings} warning{'s' if warnings > 1 else ''}")
            row.finish("warning" if warnings else "done", details, command_line)

    if exit_code != 0:
        print_output(output)
        if exit_on_failure:
            raise SystemExit(exit_code)
    elif show_output == "always" or (show_output == "warnings" and warnings):
        print_output(output)
    return exit_code


def run(
    command: list[str],
    *,
    exit_on_failure: bool = True,
    extra_environment: dict[str, str] | None = None,
    quiet: bool = False,
) -> subprocess.CompletedProcess[str]:
    """Runs `command` with a bar, keeps its output (stdout and stderr apart: the caller reads
    them, e.g. pip's JSON) and writes it to the log file.

    With `exit_on_failure`, a failure prints the end of its output and stops the script with the
    command's exit code. `quiet` is for checks that only get in the way (a version number): a
    spinner while it runs, nothing logged on screen (the command still goes to the log file)."""
    command_line = shown(command)
    log.debug(f"[command]$ {escape(command_line)}[/]")
    environment = {**os.environ, **extra_environment} if extra_environment else None
    with live_row(escape(command_line)) as row:
        result = subprocess.run(
            command,
            cwd=ROOT,
            env=environment,
            text=True,
            capture_output=True,
            stdin=subprocess.DEVNULL,
        )
        output = (result.stdout + result.stderr).strip()
        if output:
            log.debug(f"[muted]{escape(output)}[/]")
        if quiet:
            row.remove_bar()
        elif result.returncode == 0:
            row.finish("done")
        else:
            row.finish("error", [f"exit {result.returncode}"])
    if exit_on_failure and result.returncode != 0:
        print_output("\n".join(output.splitlines()[-15:]))
        raise SystemExit(result.returncode)
    return result


def live(
    command: list[str],
    *,
    exit_on_failure: bool = True,
    extra_environment: dict[str, str] | None = None,
    working_directory: Path | None = None,
) -> int:
    """Runs `command` in the terminal as if typed there: its output (a serial monitor, the RTT
    console, cargo's own with `-v`) isn't captured, and it reads the keyboard. Ctrl+C goes to the
    command only, which decides how to stop."""
    echo(command, extra_environment)
    environment = {**os.environ, **extra_environment} if extra_environment else None
    previous_handler = signal.signal(signal.SIGINT, signal.SIG_IGN)
    try:
        exit_code = subprocess.call(
            command,
            cwd=working_directory or ROOT,
            env=environment,
            preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL),
        )
    finally:
        signal.signal(signal.SIGINT, previous_handler)
    log.debug(f"exit {exit_code}: {shown(command)}")
    if exit_on_failure and exit_code != 0:
        log.error(
            f"command failed [muted](exit {exit_code})[/]: [command]{escape(shown(command))}[/]"
        )
        raise SystemExit(exit_code)
    return exit_code


def run_parallel(
    commands: dict[str, list[str]], labels: dict[str, str]
) -> dict[str, tuple[int, str, float]]:
    """Runs the commands side by side, one per key, each with its own bar (`labels[key]`) and its
    own log line when it ends, with their output captured. Returns {key: (exit code, output,
    seconds)}. Meant for work that waits on hardware (flashing two boards): each command has its
    own device, so nothing is shared."""
    for key, command in commands.items():
        log.debug(f"{labels[key]}: [command]$ {escape(shown(command))}[/]")
    label_width = max(Text.from_markup(label).cell_len for label in labels.values())
    progress = make_progress(label_width) if console.is_terminal else None
    rows = {key: LiveRow(labels[key], progress, label_width) for key in commands}

    def run_one(item: tuple[str, list[str]]) -> tuple[str, tuple[int, str, float]]:
        key, command = item
        started_at = time.monotonic()
        process = start_captured(command, None, command_environment(None))
        captured_lines = []
        assert process.stdout is not None
        for line in process.stdout:
            captured_lines.append(line)
            if ANSI_ESCAPE.sub("", line).strip():
                rows[key].show_latest_output(line)
        exit_code = process.wait()
        command_line = shown(command)
        if exit_code == 0:
            rows[key].finish("done", command_line=command_line)
        else:
            rows[key].finish("error", [f"exit {exit_code}"], command_line)
        output = "".join(captured_lines)
        log.debug(
            f"exit {exit_code}: {command_line}\n[muted]{escape(ANSI_ESCAPE.sub('', output).strip())}[/]"
        )
        return key, (exit_code, output, time.monotonic() - started_at)

    try:
        with ThreadPoolExecutor(len(commands)) as pool, progress or nullcontext():
            return dict(pool.map(run_one, commands.items()))
    except KeyboardInterrupt:
        log.info("Ctrl+C: stopped")
        raise SystemExit(130)


def notify(title: str, message: str) -> None:
    """A desktop notification, for a command you walked away from. Never fails the command;
    silent when not run from a terminal (CI, pipes) or with DEV_NOTIFY=0."""
    if os.environ.get("DEV_NOTIFY") == "0" or not sys.stdout.isatty():
        return
    if sys.platform == "darwin":
        notification_command = ["osascript", "-e", "on run argv", "-e"]
        notification_command += [
            "display notification (item 1 of argv) with title (item 2 of argv)"
        ]
        notification_command += ["-e", "end run", message, title]
    elif shutil.which("notify-send"):
        notification_command = ["notify-send", title, message]
    else:
        sys.stdout.write("\a")
        sys.stdout.flush()
        return
    try:
        subprocess.run(notification_command, capture_output=True, timeout=5, check=False)
    except (OSError, subprocess.SubprocessError):
        pass


def stream(command: list[str]) -> int:
    """Runs a long command (dev server): its output scrolls (and goes to the log file) until
    Ctrl+C stops it cleanly. Returns its exit code."""
    echo(command, suffix="  [muted](Ctrl+C to stop)[/]")
    process = subprocess.Popen(
        command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1
    )
    try:
        assert process.stdout is not None
        for line in process.stdout:
            console.print(Text(line.rstrip(), style="muted"))
            log.debug(escape(line.rstrip()))
    except KeyboardInterrupt:
        log.info("Ctrl+C: stopping …")
        process.terminate()
    try:
        return process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        return process.wait()
