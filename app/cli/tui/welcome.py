"""Welcome banner with crystal mascot and capability detection.

Design inspired by OpenCode's centered home screen with tips and footer.
"""

import os
from rich.console import Console
from rich.text import Text

from app.cli.tui.theme import (
    PRIMARY, SECONDARY, ACCENT, ACCENT_MAGENTA, MUTED, DIM,
    SUCCESS, WARNING, STATUS_DOT_ON, STATUS_DOT_OFF, SEPARATOR,
    CRYSTAL_OUTER_DIM, CRYSTAL_OUTER, CRYSTAL_INNER, CRYSTAL_CORE,
    HEADER_COMMANDS_L, HEADER_COMMANDS_R, RAINBOW,
)


def detect_capabilities() -> dict:
    caps = {}
    try:
        import sklearn  # noqa: F401
        caps["ML"] = True
    except ImportError:
        caps["ML"] = False
    try:
        from app.simulation.bridge import check_pyiron_available
        caps["pyiron"] = check_pyiron_available()
    except Exception:
        caps["pyiron"] = False
    try:
        from app.simulation.calphad_bridge import check_calphad_available
        caps["CALPHAD"] = check_calphad_available()
    except Exception:
        caps["CALPHAD"] = False
    return caps


def _detect_provider() -> str | None:
    """Detect the active LLM provider from environment."""
    if os.getenv("MARC27_TOKEN"):
        return "MARC27"
    if os.getenv("ANTHROPIC_API_KEY"):
        return "Claude"
    if os.getenv("OPENAI_API_KEY"):
        return "GPT"
    if os.getenv("OPENROUTER_API_KEY"):
        return "OpenRouter"
    return None


def show_welcome(console: Console, agent, auto_approve: bool = False):
    """Render the crystal mascot banner with provider and capability info."""
    from app import __version__
    caps = detect_capabilities()

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

    # Capabilities bar: provider + ML/pyiron/CALPHAD dots + tool/skill count
    status = Text("  ")
    provider = _detect_provider()
    if provider:
        status.append(provider, style=f"bold {SECONDARY}")
        status.append(SEPARATOR, style=DIM)

    for name, ok in caps.items():
        status.append(name, style=DIM)
        status.append(" ")
        if ok:
            status.append(STATUS_DOT_ON, style=SUCCESS)
        else:
            status.append(STATUS_DOT_OFF, style=MUTED)
        status.append("  ")

    tool_count = len(agent.tools.list_tools())
    status.append(f"{tool_count} tools", style=MUTED)
    try:
        from app.skills.registry import load_builtin_skills
        skill_count = len(load_builtin_skills().list_skills())
        status.append(SEPARATOR, style=DIM)
        status.append(f"{skill_count} skills", style=MUTED)
    except Exception:
        pass

    if auto_approve:
        status.append(SEPARATOR, style=DIM)
        status.append("auto-approve", style=WARNING)

    console.print(status)
    console.print()
