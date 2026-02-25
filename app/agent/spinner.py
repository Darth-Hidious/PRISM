"""Backward-compatibility shim — real implementation in app.cli.tui.spinner."""

from app.cli.tui.spinner import Spinner, TOOL_VERBS  # noqa: F401

# Legacy constant — no longer used by Spinner (Rich Status handles animation)
# but kept for backward compatibility with tests.
BRAILLE_FRAMES = [
    "\u280b", "\u2819", "\u2839", "\u2838", "\u283c",
    "\u2834", "\u2826", "\u2827", "\u2807", "\u280f",
]
