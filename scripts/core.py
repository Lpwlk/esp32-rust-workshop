#!/usr/bin/env python3

# Builtins (Python 3.13+)
import subprocess
import sys, os, re, time
import logging

# Rich modules (https://github.com/Textualize/rich)
from rich.theme import Theme
from rich.console import Console
from rich.logging import RichHandler
from rich.highlighter import NullHighlighter, ReprHighlighter
from rich.live import Live
from rich.progress import (
    Progress,
    SpinnerColumn,
    TextColumn,
    BarColumn,
    TimeElapsedColumn,
)

vb = "│"
hb = "─"
mn = "├"
ln = "└"
sp = " "

branch_len = 5
mid_node_branch = sp + mn + hb * (branch_len - 3) + sp
last_node_branch = sp + ln + hb * (branch_len - 3) + sp
no_node_branch = sp + vb + sp * (1 + branch_len - 3)
empty_branch = sp * branch_len

rich_theme = Theme(
    inherit=True,
    styles={
        "path": "italic yellow",
        "cmd": "italic dim grey82",
        "pkg": "italic yellow",
        "ver": "bold green",
        "ver_alt": "bold red",
        "logging.level.info": "yellow",
        "logging.level.trace": "steel_blue3",
        "log.time": "steel_blue3",
    },
)

console = Console(
    theme=rich_theme,
    highlight=False,
    height=None,
    width=None,
)
console._overflow_height = True


def init_logger(
    name: str = "logger",
    log_path: str = os.path.join(os.path.dirname(__file__), "logs", "py-scripts.log"),
    level: int = logging.DEBUG,
) -> logging.Logger:
    """
    Rich-based logger init routine.

    This methods returns a logger instance containing two logging handlers (`FileHandler` & `RichHandler` classes from the builtin logging & rich.logging modules respectively).

    Args:
        name (str, optional): Logger instance name. Defaults to `logger`.
        log_path (str, optional): Target logging file for the `FileHandler`. Defaults to `app.log`.
        level (int, optional): Logging level from the `logging` module. Defaults to `logging.DEBUG`.

    Returns:
        logging.Logger: _description_
    """

    TRACE_LEVEL = 15
    logging.addLevelName(TRACE_LEVEL, "TRACE")

    def trace(self, message, *args, **kwargs):
        if self.isEnabledFor(TRACE_LEVEL):
            self._log(TRACE_LEVEL, message, args, **kwargs)

    logging.Logger.trace = trace

    os.makedirs(os.path.dirname(log_path), exist_ok=True)
    if not os.path.exists(log_path):
        open(log_path, "a").close()

    rich_handler = RichHandler(
        console=console,
        highlighter=NullHighlighter(),
        markup=True,
        rich_tracebacks=True,
        show_time=True,
        show_level=True,
        show_path=True,
        omit_repeated_times=True,
        log_time_format="%H:%M:%S",
    )

    rich_handler._log_render.level_width = 6

    file_handler = RichHandler(
        console=Console(
            file=open(log_path, "a", encoding="utf-8"),
            theme=rich_theme,
            highlight=False,
            no_color=True,
            force_terminal=False,
            width=100,
        ),
        highlighter=NullHighlighter(),
        markup=True,
        rich_tracebacks=True,
        show_time=True,
        show_level=True,
        show_path=True,
        omit_repeated_times=False,
        log_time_format="%d-%m-%Y - %H:%M:%S",
    )
    file_handler._log_render.level_width = 6

    logger = logging.getLogger(name)
    logger.setLevel(level)
    rich_handler.setLevel(level)
    file_handler.setLevel(level)

    logger.addHandler(rich_handler)
    logger.addHandler(file_handler)

    return logger


log = init_logger()


