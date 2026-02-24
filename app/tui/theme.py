"""Dark-only theme constants for the PRISM TUI."""
SURFACE = "#1a1a2e"
TEXT_DIM = "#888888"
ACCENT_MAGENTA = "#bb86fc"
ACCENT_CYAN = "#00cccc"
SUCCESS = "#00cc44"
WARNING = "#d29922"
ERROR = "#cc555a"
INFO = "#0088ff"
TEXT_PRIMARY = "#e0e0e0"
SURFACE_DARK = "#0f0f1a"
CARD_BG = "#16213e"
CRYSTAL_OUTER_DIM = "#555577"
CRYSTAL_OUTER = "#7777aa"
CRYSTAL_INNER = "#ccccff"
CRYSTAL_CORE = "#ffffff"
RAINBOW = [
    "#ff0000", "#ff5500", "#ff8800", "#ffcc00", "#88ff00",
    "#00cc44", "#00cccc", "#0088ff", "#5500ff", "#8b00ff",
]
CARD_BORDERS = {
    "input": ACCENT_CYAN, "output": TEXT_DIM, "tool": SUCCESS,
    "error": ERROR, "error_partial": WARNING, "approval": WARNING,
    "plan": ACCENT_MAGENTA, "metrics": INFO, "calphad": INFO,
    "validation_critical": ERROR, "validation_warning": WARNING,
    "validation_info": INFO, "results": TEXT_DIM, "plot": SUCCESS,
}
