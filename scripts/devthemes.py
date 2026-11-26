"""Color themes of `scripts/dev.py`'s output. To change the look, set ACTIVE_THEME to one of the
names in THEMES (or add a theme: copy one and change its colors). There is no option for it on the
command line.

Each color is a rich style: a name (`green`, `steel_blue3`), a hex color (`#88c0d0`) or both with
attributes (`bold #ff5555`, `italic grey62`).

    accent     the spinner, the pulsing bar, the elapsed time, INFO lines
    success    DONE lines, checkmarks
    warning    WARNING lines
    error      ERROR lines, failures
    muted      commands, details, the line a command is printing
    highlight  file paths and package names
    url        web addresses (the dashboards, the docs site): keep them easy to find
    gain       a new version, a smaller size
    loss       an old version, a bigger size
    track      the empty part of the bar
"""

ACTIVE_THEME = "ocean"

THEMES: dict[str, dict[str, str]] = {
    "ocean": {
        "accent": "steel_blue3",
        "success": "green",
        "warning": "yellow",
        "error": "bold red",
        "muted": "grey62",
        "highlight": "yellow",
        "url": "bold underline bright_cyan",
        "gain": "bold green",
        "loss": "bold red",
        "track": "grey23",
    },
    "forest": {
        "accent": "spring_green3",
        "success": "green",
        "warning": "dark_orange",
        "error": "bold red",
        "muted": "grey58",
        "highlight": "khaki1",
        "url": "bold underline bright_cyan",
        "gain": "bold green",
        "loss": "bold red",
        "track": "grey19",
    },
    "sunset": {
        "accent": "dark_orange3",
        "success": "chartreuse3",
        "warning": "gold1",
        "error": "bold red1",
        "muted": "grey62",
        "highlight": "light_salmon1",
        "url": "bold underline bright_yellow",
        "gain": "bold chartreuse3",
        "loss": "bold red1",
        "track": "grey23",
    },
    "dracula": {
        "accent": "#bd93f9",
        "success": "#50fa7b",
        "warning": "#f1fa8c",
        "error": "bold #ff5555",
        "muted": "#6272a4",
        "highlight": "#ffb86c",
        "url": "bold underline #8be9fd",
        "gain": "bold #50fa7b",
        "loss": "bold #ff5555",
        "track": "#44475a",
    },
    "nord": {
        "accent": "#88c0d0",
        "success": "#a3be8c",
        "warning": "#ebcb8b",
        "error": "bold #bf616a",
        "muted": "#7b88a1",
        "highlight": "#d08770",
        "url": "bold underline #8fbcbb",
        "gain": "bold #a3be8c",
        "loss": "bold #bf616a",
        "track": "#3b4252",
    },
    "mono": {
        "accent": "bold",
        "success": "bold",
        "warning": "underline",
        "error": "bold reverse",
        "muted": "dim",
        "highlight": "italic",
        "url": "bold underline",
        "gain": "bold",
        "loss": "reverse",
        "track": "dim",
    },
}