def run_process(
    command: list[str],
    verbose: bool = True,
    echo_output: bool = False,
    server_mode: bool = False,
    extra_msg: str | None = None,
    suffix: str | None = None,
    highlight: bool = False,
) -> subprocess.CompletedProcess:

    progress = Progress(
        SpinnerColumn(),
        TextColumn("{task.description}"),
        BarColumn(),
        TimeElapsedColumn(),
        transient=True,
    )
    if "python" in command[0]:
        task_description = f"Running command: [cmd]py {" ".join(command[1:])}[/]"
    else:
        task_description = f"Running command: [cmd]{" ".join(command)}[/]"

    progress.add_task(
        description=task_description + (extra_msg if extra_msg else ""),
        total=None,
    )
    
    if highlight: log.handlers[0].highlighter = ReprHighlighter()

    with Live(
        renderable=progress,
        refresh_per_second=20,
        console=console,
        transient=True,
    ):
        if server_mode:
            if verbose:
                log.info(
                    (suffix if suffix else "")
                    + task_description
                    + (extra_msg if extra_msg else "")
                )

            process = subprocess.Popen(
                command,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                bufsize=1,
                universal_newlines=True,
            )

            collected_output: list[str] = []
            while True:
                try:
                    line = process.stdout.readline()
                    if line:
                        collected_output.append(line)
                        log.trace(f"[dim]{line.rstrip()}[/]")
                    else:
                        if process.poll() is not None:
                            return
                        time.sleep(0.1)
                except KeyboardInterrupt:
                    log.info(
                        "Keyboard interrupt trigerred, killing subprocess & exiting script ..."
                    )
                    return
        else:
            result = subprocess.run(
                command,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            collected_output = result.stdout + result.stderr

    if verbose:
        log.info(
            (suffix if suffix else "")
            + task_description
            + (extra_msg if extra_msg else "")
        )
        time.sleep(0.2)

    if echo_output:
        if result.stdout:
            log.info(
                f"\\[[dim underline green]stdout[/]]\n[dim]{(result.stdout).strip()}[/]"
            )
        if result.stderr:
            log.info(
                f"\\[[dim underline red]stderr[/]]\n[dim]{(result.stderr).strip()}[/]"
            )
                
    return result


def parse_pip_installed_version(output: str) -> str:
    match = re.compile(r"^Version:\s+(.+)$", re.MULTILINE).search(output)
    if not match:
        raise ValueError(f"Unable to parse local version from pip show output:\n{output}")
    return match.group(1).strip()


def parse_pip_latest_version(output: str) -> str:
    match = re.compile(r"\(([^)]+)\)").search(output)
    if not match:
        raise ValueError(
            f"Unable to parse latest version from pip index output:\n{output}"
        )
    return match.group(1).strip()


def get_installed_version(package_name: str) -> str | None:
    show = run_process([sys.executable, "-m", "pip", "show", package_name])

    if "not found" in show.stderr:
        return None
    else:
        return parse_pip_installed_version(show.stdout)


def get_latest_version(package_name: str) -> str:
    index = run_process(
        [sys.executable, "-m", "pip", "index", "versions", package_name],
        suffix=empty_branch + mid_node_branch,
    )
    return parse_pip_latest_version(index.stdout)


def check_pkg_version(package_name: str):
    log.info(f"Checking pip installation for [pkg]{package_name}[/] ...")

    installed = get_installed_version(package_name)
    
    if installed:
        log.info(
            f"{last_node_branch}Found [pkg]{package_name}[/] version: [ver]{installed}[/]"
        )
        latest = get_latest_version(package_name)

        if installed == latest:
            log.info(
                f"{empty_branch + last_node_branch}Latest [pkg]{package_name}[/] version already installed ([ver]{installed}[/])"
            )
            return

        log.info(
            f"{last_node_branch}[yellow]Updating [pkg]{package_name}[/] from [ver_alt]{installed}[/] to [ver]{latest}[/]"
        )

        run_process(
            [sys.executable, "-m", "pip", "install", "--upgrade", package_name],
        )

        new_version = get_installed_version(package_name)

        log.info(f"Updated [pkg]{package_name}[/] to version [ver]{new_version}[/]")

    else:
        log.error(
            f"{mid_node_branch}[pkg]{package_name}[/] package not found: installing..."
        )
