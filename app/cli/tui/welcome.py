"""Welcome banner with crystal mascot and status checks.

Design inspired by OpenCode's centered home screen with tips and footer.
"""

import os
from rich.console import Console
from rich.text import Text

from app.cli.tui.theme import (
    PRIMARY, SECONDARY, ACCENT, ACCENT_MAGENTA, ACCENT_CYAN, MUTED, DIM,
    SUCCESS, WARNING, ERROR, STATUS_DOT_ON, STATUS_DOT_OFF, SEPARATOR,
    CRYSTAL_OUTER_DIM, CRYSTAL_OUTER, CRYSTAL_INNER, CRYSTAL_CORE,
    HEADER_COMMANDS_L, HEADER_COMMANDS_R, RAINBOW,
)


# Keep for backwards compat — other modules may import these
def detect_capabilities() -> dict:
    """Legacy capability detection. Use app.backend.status instead."""
    from app.backend.status import detect_llm, detect_commands
    llm = detect_llm()
    cmds = detect_commands()
    return {
        "LLM": llm["connected"],
        "search": any(t["healthy"] for t in cmds["tools"] if t["name"] == "search_materials"),
    }


def _detect_provider() -> str | None:
    """Detect the active LLM provider from environment."""
    from app.backend.status import detect_llm
    result = detect_llm()
    return result["provider"]


# Short display names for tools
_SHORT_NAMES = {
    "search_materials": "search",
    "query_materials_project": "MP",
    "literature_search": "literature",
    "predict_property": "predict",
    "execute_python": "python",
    "web_search": "web",
}


def show_welcome(console: Console, agent, auto_approve: bool = False):
    """Render the crystal mascot banner with status checks."""
    from app import __version__
    from app.backend.status import build_status

    status = build_status(tool_registry=agent.tools)
    llm = status["llm"]
    plugins = status["plugins"]
    commands = status["commands"]
    skills = status["skills"]

    console.print()

    # Line 0: top outer rim
    t = Text("    ")
    t.append("\u2b21", style=CRYSTAL_OUTER_DIM)
    t.append(" ")
    t.append("\u2b21", style=CRYSTAL_OUTER_DIM)
    t.append(" ")
    t.append("\u2b21", style=CRYSTAL_OUTER_DIM)
    console.print(t)

    # Line 1: middle row + rainbow rays + left commands
    t = Text("  ")
    t.append("\u2b21", style=CRYSTAL_OUTER)
    t.append(" ")
    t.append("\u2b22", style=f"bold {CRYSTAL_INNER}")
    t.append(" ")
    t.append("\u2b22", style=f"bold {CRYSTAL_CORE}")
    t.append(" ")
    t.append("\u2b22", style=f"bold {CRYSTAL_INNER}")
    t.append(" ")
    t.append("\u2b21", style=CRYSTAL_OUTER)
    t.append("  ")
    for j in range(15):
        t.append("\u2501", style=f"bold {RAINBOW[j]}")
    t.append("  ")
    for cmd in HEADER_COMMANDS_L:
        t.append(cmd, style=MUTED)
        t.append(" ")
    console.print(t)

    # Line 2: middle row + rainbow rays + right commands
    t = Text("  ")
    t.append("\u2b21", style=CRYSTAL_OUTER)
    t.append(" ")
    t.append("\u2b22", style=f"bold {CRYSTAL_INNER}")
    t.append(" ")
    t.append("\u2b22", style=f"bold {CRYSTAL_CORE}")
    t.append(" ")
    t.append("\u2b22", style=f"bold {CRYSTAL_INNER}")
    t.append(" ")
    t.append("\u2b21", style=CRYSTAL_OUTER)
    t.append("  ")
    for j in range(15):
        t.append("\u2501", style=f"bold {RAINBOW[j]}")
    t.append("  ")
    for cmd in HEADER_COMMANDS_R:
        t.append(cmd, style=MUTED)
        t.append(" ")
    console.print(t)

    # Line 3: bottom rim
    t = Text("    ")
    t.append("\u2b21", style=CRYSTAL_OUTER_DIM)
    t.append(" ")
    t.append("\u2b21", style=CRYSTAL_OUTER_DIM)
    t.append(" ")
    t.append("\u2b21", style=CRYSTAL_OUTER_DIM)
    console.print(t)

    console.print()

    # Title + version
    title = Text("  ")
    title.append("PRISM", style=f"bold {PRIMARY}")
    title.append(f" v{__version__}", style=MUTED)
    console.print(title)

    # Subtitle
    sub = Text("  ")
    sub.append("AI-Native Autonomous Materials Discovery", style=DIM)
    console.print(sub)

    console.print()

    # 1. LLM status
    line = Text("  ")
    if llm["connected"]:
        line.append(STATUS_DOT_ON, style=SUCCESS)
        line.append(" ")
        line.append(llm["provider"], style=f"bold {SECONDARY}")
        line.append(" connected", style=DIM)
    else:
        line.append(STATUS_DOT_OFF, style=MUTED)
        line.append(" ")
        line.append("No LLM configured", style=ERROR)
        line.append(" \u2014 run ", style=DIM)
        line.append("prism setup", style=ACCENT_CYAN)
        line.append(" or visit ", style=DIM)
        line.append("platform.marc27.com", style=SECONDARY)
    console.print(line)

    # 2. Plugins
    line = Text("  ")
    line.append(STATUS_DOT_OFF, style=MUTED)
    line.append(" ")
    line.append("Plugins", style=DIM)
    line.append(" coming soon", style=MUTED)
    console.print(line)

    # 3. Commands
    line = Text("  ")
    any_healthy = any(t["healthy"] for t in commands["tools"])
    line.append(STATUS_DOT_ON if any_healthy else STATUS_DOT_OFF,
                style=SUCCESS if any_healthy else MUTED)
    line.append(" ")
    line.append("Commands ", style=DIM)
    for i, tool in enumerate(commands["tools"]):
        name = _SHORT_NAMES.get(tool["name"], tool["name"])
        if tool["registered"]:
            line.append(name, style=SUCCESS if tool["healthy"] else WARNING)
        else:
            line.append(name, style=MUTED)
        if i < len(commands["tools"]) - 1:
            line.append(" \u00b7 ", style=DIM)
    if commands["healthy_providers"] > 0:
        line.append(f" ({commands['healthy_providers']}/{commands['total_providers']} providers)", style=DIM)
    console.print(line)

    # 4. Skills
    line = Text("  ")
    line.append(STATUS_DOT_ON if skills["count"] > 0 else STATUS_DOT_OFF,
                style=SUCCESS if skills["count"] > 0 else MUTED)
    line.append(" ")
    line.append(f"{skills['count']} skills", style=DIM)
    console.print(line)

    # Auto-approve warning
    if auto_approve:
        line = Text("  ")
        line.append("\u26a0 auto-approve enabled", style=WARNING)
        console.print(line)

    console.print()
