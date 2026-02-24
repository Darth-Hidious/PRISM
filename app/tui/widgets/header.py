"""Header widget with C4 glowing hex crystal mascot."""
from textual.widget import Widget
from textual.reactive import reactive
from rich.text import Text
from app.tui.theme import (
    CRYSTAL_OUTER_DIM, CRYSTAL_OUTER, CRYSTAL_INNER, CRYSTAL_CORE,
    RAINBOW, TEXT_DIM, SUCCESS,
)

MASCOT_LINES = [
    "    \u2b21 \u2b21 \u2b21",
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "    \u2b21 \u2b21 \u2b21",
]

HEADER_COMMANDS = ["/help", "/tools", "/skills", "/scratchpad", "/status"]


def build_mascot_line(line_idx: int) -> Text:
    """Build a Rich Text object for one mascot line with glow colors."""
    t = Text()
    raw = MASCOT_LINES[line_idx]
    for ch in raw:
        if ch == "\u2b22":
            t.append(ch, style=f"bold {CRYSTAL_CORE}")
        elif ch == "\u2b21":
            t.append(ch, style=f"{CRYSTAL_OUTER}")
        else:
            t.append(ch)
    return t


def build_rainbow_bar(length: int = 14) -> Text:
    """Build a rainbow bar of \u2501 characters."""
    t = Text()
    for i in range(length):
        t.append("\u2501", style=f"bold {RAINBOW[i % len(RAINBOW)]}")
    return t


def build_header_text(provider: str = "Claude",
                      ml_ready: bool = False,
                      calphad_ready: bool = False) -> Text:
    """Build the complete 4-line header."""
    t = Text()
    mascot_0 = build_mascot_line(0)
    t.append_text(mascot_0)
    t.append("\n")

    mascot_1 = build_mascot_line(1)
    rainbow = build_rainbow_bar()
    t.append_text(mascot_1)
    t.append("  ")
    t.append_text(rainbow)
    t.append("  ")
    for cmd in HEADER_COMMANDS[:3]:
        t.append(cmd, style=TEXT_DIM)
        t.append("  ", style=TEXT_DIM)
    t.append("\n")

    mascot_2 = build_mascot_line(2)
    rainbow2 = build_rainbow_bar()
    t.append_text(mascot_2)
    t.append("  ")
    t.append_text(rainbow2)
    t.append("  ")
    for cmd in HEADER_COMMANDS[3:]:
        t.append(cmd, style=TEXT_DIM)
        t.append("  ", style=TEXT_DIM)
    t.append("\n")

    mascot_3 = build_mascot_line(3)
    t.append_text(mascot_3)
    t.append("           ", style="")
    t.append("MARC27", style=TEXT_DIM)
    t.append(" \u2022 ", style=TEXT_DIM)
    t.append(provider, style=TEXT_DIM)
    t.append(" \u2022 ML ", style=TEXT_DIM)
    t.append("\u25cf" if ml_ready else "\u25cb",
             style=SUCCESS if ml_ready else TEXT_DIM)
    t.append(" CALPHAD ", style=TEXT_DIM)
    t.append("\u25cf" if calphad_ready else "\u25cb",
             style=SUCCESS if calphad_ready else TEXT_DIM)

    return t


class HeaderWidget(Widget):
    """Static header showing mascot, commands, and capabilities."""

    DEFAULT_CSS = """
    HeaderWidget {
        height: 4;
        dock: top;
        padding: 0 1;
    }
    """

    provider = reactive("Claude")
    ml_ready = reactive(False)
    calphad_ready = reactive(False)

    def render(self) -> Text:
        return build_header_text(
            provider=self.provider,
            ml_ready=self.ml_ready,
            calphad_ready=self.calphad_ready,
        )
