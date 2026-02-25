"""Status line rendered below the prompt.

Inspired by OpenCode's footer bar with dot indicators.
"""

import subprocess
from rich.console import Console

from app.cli.tui.theme import (
    PRIMARY, WARNING, ACCENT_MAGENTA, MUTED, DIM,
    STATUS_DOT_ON, SEPARATOR,
)


def git_short_status() -> str:
    """Return a short git status string, or empty if not a git repo."""
    try:
        r = subprocess.run(
            ["git", "status", "--porcelain", "--short"],
            capture_output=True, text=True, timeout=2,
        )
        if r.returncode != 0:
            return ""
        lines = [ln for ln in r.stdout.strip().split("\n") if ln]
        if not lines:
            return ""
        return f"\u0394{len(lines)}"
    except Exception:
        return ""


def render_status_line(console: Console, agent, auto_approve: bool = False):
    """Print a compact status line: left = modes, right = context + git."""
    left_parts = []
    if auto_approve:
        left_parts.append(f"[{WARNING}]{STATUS_DOT_ON} auto-approve[/{WARNING}]")
    plan_mode = getattr(agent, "plan_mode", False)
    if plan_mode:
        left_parts.append(f"[{ACCENT_MAGENTA}]{STATUS_DOT_ON} plan[/{ACCENT_MAGENTA}]")
    left = SEPARATOR.join(left_parts) if left_parts else f"[{MUTED}]ready[/{MUTED}]"

    right_parts = []
    n = len(agent.history)
    if n:
        right_parts.append(f"{n} msgs")
    gs = git_short_status()
    if gs:
        right_parts.append(gs)
    right = f"[{MUTED}]" + SEPARATOR.join(right_parts) + f"[/{MUTED}]" if right_parts else ""

    console.print(f"  {left}{'':>4}{right}")
