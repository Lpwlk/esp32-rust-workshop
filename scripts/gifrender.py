"""Turns an asciicast (.cast) into a GIF, with a look read from docs/book/assets/demo/render.toml.

Shared by `record.py` (which records, then renders) and `render_demos.py` (which only renders:
no board needed, since a cast is the raw text of the session). The style keys, their meaning and
their defaults are documented in render.toml.

    from gifrender import add_style_arguments, overrides_from, load_style, render
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RENDER_TOML = ROOT / "docs" / "book" / "assets" / "demo" / "render.toml"

# What render.toml says when it says nothing: the look the first recordings had.
DEFAULTS: dict[str, object] = {
    "font_size": 15,
    "line_height": 1.3,
    "theme": "monokai",
    "font_family": "",
    "speed": 1.0,
    "idle_time_limit": 1.2,
    "last_frame_duration": 3.0,
    "fps_cap": 30,
    "cols": 0,
    "rows": 0,
    "colors": 256,
}

THEMES = (
    "asciinema dracula github-dark github-light kanagawa kanagawa-dragon kanagawa-light "
    "monokai nord solarized-dark solarized-light gruvbox-dark"
).split()


class StyleError(ValueError):
    """A wrong key or value in render.toml or on the command line."""


def add_style_arguments(parser: argparse.ArgumentParser) -> None:
    """The `--font-size`... options, which override render.toml for this run."""
    g = parser.add_argument_group("GIF look (overrides docs/book/assets/demo/render.toml)")
    g.add_argument("--font-size", type=int, metavar="PX", help="text size in whole pixels")
    g.add_argument(
        "--line-height", type=float, metavar="X", help="line height, times the font size"
    )
    g.add_argument("--theme", choices=THEMES, help="color theme")
    g.add_argument("--font-family", metavar="NAME[,NAME]", help="monospace font(s), tried in order")
    g.add_argument("--speed", type=float, metavar="X", help="playback speed (2 = twice as fast)")
    g.add_argument("--idle-limit", type=float, metavar="S", help="longest pause kept, seconds")
    g.add_argument("--last-frame", type=float, metavar="S", help="how long the last frame stays")
    g.add_argument("--fps-cap", type=int, metavar="N", help="frames per second at most")
    g.add_argument("--window-cols", type=int, metavar="N", help="terminal width in characters")
    g.add_argument("--window-rows", type=int, metavar="N", help="terminal height in characters")
    g.add_argument("--colors", type=int, metavar="N", help="palette size, 2-256")


def overrides_from(args: argparse.Namespace) -> dict[str, object]:
    """The style keys given on the command line (and only those)."""
    names = {
        "font_size": "font_size",
        "line_height": "line_height",
        "theme": "theme",
        "font_family": "font_family",
        "speed": "speed",
        "idle_limit": "idle_time_limit",
        "last_frame": "last_frame_duration",
        "fps_cap": "fps_cap",
        "window_cols": "cols",
        "window_rows": "rows",
        "colors": "colors",
    }
    found = {key: getattr(args, attr, None) for attr, key in names.items()}
    return {k: v for k, v in found.items() if v is not None}


def style_flags(overrides: dict[str, object]) -> list[str]:
    """The same overrides as command-line flags, to hand them to another script."""
    flags = {
        "font_size": "--font-size",
        "line_height": "--line-height",
        "theme": "--theme",
        "font_family": "--font-family",
        "speed": "--speed",
        "idle_time_limit": "--idle-limit",
        "last_frame_duration": "--last-frame",
        "fps_cap": "--fps-cap",
        "cols": "--window-cols",
        "rows": "--window-rows",
        "colors": "--colors",
    }
    return [part for key, value in overrides.items() for part in (flags[key], str(value))]


def _check(style: dict[str, object], where: str) -> None:
    unknown = sorted(set(style) - set(DEFAULTS))
    if unknown:
        raise StyleError(f"{where}: unknown key {unknown} (known: {', '.join(DEFAULTS)})")
    for key, value in style.items():
        if isinstance(DEFAULTS[key], str) != isinstance(value, str) or isinstance(value, bool):
            raise StyleError(f"{where}: `{key}` has the wrong type ({value!r})")
    if style["theme"] not in THEMES:
        raise StyleError(
            f"{where}: unknown theme {style['theme']!r} (choose from {', '.join(THEMES)})"
        )
    if style["font_size"] != int(style["font_size"]):
        raise StyleError(f"{where}: `font_size` must be a whole number of pixels")
    for key, low in (("font_size", 4), ("line_height", 0.8), ("speed", 0.05), ("fps_cap", 1)):
        if style[key] < low:
            raise StyleError(f"{where}: `{key}` must be at least {low}")
    if not 2 <= style["colors"] <= 256:
        raise StyleError(f"{where}: `colors` must be 2-256")
    for key in ("idle_time_limit", "last_frame_duration", "cols", "rows"):
        if style[key] < 0:
            raise StyleError(f"{where}: `{key}` can't be negative")


def load_style(cast: Path, overrides: dict[str, object] | None = None) -> dict[str, object]:
    """The look of `cast`: the defaults, then [style], then the table named like the cast,
    then what was asked on the command line."""
    config = tomllib.loads(RENDER_TOML.read_text()) if RENDER_TOML.exists() else {}
    style = {**DEFAULTS, **config.get("style", {})}
    _check(style, f"{RENDER_TOML.name} [style]")
    own = config.get(cast.stem, {})
    if own:
        style = {**style, **own}
        _check(style, f"{RENDER_TOML.name} [{cast.stem}]")
    if overrides:
        style = {**style, **overrides}
        _check(style, "command line")
    return style


def render(cast: Path, style: dict[str, object]) -> Path:
    """Renders `cast` to `<cast>.gif` with agg, then shrinks it with gifsicle if that is installed."""
    if not shutil.which("agg"):
        raise SystemExit(
            "agg not found: brew install agg (or cargo install --git https://github.com/asciinema/agg)"
        )
    gif = cast.with_suffix(".gif")
    cmd = [
        "agg",
        "--quiet",
        "--theme", str(style["theme"]),
        "--font-size", str(int(style["font_size"])),
        "--line-height", str(style["line_height"]),
        "--idle-time-limit", str(style["idle_time_limit"]),
        "--speed", str(style["speed"]),
        "--last-frame-duration", str(style["last_frame_duration"]),
        "--fps-cap", str(style["fps_cap"]),
    ]  # fmt: skip
    if style["font_family"]:
        cmd += ["--font-family", str(style["font_family"])]
    if style["cols"]:
        cmd += ["--cols", str(style["cols"])]
    if style["rows"]:
        cmd += ["--rows", str(style["rows"])]
    subprocess.run([*cmd, str(cast), str(gif)], check=True)
    if shutil.which("gifsicle"):
        # Lossless at 256 colors; fewer colors is smaller but can band the gradients of the RGB swatches.
        subprocess.run(
            ["gifsicle", "-O3", "--colors", str(style["colors"]), "-b", str(gif)], check=True
        )
    return gif
