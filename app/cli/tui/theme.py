"""Design tokens for the PRISM TUI.

Single source of truth for every colour, border style, and visual
constant used by the Rich-based terminal interface.

Palette inspired by OpenCode's warm-tone dark theme.
"""

# ── Base palette ─────────────────────────────────────────────────────

PRIMARY = "#fab283"        # Warm orange (primary actions, branding)
SECONDARY = "#5c9cf5"      # Blue (links, secondary info)
ACCENT_MAGENTA = "#bb86fc"  # Purple (plans, emphasis)
ACCENT_CYAN = "#56b6c2"    # Teal (user input, interactive)

# ── Semantic colours ─────────────────────────────────────────────────

SUCCESS = "#7fd88f"
WARNING = "#e5c07b"
ERROR = "#e06c75"
INFO = "#61afef"
ACCENT = f"bold {PRIMARY}"
DIM = "dim"
TEXT = "#e0e0e0"
MUTED = "#808080"

# ── Card border colours by result type ───────────────────────────────

BORDERS = {
    "input": ACCENT_CYAN,
    "output": MUTED,
    "tool": SUCCESS,
    "error": ERROR,
    "error_partial": WARNING,
    "metrics": INFO,
    "calphad": SECONDARY,
    "labs": ACCENT_MAGENTA,
    "validation_critical": ERROR,
    "validation_warning": WARNING,
    "validation_info": INFO,
    "results": MUTED,
    "plot": SUCCESS,
    "approval": WARNING,
    "plan": ACCENT_MAGENTA,
}

# ── Crystal mascot — 3-tier glow ────────────────────────────────────

CRYSTAL_OUTER_DIM = "#555577"
CRYSTAL_OUTER = "#7777aa"
CRYSTAL_INNER = "#ccccff"
CRYSTAL_CORE = "#ffffff"

MASCOT = [
    "    \u2b21 \u2b21 \u2b21",
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "    \u2b21 \u2b21 \u2b21",
]

# ── Welcome header commands (shown next to rainbow rays) ─────────────

HEADER_COMMANDS_L = ["/help", "/tools", "/skills"]
HEADER_COMMANDS_R = ["/scratchpad", "/status", "/save"]

# VIBGYOR rainbow (10 stops, extended to 15 for ray length)
RAINBOW = [
    "#ff0000", "#ff5500", "#ff8800", "#ffcc00", "#88ff00",
    "#00cc44", "#00cccc", "#0088ff", "#5500ff", "#8b00ff",
    "#ff0000", "#ff5500", "#ff8800", "#ffcc00", "#88ff00",
]

# ── Card icons ───────────────────────────────────────────────────────

ICONS = {
    "input": "\u276f",       # ❯
    "output": "\u25cb",      # ○
    "tool": "\u2699",        # ⚙
    "error": "\u2717",       # ✗
    "success": "\u2714",     # ✔
    "metrics": "\u25a0",     # ■
    "calphad": "\u2206",     # ∆
    "labs": "\u2726",        # ✦
    "validation": "\u25cf",  # ●
    "results": "\u2261",     # ≡
    "plot": "\u25a3",        # ▣
    "approval": "\u26a0",    # ⚠
    "plan": "\u25b7",        # ▷
    "pending": "\u223c",     # ∼
}

# ── Output truncation ───────────────────────────────────────────────

TRUNCATION_LINES = 6
TRUNCATION_CHARS = 50_000

# ── Footer status indicators ────────────────────────────────────────

STATUS_DOT_ON = "\u25cf"   # ●
STATUS_DOT_OFF = "\u25cb"  # ○
SEPARATOR = " \u00b7 "     # ·
