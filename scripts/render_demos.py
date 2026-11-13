#!/usr/bin/env python3
"""Renders the console recordings (docs/book/assets/demo/*.cast) to GIFs again, with another look.

A cast is the raw text of a session, so the font size, the theme, the speed... can change without
recording again, and without a board. The look is in docs/book/assets/demo/render.toml; the options
below override it for one run.

    python3 scripts/render_demos.py                      every cast
    python3 scripts/render_demos.py boot eth-boot        some of them
    python3 scripts/render_demos.py --font-size 12       everything smaller, to try it
    python3 scripts/render_demos.py --list               the look each cast would get
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from gifrender import (
    ROOT,
    StyleError,
    add_style_arguments,
    load_style,
    overrides_from,
    render,
)  # noqa: E402

DEMOS = ROOT / "docs" / "book" / "assets" / "demo"


def main() -> None:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument(
        "casts", nargs="*", metavar="NAME", help="casts to render, without .cast (default: all)"
    )
    p.add_argument("--list", action="store_true", help="show the look of each cast, render nothing")
    add_style_arguments(p)
    args = p.parse_args()

    available = {c.stem: c for c in sorted(DEMOS.glob("*.cast"))}
    unknown = [n for n in args.casts if n not in available]
    if unknown:
        sys.exit(f"no such cast: {', '.join(unknown)} (have: {', '.join(available)})")
    chosen = [available[n] for n in args.casts] or list(available.values())
    if not chosen:
        sys.exit(f"no .cast in {DEMOS.relative_to(ROOT)}")

    for cast in chosen:
        try:
            style = load_style(cast, overrides_from(args))
        except StyleError as e:
            sys.exit(str(e))
        if args.list:
            print(
                f"{cast.stem}: "
                + ", ".join(f"{k}={v}" for k, v in style.items() if v not in ("", 0))
            )
            continue
        gif = render(cast, style)
        print(
            f"{gif.relative_to(ROOT)}: {gif.stat().st_size // 1024} KB (font {style['font_size']} px)"
        )


if __name__ == "__main__":
    main()
