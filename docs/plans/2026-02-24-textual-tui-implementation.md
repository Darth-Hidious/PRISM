# Textual TUI Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the Rich+prompt_toolkit REPL with a full Textual app featuring persistent screen zones, typed card stream, modal overlays, error retry, and a hex crystal mascot.

**Architecture:** New `app/tui/` package consumed by `app/cli.py`. The Textual app wraps `AgentCore.process_stream()` in a worker, maps events to card widgets in a scrollable stream. Agent core, events, tools, skills — all untouched.

**Tech Stack:** Textual (>=0.50.0), Rich (already present, used inside Textual widgets)

**Design doc:** `docs/plans/2026-02-24-textual-tui-design.md`

---

### Task 1: Add Textual dependency and scaffold `app/tui/` package

**Files:**
- Modify: `pyproject.toml` (add `textual>=0.50.0` to dependencies)
- Create: `app/tui/__init__.py`
- Create: `app/tui/widgets/__init__.py`
- Create: `app/tui/screens/__init__.py`

**Step 1: Add textual to pyproject.toml**

In `pyproject.toml`, add `"textual>=0.50.0"` to the `dependencies` list after `"prompt_toolkit>=3.0.0"`.

**Step 2: Create empty package files**

```python
# app/tui/__init__.py
"""PRISM Textual TUI."""

# app/tui/widgets/__init__.py
"""TUI widgets."""

# app/tui/screens/__init__.py
"""TUI screens."""
```

**Step 3: Install the new dependency**

Run: `pip install textual>=0.50.0`

**Step 4: Verify import works**

Run: `python3 -c "from textual.app import App; print('OK')"`
Expected: `OK`

**Step 5: Commit**

```bash
git add pyproject.toml app/tui/
git commit -m "chore: scaffold app/tui/ package, add textual dependency"
```

---

### Task 2: Theme and config modules

**Files:**
- Create: `app/tui/theme.py`
- Create: `app/tui/config.py`
- Test: `tests/test_tui_config.py`

**Step 1: Write failing test for config defaults**

```python
# tests/test_tui_config.py
"""Tests for TUI configuration."""

def test_default_truncation_lines():
    from app.tui.config import TUIConfig
    cfg = TUIConfig()
    assert cfg.truncation_lines == 6

def test_default_max_status_tasks():
    from app.tui.config import TUIConfig
    cfg = TUIConfig()
    assert cfg.max_status_tasks == 5

def test_default_auto_scroll():
    from app.tui.config import TUIConfig
    cfg = TUIConfig()
    assert cfg.auto_scroll is True

def test_default_image_preview():
    from app.tui.config import TUIConfig
    cfg = TUIConfig()
    assert cfg.image_preview == "system"

def test_config_override():
    from app.tui.config import TUIConfig
    cfg = TUIConfig(truncation_lines=10, image_preview="none")
    assert cfg.truncation_lines == 10
    assert cfg.image_preview == "none"
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_config.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'app.tui.config'`

**Step 3: Implement config and theme**

```python
# app/tui/config.py
"""TUI configuration with sensible defaults."""
from dataclasses import dataclass


@dataclass
class TUIConfig:
    """User-overridable TUI settings."""
    truncation_lines: int = 6
    max_status_tasks: int = 5
    auto_scroll: bool = True
    image_preview: str = "system"  # "inline" | "system" | "none"
```

```python
# app/tui/theme.py
"""Dark-only theme constants for the PRISM TUI."""

# Surface colors
SURFACE = "#1a1a2e"
SURFACE_DARK = "#0f0f1a"
CARD_BG = "#16213e"

# Text
TEXT_PRIMARY = "#e0e0e0"
TEXT_DIM = "#888888"

# Accents
SUCCESS = "#00cc44"
WARNING = "#d29922"
ERROR = "#cc555a"
INFO = "#0088ff"
ACCENT_MAGENTA = "#bb86fc"
ACCENT_CYAN = "#00cccc"

# Crystal mascot
CRYSTAL_OUTER_DIM = "#555577"
CRYSTAL_OUTER = "#7777aa"
CRYSTAL_INNER = "#ccccff"
CRYSTAL_CORE = "#ffffff"

# Rainbow (10 stops, VIBGYOR)
RAINBOW = [
    "#ff0000", "#ff5500", "#ff8800", "#ffcc00", "#88ff00",
    "#00cc44", "#00cccc", "#0088ff", "#5500ff", "#8b00ff",
]

# Card border colors by type
CARD_BORDERS = {
    "input": ACCENT_CYAN,
    "output": TEXT_DIM,
    "tool": SUCCESS,
    "error": ERROR,
    "error_partial": WARNING,
    "approval": WARNING,
    "plan": ACCENT_MAGENTA,
    "metrics": INFO,
    "calphad": INFO,
    "validation_critical": ERROR,
    "validation_warning": WARNING,
    "validation_info": INFO,
    "results": TEXT_DIM,
    "plot": SUCCESS,
}
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_config.py -v`
Expected: All 5 PASS

**Step 5: Commit**

```bash
git add app/tui/theme.py app/tui/config.py tests/test_tui_config.py
git commit -m "feat(tui): add theme constants and configurable settings"
```

---

### Task 3: Keymap module

**Files:**
- Create: `app/tui/keymap.py`
- Test: `tests/test_tui_keymap.py`

**Step 1: Write failing test**

```python
# tests/test_tui_keymap.py
"""Tests for TUI key bindings."""

def test_keymap_has_required_bindings():
    from app.tui.keymap import KEYMAP
    required = ["ctrl+o", "ctrl+q", "ctrl+s", "ctrl+l", "ctrl+p", "ctrl+t"]
    for key in required:
        assert key in KEYMAP, f"Missing binding: {key}"

def test_keymap_actions_are_strings():
    from app.tui.keymap import KEYMAP
    for key, action in KEYMAP.items():
        assert isinstance(action, str), f"{key} action is not a string"

def test_card_actions_exist():
    from app.tui.keymap import CARD_ACTIONS
    assert "r" in CARD_ACTIONS  # retry
    assert "s" in CARD_ACTIONS  # skip
    assert "y" in CARD_ACTIONS  # approve
    assert "n" in CARD_ACTIONS  # deny
    assert "a" in CARD_ACTIONS  # always
    assert "e" in CARD_ACTIONS  # export
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_keymap.py -v`
Expected: FAIL

**Step 3: Implement keymap**

```python
# app/tui/keymap.py
"""Key binding definitions for the PRISM TUI.

Global bindings apply everywhere. Card actions apply when a
specific card type is focused.
"""

# Global key bindings: key → action name
KEYMAP = {
    "ctrl+o": "expand_content",      # Open modal overlay for focused card
    "ctrl+q": "view_task_queue",     # View task queue detail
    "ctrl+s": "save_session",        # Save current session
    "ctrl+l": "clear_stream",        # Clear the output stream
    "ctrl+p": "toggle_plan_mode",    # Toggle plan mode
    "ctrl+t": "list_tools",          # Show available tools
    "ctrl+c": "cancel_operation",    # Cancel current agent operation
    "ctrl+d": "exit_app",            # Exit the application
    "escape": "dismiss_modal",       # Dismiss modal / cancel
}

# Card-local actions: key → action name
CARD_ACTIONS = {
    "r": "retry_failed",             # Retry failed tool (ErrorRetryCard)
    "s": "skip_failed",              # Skip failed tool (ErrorRetryCard)
    "y": "approve_tool",             # Approve tool execution (ApprovalCard)
    "n": "deny_tool",                # Deny tool execution (ApprovalCard)
    "a": "always_approve_tool",      # Always approve this tool (ApprovalCard)
    "e": "export_csv",               # Export results to CSV (ResultsTableCard)
}

# Human-readable descriptions for /help display
BINDING_DESCRIPTIONS = {
    "ctrl+o": "View full content",
    "ctrl+q": "View task queue",
    "ctrl+s": "Save session",
    "ctrl+l": "Clear output",
    "ctrl+p": "Toggle plan mode",
    "ctrl+t": "List tools",
    "ctrl+c": "Cancel",
    "ctrl+d": "Exit",
    "escape": "Dismiss / back",
}
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_keymap.py -v`
Expected: All 3 PASS

**Step 5: Commit**

```bash
git add app/tui/keymap.py tests/test_tui_keymap.py
git commit -m "feat(tui): add keymap with global and card-local bindings"
```

---

### Task 4: Header widget with C4 hex crystal mascot

**Files:**
- Create: `app/tui/widgets/header.py`
- Test: `tests/test_tui_header.py`

**Step 1: Write failing test**

```python
# tests/test_tui_header.py
"""Tests for the header widget."""
from textual.app import App, ComposeResult

def test_header_widget_renders():
    """HeaderWidget can be instantiated and has expected content."""
    from app.tui.widgets.header import HeaderWidget
    widget = HeaderWidget()
    assert widget is not None

def test_header_contains_mascot_chars():
    from app.tui.widgets.header import MASCOT_LINES
    # C4 glowing hex crystal uses these chars
    combined = "".join(MASCOT_LINES)
    assert "⬢" in combined
    assert "⬡" in combined

def test_header_contains_commands():
    from app.tui.widgets.header import HEADER_COMMANDS
    assert "/help" in HEADER_COMMANDS
    assert "/tools" in HEADER_COMMANDS
    assert "/skills" in HEADER_COMMANDS
    assert "/scratchpad" in HEADER_COMMANDS
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_header.py -v`
Expected: FAIL

**Step 3: Implement header widget**

```python
# app/tui/widgets/header.py
"""Header widget with C4 glowing hex crystal mascot."""
from textual.widget import Widget
from textual.reactive import reactive
from rich.text import Text
from app.tui.theme import (
    CRYSTAL_OUTER_DIM, CRYSTAL_OUTER, CRYSTAL_INNER, CRYSTAL_CORE,
    RAINBOW, TEXT_DIM, SUCCESS,
)

MASCOT_LINES = [
    "    ⬡ ⬡ ⬡",
    "  ⬡ ⬢ ⬢ ⬢ ⬡",
    "  ⬡ ⬢ ⬢ ⬢ ⬡",
    "    ⬡ ⬡ ⬡",
]

HEADER_COMMANDS = ["/help", "/tools", "/skills", "/scratchpad", "/status"]


def build_mascot_line(line_idx: int) -> Text:
    """Build a Rich Text object for one mascot line with glow colors."""
    t = Text()
    raw = MASCOT_LINES[line_idx]
    for ch in raw:
        if ch == "⬢":
            t.append(ch, style=f"bold {CRYSTAL_CORE}")
        elif ch == "⬡":
            t.append(ch, style=f"{CRYSTAL_OUTER}")
        else:
            t.append(ch)
    return t


def build_rainbow_bar(length: int = 14) -> Text:
    """Build a rainbow bar of ━ characters."""
    t = Text()
    for i in range(length):
        t.append("━", style=f"bold {RAINBOW[i % len(RAINBOW)]}")
    return t


def build_header_text(provider: str = "Claude",
                      ml_ready: bool = False,
                      calphad_ready: bool = False) -> Text:
    """Build the complete 4-line header."""
    t = Text()
    # Line 0: outer hex top
    mascot_0 = build_mascot_line(0)
    t.append_text(mascot_0)
    t.append("\n")

    # Line 1: inner hex + rainbow + commands
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

    # Line 2: inner hex + rainbow + more commands
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

    # Line 3: outer hex bottom + provider + capabilities
    mascot_3 = build_mascot_line(3)
    t.append_text(mascot_3)
    t.append("           ", style="")
    t.append("MARC27", style=TEXT_DIM)
    t.append(" • ", style=TEXT_DIM)
    t.append(provider, style=TEXT_DIM)
    t.append(" • ML ", style=TEXT_DIM)
    t.append("●" if ml_ready else "○",
             style=SUCCESS if ml_ready else TEXT_DIM)
    t.append(" CALPHAD ", style=TEXT_DIM)
    t.append("●" if calphad_ready else "○",
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
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_header.py -v`
Expected: All 3 PASS

**Step 5: Commit**

```bash
git add app/tui/widgets/header.py tests/test_tui_header.py
git commit -m "feat(tui): header widget with C4 hex crystal mascot"
```

---

### Task 5: Card widgets

**Files:**
- Create: `app/tui/widgets/cards.py`
- Test: `tests/test_tui_cards.py`

**Step 1: Write failing test**

```python
# tests/test_tui_cards.py
"""Tests for TUI card widgets."""

def test_input_card_stores_message():
    from app.tui.widgets.cards import InputCard
    card = InputCard("Find W-Rh alloys")
    assert card.message == "Find W-Rh alloys"

def test_output_card_truncation():
    from app.tui.widgets.cards import OutputCard
    long_text = "\n".join(f"Line {i}" for i in range(20))
    card = OutputCard(long_text, truncation_lines=6)
    assert card.is_truncated is True
    assert card.full_content == long_text

def test_output_card_no_truncation_when_short():
    from app.tui.widgets.cards import OutputCard
    card = OutputCard("Short text", truncation_lines=6)
    assert card.is_truncated is False

def test_tool_card_success():
    from app.tui.widgets.cards import ToolCard
    card = ToolCard(
        tool_name="search_optimade",
        elapsed_ms=17000,
        summary="49 results",
        result={"count": 49, "results": []},
    )
    assert card.tool_name == "search_optimade"
    assert card.is_error is False

def test_tool_card_error():
    from app.tui.widgets.cards import ToolCard
    card = ToolCard(
        tool_name="search_optimade",
        elapsed_ms=5000,
        summary="",
        result={"error": "Connection failed"},
    )
    assert card.is_error is True

def test_card_type_detection_metrics():
    from app.tui.widgets.cards import detect_card_type
    result = {"metrics": {"mae": 0.04}, "algorithm": "random_forest"}
    assert detect_card_type(result) == "metrics"

def test_card_type_detection_calphad():
    from app.tui.widgets.cards import detect_card_type
    result = {"phases_present": ["BCC"], "gibbs_energy": -1234.5}
    assert detect_card_type(result) == "calphad"

def test_card_type_detection_validation():
    from app.tui.widgets.cards import detect_card_type
    result = {"findings": [], "quality_score": 0.9}
    assert detect_card_type(result) == "validation"

def test_card_type_detection_results_table():
    from app.tui.widgets.cards import detect_card_type
    result = {"results": [{"a": 1}] * 5, "count": 5}
    assert detect_card_type(result) == "results_table"

def test_card_type_detection_plot():
    from app.tui.widgets.cards import detect_card_type
    result = {"filename": "plot.png"}
    assert detect_card_type(result) == "plot"

def test_card_type_detection_error():
    from app.tui.widgets.cards import detect_card_type
    result = {"error": "something broke"}
    assert detect_card_type(result) == "error"

def test_card_type_detection_default():
    from app.tui.widgets.cards import detect_card_type
    result = {"success": True}
    assert detect_card_type(result) == "tool"
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_cards.py -v`
Expected: FAIL

**Step 3: Implement cards**

```python
# app/tui/widgets/cards.py
"""Card widgets for the PRISM TUI output stream.

Each card type represents a distinct event in the agent loop.
Cards are added to the StreamView chronologically.
"""
from textual.widget import Widget
from textual.reactive import reactive
from rich.text import Text
from rich.panel import Panel
from rich.markdown import Markdown
from rich.table import Table
from app.tui.theme import CARD_BORDERS, TEXT_DIM, SUCCESS, ERROR, WARNING, INFO


def detect_card_type(result: dict) -> str:
    """Determine card type from a tool result dict shape."""
    if "error" in result:
        return "error"
    if "metrics" in result and "algorithm" in result:
        return "metrics"
    if "phases_present" in result and "gibbs_energy" in result:
        return "calphad"
    if "findings" in result and "quality_score" in result:
        return "validation"
    if isinstance(result.get("filename"), str) and result["filename"].endswith(".png"):
        return "plot"
    if isinstance(result.get("results"), list) and len(result["results"]) > 3:
        return "results_table"
    return "tool"


class InputCard(Widget):
    """Displays a user input message."""

    DEFAULT_CSS = """
    InputCard {
        height: auto;
        margin: 0 0 1 0;
    }
    """

    def __init__(self, message: str, **kwargs):
        super().__init__(**kwargs)
        self.message = message

    def render(self) -> Panel:
        return Panel(
            Text(self.message),
            title="input",
            title_align="left",
            border_style=CARD_BORDERS["input"],
            padding=(0, 1),
        )


class OutputCard(Widget):
    """Displays agent text output with optional truncation."""

    DEFAULT_CSS = """
    OutputCard {
        height: auto;
        margin: 0 0 1 0;
    }
    """

    def __init__(self, content: str, truncation_lines: int = 6, **kwargs):
        super().__init__(**kwargs)
        self.full_content = content
        self.truncation_lines = truncation_lines
        lines = content.split("\n")
        self.is_truncated = len(lines) > truncation_lines
        if self.is_truncated:
            self._display = "\n".join(lines[:truncation_lines]) + "\n..."
        else:
            self._display = content

    def render(self) -> Panel:
        title_text = Text()
        title_text.append("output", style=TEXT_DIM)
        footer = None
        if self.is_truncated:
            footer = Text("Ctrl+O", style=TEXT_DIM)
        return Panel(
            Markdown(self._display),
            title=title_text,
            title_align="left",
            subtitle=footer,
            subtitle_align="right",
            border_style=CARD_BORDERS["output"],
            padding=(0, 1),
        )


class ToolCard(Widget):
    """Displays a tool execution result."""

    DEFAULT_CSS = """
    ToolCard {
        height: auto;
        margin: 0 0 1 0;
    }
    """

    def __init__(self, tool_name: str, elapsed_ms: float, summary: str,
                 result: dict, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.summary = summary
        self.result = result
        self.is_error = "error" in result

    def _format_elapsed(self) -> str:
        if self.elapsed_ms >= 1000:
            return f"{self.elapsed_ms / 1000:.1f}s"
        return f"{self.elapsed_ms:.0f}ms"

    def render(self) -> Panel:
        card_type = detect_card_type(self.result)
        border = CARD_BORDERS.get(card_type, CARD_BORDERS["tool"])

        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" {self._format_elapsed()} ", style=TEXT_DIM)

        body = Text()
        if self.is_error:
            body.append("✗ ", style=f"bold {ERROR}")
            body.append(str(self.result.get("error", ""))[:200], style=TEXT_DIM)
        else:
            body.append("✔ ", style=f"bold {SUCCESS}")
            body.append(self.summary or "completed", style=TEXT_DIM)

        return Panel(
            body,
            title=title,
            title_align="left",
            border_style=border,
            padding=(0, 1),
        )


class ApprovalCard(Widget):
    """Displays a tool approval request."""

    DEFAULT_CSS = """
    ApprovalCard {
        height: auto;
        margin: 0 0 1 0;
    }
    """

    def __init__(self, tool_name: str, tool_args: dict, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.tool_args = tool_args

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(" approval ", style=f"bold {WARNING}")

        body = Text()
        args_preview = ", ".join(
            f"{k}={v!r}" for k, v in list(self.tool_args.items())[:3]
        )
        body.append(f"⚠ Requires approval\n", style=WARNING)
        body.append(f"  {args_preview}\n", style=TEXT_DIM)
        body.append("\n  [y] approve  [n] deny  [a] always", style=TEXT_DIM)

        return Panel(
            body,
            title=title,
            title_align="left",
            border_style=CARD_BORDERS["approval"],
            padding=(0, 1),
        )


class PlanCard(Widget):
    """Displays an agent plan for user confirmation."""

    DEFAULT_CSS = """
    PlanCard {
        height: auto;
        margin: 0 0 1 0;
    }
    """

    def __init__(self, plan_text: str, **kwargs):
        super().__init__(**kwargs)
        self.plan_text = plan_text

    def render(self) -> Panel:
        return Panel(
            Markdown(self.plan_text),
            title=Text(" plan ", style=f"bold {INFO}"),
            title_align="left",
            subtitle=Text("[y] execute  [n] reject", style=TEXT_DIM),
            subtitle_align="right",
            border_style=CARD_BORDERS["plan"],
            padding=(1, 2),
        )
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_cards.py -v`
Expected: All 13 PASS

**Step 5: Commit**

```bash
git add app/tui/widgets/cards.py tests/test_tui_cards.py
git commit -m "feat(tui): card widgets — input, output, tool, approval, plan"
```

---

### Task 6: Stream view widget

**Files:**
- Create: `app/tui/widgets/stream.py`
- Test: `tests/test_tui_stream.py`

**Step 1: Write failing test**

```python
# tests/test_tui_stream.py
"""Tests for the stream view widget."""

def test_stream_view_instantiates():
    from app.tui.widgets.stream import StreamView
    sv = StreamView()
    assert sv is not None

def test_stream_view_auto_scroll_default():
    from app.tui.widgets.stream import StreamView
    sv = StreamView()
    assert sv.auto_scroll is True
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_stream.py -v`
Expected: FAIL

**Step 3: Implement stream view**

```python
# app/tui/widgets/stream.py
"""Scrollable stream view for the card-based output."""
from textual.containers import VerticalScroll
from textual.widget import Widget


class StreamView(VerticalScroll):
    """Scrollable container for output cards.

    Auto-scrolls to bottom on new cards. Pauses auto-scroll
    when user scrolls up. Resumes on scroll-to-bottom or new input.
    """

    DEFAULT_CSS = """
    StreamView {
        height: 1fr;
        padding: 0 1;
    }
    """

    auto_scroll = True

    def add_card(self, card: Widget) -> None:
        """Mount a card and optionally scroll to it."""
        self.mount(card)
        if self.auto_scroll:
            card.scroll_visible()

    def on_scroll_up(self) -> None:
        """Pause auto-scroll when user scrolls up."""
        self.auto_scroll = False

    def resume_auto_scroll(self) -> None:
        """Resume auto-scroll (called on new user input)."""
        self.auto_scroll = True
        self.scroll_end(animate=False)
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_stream.py -v`
Expected: All 2 PASS

**Step 5: Commit**

```bash
git add app/tui/widgets/stream.py tests/test_tui_stream.py
git commit -m "feat(tui): scrollable stream view with auto-scroll"
```

---

### Task 7: Status bar widget (agent status + task tracker)

**Files:**
- Create: `app/tui/widgets/status_bar.py`
- Test: `tests/test_tui_status_bar.py`

**Step 1: Write failing test**

```python
# tests/test_tui_status_bar.py
"""Tests for the status bar widget."""

def test_status_bar_instantiates():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    assert sb is not None

def test_status_bar_update_spinner():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.update_agent_step("Searching OPTIMADE databases...")
    assert sb.current_step == "Searching OPTIMADE databases..."

def test_status_bar_update_next_step():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.update_next_step("Parse results")
    assert sb.next_step == "Parse results"

def test_status_bar_add_task():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.add_task("Search alloy databases")
    assert len(sb.tasks) == 1
    assert sb.tasks[0]["label"] == "Search alloy databases"
    assert sb.tasks[0]["status"] == "pending"

def test_status_bar_complete_task():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.add_task("Search alloy databases")
    sb.complete_task(0)
    assert sb.tasks[0]["status"] == "done"

def test_status_bar_start_task():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.add_task("Search alloy databases")
    sb.start_task(0)
    assert sb.tasks[0]["status"] == "running"

def test_status_bar_task_ordering():
    """Done tasks move down, upcoming shown on top."""
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.add_task("Task A")
    sb.add_task("Task B")
    sb.add_task("Task C")
    sb.complete_task(0)
    sb.start_task(1)
    ordered = sb.get_display_tasks()
    # Running first, then pending, then done
    assert ordered[0]["status"] == "running"
    assert ordered[1]["status"] == "pending"
    assert ordered[2]["status"] == "done"

def test_status_bar_truncates_after_max():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar(max_visible_tasks=3)
    for i in range(10):
        sb.add_task(f"Task {i}")
    displayed = sb.get_display_tasks(max_visible=3)
    assert len(displayed) <= 3
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_status_bar.py -v`
Expected: FAIL

**Step 3: Implement status bar**

```python
# app/tui/widgets/status_bar.py
"""Status bar with agent step spinner and task tracker."""
from textual.widget import Widget
from textual.reactive import reactive
from rich.text import Text
from app.tui.theme import TEXT_DIM, SUCCESS, WARNING, ACCENT_MAGENTA
from app.agent.spinner import BRAILLE_FRAMES


class StatusBar(Widget):
    """Bottom status area: agent step + task tracker.

    Agent step = what the agent is doing RIGHT NOW (ephemeral).
    Tasks = the higher-level plan items (persistent).
    """

    DEFAULT_CSS = """
    StatusBar {
        dock: bottom;
        height: auto;
        max-height: 12;
        padding: 0 1;
    }
    """

    current_step = reactive("")
    next_step = reactive("")
    spinner_frame = reactive(0)
    is_thinking = reactive(False)

    def __init__(self, max_visible_tasks: int = 5, **kwargs):
        super().__init__(**kwargs)
        self.max_visible_tasks = max_visible_tasks
        self.tasks: list[dict] = []

    def update_agent_step(self, step: str) -> None:
        self.current_step = step
        self.is_thinking = True

    def update_next_step(self, step: str) -> None:
        self.next_step = step

    def stop_thinking(self) -> None:
        self.is_thinking = False
        self.current_step = ""
        self.next_step = ""

    def add_task(self, label: str) -> int:
        idx = len(self.tasks)
        self.tasks.append({"label": label, "status": "pending"})
        self.refresh()
        return idx

    def start_task(self, idx: int) -> None:
        if 0 <= idx < len(self.tasks):
            self.tasks[idx]["status"] = "running"
            self.refresh()

    def complete_task(self, idx: int) -> None:
        if 0 <= idx < len(self.tasks):
            self.tasks[idx]["status"] = "done"
            self.refresh()

    def get_display_tasks(self, max_visible: int | None = None) -> list[dict]:
        """Return tasks sorted: running → pending → done, truncated."""
        order = {"running": 0, "pending": 1, "done": 2}
        sorted_tasks = sorted(self.tasks, key=lambda t: order.get(t["status"], 3))
        limit = max_visible or self.max_visible_tasks
        return sorted_tasks[:limit]

    def advance_spinner(self) -> None:
        self.spinner_frame = (self.spinner_frame + 1) % len(BRAILLE_FRAMES)

    def render(self) -> Text:
        t = Text()

        # Agent step spinner
        if self.is_thinking:
            frame = BRAILLE_FRAMES[self.spinner_frame]
            t.append(f"  {frame} ", style=f"bold {ACCENT_MAGENTA}")
            t.append(self.current_step or "Thinking...", style="italic")
            if self.current_step:
                t.append(f"\n     └── {self.current_step}", style=TEXT_DIM)
                t.append("  [Ctrl+O]", style=TEXT_DIM)
            if self.next_step:
                t.append(f"\n     └── Next: {self.next_step}", style=TEXT_DIM)
            t.append("\n")

        # Task tracker
        if self.tasks:
            done = sum(1 for t_ in self.tasks if t_["status"] == "done")
            total = len(self.tasks)
            t.append(f"\n  ◉ Tasks [{done}/{total}]", style="bold")
            t.append("  [Ctrl+Q]\n", style=TEXT_DIM)

            for task in self.get_display_tasks():
                if task["status"] == "done":
                    t.append(f"    ✔ {task['label']}\n", style=SUCCESS)
                elif task["status"] == "running":
                    t.append(f"    ▸ {task['label']}...\n", style=WARNING)
                else:
                    t.append(f"    ○ {task['label']}\n", style=TEXT_DIM)

            remaining = total - len(self.get_display_tasks())
            if remaining > 0:
                t.append(f"    ... +{remaining} more\n", style=TEXT_DIM)

        return t
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_status_bar.py -v`
Expected: All 8 PASS

**Step 5: Commit**

```bash
git add app/tui/widgets/status_bar.py tests/test_tui_status_bar.py
git commit -m "feat(tui): status bar with agent step spinner and task tracker"
```

---

### Task 8: Input bar widget

**Files:**
- Create: `app/tui/widgets/input_bar.py`
- Test: `tests/test_tui_input_bar.py`

**Step 1: Write failing test**

```python
# tests/test_tui_input_bar.py
"""Tests for the input bar widget."""

def test_input_bar_instantiates():
    from app.tui.widgets.input_bar import InputBar
    ib = InputBar()
    assert ib is not None

def test_input_bar_has_placeholder():
    from app.tui.widgets.input_bar import InputBar
    ib = InputBar()
    assert ib.placeholder != ""
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_input_bar.py -v`
Expected: FAIL

**Step 3: Implement input bar**

```python
# app/tui/widgets/input_bar.py
"""Input bar widget pinned to the bottom of the screen."""
from textual.widgets import Input
from app.tui.theme import ACCENT_MAGENTA


class InputBar(Input):
    """Text input bar for user messages.

    Pinned at the very bottom. On submit, the message is sent to
    PrismApp which creates an InputCard in the stream and clears
    this widget.
    """

    DEFAULT_CSS = """
    InputBar {
        dock: bottom;
        height: 3;
        padding: 0 1;
    }
    """

    def __init__(self, **kwargs):
        super().__init__(
            placeholder="Ask PRISM anything...",
            **kwargs,
        )
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_input_bar.py -v`
Expected: All 2 PASS

**Step 5: Commit**

```bash
git add app/tui/widgets/input_bar.py tests/test_tui_input_bar.py
git commit -m "feat(tui): input bar widget"
```

---

### Task 9: Modal overlay screen

**Files:**
- Create: `app/tui/screens/overlay.py`
- Test: `tests/test_tui_overlay.py`

**Step 1: Write failing test**

```python
# tests/test_tui_overlay.py
"""Tests for the modal overlay screen."""

def test_overlay_stores_content():
    from app.tui.screens.overlay import FullContentScreen
    screen = FullContentScreen("Full content here", title="search_optimade")
    assert screen.content == "Full content here"
    assert screen.title_text == "search_optimade"
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_overlay.py -v`
Expected: FAIL

**Step 3: Implement overlay**

```python
# app/tui/screens/overlay.py
"""Full-content modal overlay screen."""
from textual.screen import ModalScreen
from textual.containers import VerticalScroll
from textual.widgets import Static
from rich.markdown import Markdown
from rich.panel import Panel
from app.tui.theme import TEXT_DIM, ACCENT_MAGENTA


class FullContentScreen(ModalScreen):
    """Modal overlay showing full content of a truncated card.

    Press Escape to dismiss.
    """

    BINDINGS = [("escape", "dismiss", "Close")]

    DEFAULT_CSS = """
    FullContentScreen {
        align: center middle;
    }

    #overlay-container {
        width: 90%;
        height: 85%;
        background: $surface;
        border: round $accent;
        padding: 1 2;
        overflow-y: auto;
    }
    """

    def __init__(self, content: str, title: str = "", **kwargs):
        super().__init__(**kwargs)
        self.content = content
        self.title_text = title

    def compose(self):
        with VerticalScroll(id="overlay-container"):
            yield Static(
                Panel(
                    Markdown(self.content),
                    title=self.title_text,
                    title_align="left",
                    border_style=ACCENT_MAGENTA,
                    subtitle="Escape to close",
                    subtitle_align="right",
                    padding=(1, 2),
                )
            )

    def action_dismiss(self) -> None:
        self.app.pop_screen()
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_overlay.py -v`
Expected: All 1 PASS

**Step 5: Commit**

```bash
git add app/tui/screens/overlay.py tests/test_tui_overlay.py
git commit -m "feat(tui): modal overlay screen for full content view"
```

---

### Task 10: Main PrismApp — compose and wire everything

**Files:**
- Create: `app/tui/app.py`
- Test: `tests/test_tui_app.py`

**Step 1: Write failing test**

```python
# tests/test_tui_app.py
"""Tests for the main PrismApp."""
import pytest


def test_prism_app_instantiates():
    from app.tui.app import PrismApp
    app = PrismApp()
    assert app is not None


@pytest.mark.asyncio
async def test_prism_app_has_all_widgets():
    """PrismApp composes the expected widget tree."""
    from app.tui.app import PrismApp
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        app = pilot.app
        from app.tui.widgets.header import HeaderWidget
        from app.tui.widgets.stream import StreamView
        from app.tui.widgets.status_bar import StatusBar
        from app.tui.widgets.input_bar import InputBar
        assert app.query_one(HeaderWidget) is not None
        assert app.query_one(StreamView) is not None
        assert app.query_one(StatusBar) is not None
        assert app.query_one(InputBar) is not None
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_app.py -v`
Expected: FAIL

**Step 3: Implement PrismApp**

```python
# app/tui/app.py
"""Main PRISM Textual application."""
from textual.app import App, ComposeResult
from textual.binding import Binding

from app.tui.widgets.header import HeaderWidget
from app.tui.widgets.stream import StreamView
from app.tui.widgets.cards import InputCard, OutputCard, ToolCard, ApprovalCard, PlanCard, detect_card_type
from app.tui.widgets.status_bar import StatusBar
from app.tui.widgets.input_bar import InputBar
from app.tui.screens.overlay import FullContentScreen
from app.tui.keymap import KEYMAP, BINDING_DESCRIPTIONS
from app.tui.config import TUIConfig
from app.tui.theme import SURFACE


class PrismApp(App):
    """PRISM AI Materials Discovery — Textual TUI."""

    CSS = """
    Screen {
        background: """ + SURFACE + """;
    }
    """

    BINDINGS = [
        Binding("ctrl+o", "expand_content", BINDING_DESCRIPTIONS.get("ctrl+o", "")),
        Binding("ctrl+q", "view_task_queue", BINDING_DESCRIPTIONS.get("ctrl+q", "")),
        Binding("ctrl+s", "save_session", BINDING_DESCRIPTIONS.get("ctrl+s", "")),
        Binding("ctrl+l", "clear_stream", BINDING_DESCRIPTIONS.get("ctrl+l", "")),
        Binding("ctrl+p", "toggle_plan_mode", BINDING_DESCRIPTIONS.get("ctrl+p", "")),
        Binding("ctrl+t", "list_tools", BINDING_DESCRIPTIONS.get("ctrl+t", "")),
        Binding("ctrl+d", "exit_app", BINDING_DESCRIPTIONS.get("ctrl+d", "")),
    ]

    def __init__(self, backend=None, enable_mcp: bool = True,
                 auto_approve: bool = False, config: TUIConfig | None = None,
                 **kwargs):
        super().__init__(**kwargs)
        self.config = config or TUIConfig()
        self._backend = backend
        self._enable_mcp = enable_mcp
        self._auto_approve = auto_approve
        self._agent = None  # Lazy init — needs backend

    def compose(self) -> ComposeResult:
        yield HeaderWidget()
        yield StreamView()
        yield StatusBar(max_visible_tasks=self.config.max_status_tasks)
        yield InputBar()

    def on_mount(self) -> None:
        """Focus the input bar on startup."""
        self.query_one(InputBar).focus()
        # Start spinner timer (12.5 fps)
        self.set_interval(0.08, self._tick_spinner)

    def _tick_spinner(self) -> None:
        """Advance the spinner animation frame."""
        status = self.query_one(StatusBar)
        if status.is_thinking:
            status.advance_spinner()

    async def on_input_submitted(self, event: InputBar.Submitted) -> None:
        """Handle user input submission."""
        message = event.value.strip()
        if not message:
            return

        input_bar = self.query_one(InputBar)
        input_bar.value = ""

        stream = self.query_one(StreamView)
        stream.resume_auto_scroll()

        # Handle / commands
        if message.startswith("/"):
            await self._handle_command(message)
            return

        # Add input card to stream
        stream.add_card(InputCard(message))

        # Process with agent (in worker to avoid blocking)
        if self._agent:
            self.run_worker(self._process_agent_message(message))

    async def _process_agent_message(self, message: str) -> None:
        """Run agent.process_stream() and map events to cards."""
        from app.agent.events import (
            TextDelta, ToolCallStart, ToolCallResult,
            TurnComplete, ToolApprovalRequest,
        )
        import time

        stream = self.query_one(StreamView)
        status = self.query_one(StatusBar)
        accumulated_text = ""
        tool_start_time = 0.0

        try:
            for event in self._agent.process_stream(message):
                if isinstance(event, TextDelta):
                    accumulated_text += event.text

                elif isinstance(event, ToolCallStart):
                    # Flush accumulated text
                    if accumulated_text.strip():
                        stream.add_card(OutputCard(
                            accumulated_text.strip(),
                            truncation_lines=self.config.truncation_lines,
                        ))
                        accumulated_text = ""
                    # Update agent status
                    from app.agent.spinner import TOOL_VERBS
                    verb = TOOL_VERBS.get(event.tool_name, "Thinking...")
                    status.update_agent_step(verb)
                    tool_start_time = time.time()

                elif isinstance(event, ToolCallResult):
                    elapsed = (time.time() - tool_start_time) * 1000
                    status.stop_thinking()
                    stream.add_card(ToolCard(
                        tool_name=event.tool_name,
                        elapsed_ms=elapsed,
                        summary=event.summary,
                        result=event.result if isinstance(event.result, dict) else {},
                    ))

                elif isinstance(event, ToolApprovalRequest):
                    stream.add_card(ApprovalCard(
                        tool_name=event.tool_name,
                        tool_args=event.tool_args,
                    ))

                elif isinstance(event, TurnComplete):
                    if accumulated_text.strip():
                        stream.add_card(OutputCard(
                            accumulated_text.strip(),
                            truncation_lines=self.config.truncation_lines,
                        ))
                        accumulated_text = ""
                    status.stop_thinking()

        except Exception as e:
            stream.add_card(OutputCard(f"Error: {e}"))
            status.stop_thinking()

    async def _handle_command(self, command: str) -> None:
        """Handle / commands."""
        stream = self.query_one(StreamView)
        cmd = command.split()[0].lower()

        if cmd in ("/exit", "/quit"):
            self.exit()
        elif cmd == "/clear":
            stream.remove_children()
        elif cmd == "/help":
            from app.tui.keymap import BINDING_DESCRIPTIONS
            from app.agent.repl import REPL_COMMANDS
            help_text = "**Commands:**\n"
            for c, desc in REPL_COMMANDS.items():
                help_text += f"- `{c}` — {desc}\n"
            help_text += "\n**Key Bindings:**\n"
            for key, desc in BINDING_DESCRIPTIONS.items():
                help_text += f"- `{key}` — {desc}\n"
            stream.add_card(OutputCard(help_text))
        else:
            stream.add_card(OutputCard(f"Unknown command: {cmd}"))

    # --- Actions ---

    def action_expand_content(self) -> None:
        """Open modal overlay for focused card."""
        focused = self.focused
        content = ""
        title = ""
        if hasattr(focused, "full_content"):
            content = focused.full_content
            title = "output"
        elif hasattr(focused, "result"):
            import json
            content = json.dumps(focused.result, indent=2, default=str)
            title = getattr(focused, "tool_name", "details")
        elif hasattr(focused, "plan_text"):
            content = focused.plan_text
            title = "plan"
        if content:
            self.push_screen(FullContentScreen(content, title=title))

    def action_view_task_queue(self) -> None:
        status = self.query_one(StatusBar)
        lines = []
        for i, task in enumerate(status.tasks):
            icon = {"done": "✔", "running": "▸", "pending": "○"}[task["status"]]
            lines.append(f"{icon} {task['label']}")
        content = "\n".join(lines) if lines else "No tasks."
        self.push_screen(FullContentScreen(content, title="Task Queue"))

    def action_save_session(self) -> None:
        pass  # Wired in Task 11

    def action_clear_stream(self) -> None:
        self.query_one(StreamView).remove_children()

    def action_toggle_plan_mode(self) -> None:
        pass  # Wired in Task 11

    def action_list_tools(self) -> None:
        pass  # Wired in Task 11

    def action_exit_app(self) -> None:
        self.exit()
```

**Step 4: Run tests (install pytest-asyncio first)**

Run: `pip install pytest-asyncio && python3 -m pytest tests/test_tui_app.py -v`
Expected: All 2 PASS

**Step 5: Commit**

```bash
git add app/tui/app.py tests/test_tui_app.py
git commit -m "feat(tui): main PrismApp with compose, event loop, key bindings"
```

---

### Task 11: Wire TUI into CLI

**Files:**
- Modify: `app/cli.py` (~lines 58, 179–292)
- Test: `tests/test_cli.py` (add new test)

**Step 1: Write failing test**

```python
# Add to tests/test_cli.py (or create tests/test_cli_tui.py)

def test_cli_has_tui_flag(cli_runner):
    """CLI accepts --tui and --classic flags."""
    from click.testing import CliRunner
    from app.cli import cli
    runner = CliRunner()
    result = runner.invoke(cli, ["--help"])
    assert "--tui" in result.output or "--classic" in result.output
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_cli_tui.py -v` (or add to existing test_cli.py)
Expected: FAIL

**Step 3: Add --tui / --classic flags to cli.py**

In `app/cli.py`, add the options to the `cli` group (around line 188):

```python
@click.option('--tui/--classic', default=True, help='Use Textual TUI (default) or classic REPL')
```

Add `tui` parameter to the `cli` function signature.

In the REPL launch block (around line 280), change to:

```python
if tui:
    from app.tui.app import PrismApp
    app = PrismApp(
        backend=create_backend(),
        enable_mcp=not no_mcp,
        auto_approve=dangerously_accept_all,
    )
    app.run()
else:
    # Classic REPL fallback
    backend = create_backend()
    repl = AgentREPL(backend=backend, enable_mcp=not no_mcp,
                     auto_approve=dangerously_accept_all)
    if resume:
        try:
            repl._load_session(resume)
            console.print(f"[green]Resumed session: {resume}[/green]")
        except FileNotFoundError:
            console.print(f"[red]Session not found: {resume}[/red]")
            return
    repl.run()
```

**Step 4: Run test**

Run: `python3 -m pytest tests/test_cli_tui.py -v`
Expected: PASS

**Step 5: Commit**

```bash
git add app/cli.py tests/test_cli_tui.py
git commit -m "feat(tui): wire PrismApp into CLI with --tui/--classic flag"
```

---

### Task 12: Wire AgentCore into PrismApp (lazy init + approval callback)

**Files:**
- Modify: `app/tui/app.py`
- Test: `tests/test_tui_agent_wiring.py`

**Step 1: Write failing test**

```python
# tests/test_tui_agent_wiring.py
"""Tests for agent wiring in PrismApp."""
from unittest.mock import MagicMock

def test_prism_app_creates_agent_on_first_message():
    from app.tui.app import PrismApp
    mock_backend = MagicMock()
    app = PrismApp(backend=mock_backend)
    # Agent is lazy — not created until first message
    assert app._agent is None

def test_prism_app_init_agent():
    from app.tui.app import PrismApp
    mock_backend = MagicMock()
    app = PrismApp(backend=mock_backend)
    app._init_agent()
    assert app._agent is not None
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_agent_wiring.py -v`
Expected: FAIL (no `_init_agent` method yet)

**Step 3: Add `_init_agent` to PrismApp**

Add this method to `PrismApp` in `app/tui/app.py`:

```python
def _init_agent(self) -> None:
    """Lazily initialize AgentCore with tool registry and approval callback."""
    if self._agent is not None:
        return
    from app.plugins.bootstrap import build_full_registry
    from app.agent.core import AgentCore
    from app.agent.scratchpad import Scratchpad
    from app.agent.memory import SessionMemory

    tools = build_full_registry(enable_mcp=self._enable_mcp)
    self._memory = SessionMemory()
    self._scratchpad = Scratchpad()
    self._agent = AgentCore(
        backend=self._backend,
        tools=tools,
        approval_callback=self._approval_callback,
        auto_approve=self._auto_approve,
    )
    self._agent.scratchpad = self._scratchpad
```

And update `_process_agent_message` to call `_init_agent()` at the top:

```python
async def _process_agent_message(self, message: str) -> None:
    self._init_agent()
    # ... rest of method
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_agent_wiring.py -v`
Expected: All 2 PASS

**Step 5: Commit**

```bash
git add app/tui/app.py tests/test_tui_agent_wiring.py
git commit -m "feat(tui): lazy AgentCore init with approval callback wiring"
```

---

### Task 13: Approval flow in TUI (async callback)

**Files:**
- Modify: `app/tui/app.py`
- Modify: `app/tui/widgets/cards.py`
- Test: `tests/test_tui_approval.py`

**Step 1: Write failing test**

```python
# tests/test_tui_approval.py
"""Tests for TUI approval flow."""
from unittest.mock import MagicMock

def test_approval_card_renders_args():
    from app.tui.widgets.cards import ApprovalCard
    card = ApprovalCard(
        tool_name="calculate_phase_diagram",
        tool_args={"system": "W-Rh", "temperature": "300-2000K"},
    )
    assert card.tool_name == "calculate_phase_diagram"
    assert "W-Rh" in str(card.tool_args)
```

**Step 2: Run test**

Run: `python3 -m pytest tests/test_tui_approval.py -v`
Expected: PASS (ApprovalCard already implemented in Task 5)

**Step 3: Add `_approval_callback` to PrismApp**

This is the trickiest part — the agent's approval callback is synchronous, but Textual is async. We need an `asyncio.Event` bridge.

Add to `app/tui/app.py`:

```python
import asyncio
import threading

# In PrismApp.__init__:
self._approval_event = threading.Event()
self._approval_result = False

def _approval_callback(self, tool_name: str, tool_args: dict) -> bool:
    """Called by AgentCore (from worker thread) when a tool needs approval."""
    self._approval_event.clear()
    self._approval_result = False
    # Post card to main thread
    self.call_from_thread(self._show_approval_card, tool_name, tool_args)
    # Block worker until user responds
    self._approval_event.wait()
    return self._approval_result

def _show_approval_card(self, tool_name: str, tool_args: dict) -> None:
    stream = self.query_one(StreamView)
    card = ApprovalCard(tool_name=tool_name, tool_args=tool_args)
    stream.add_card(card)
    card.focus()

def _resolve_approval(self, approved: bool) -> None:
    self._approval_result = approved
    self._approval_event.set()
```

**Step 4: Commit**

```bash
git add app/tui/app.py tests/test_tui_approval.py
git commit -m "feat(tui): approval callback bridge between worker thread and UI"
```

---

### Task 14: Error retry mechanism

**Files:**
- Modify: `app/tui/widgets/cards.py` (add ErrorRetryCard)
- Modify: `app/tui/app.py` (retry handler)
- Test: `tests/test_tui_error_retry.py`

**Step 1: Write failing test**

```python
# tests/test_tui_error_retry.py
"""Tests for error retry cards."""

def test_error_retry_card_partial():
    from app.tui.widgets.cards import ErrorRetryCard
    card = ErrorRetryCard(
        tool_name="search_optimade",
        elapsed_ms=17000,
        succeeded=["MP", "OQMD", "COD", "JARVIS"],
        failed={"AFLOW": "500 Internal Server Error", "MaterialsCloud": "404"},
        partial_result={"count": 49, "results": [{"id": "mp-1"}]},
    )
    assert card.is_partial is True
    assert len(card.failed) == 2
    assert card.partial_result["count"] == 49

def test_error_retry_card_total_failure():
    from app.tui.widgets.cards import ErrorRetryCard
    card = ErrorRetryCard(
        tool_name="query_materials_project",
        elapsed_ms=2000,
        succeeded=[],
        failed={"MP": "API key not set"},
        partial_result=None,
    )
    assert card.is_partial is False
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_error_retry.py -v`
Expected: FAIL

**Step 3: Implement ErrorRetryCard**

Add to `app/tui/widgets/cards.py`:

```python
class ErrorRetryCard(Widget):
    """Displays a failed or partially failed tool execution with retry option."""

    DEFAULT_CSS = """
    ErrorRetryCard {
        height: auto;
        margin: 0 0 1 0;
    }
    """

    def __init__(self, tool_name: str, elapsed_ms: float,
                 succeeded: list[str], failed: dict[str, str],
                 partial_result: dict | None = None, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.succeeded = succeeded
        self.failed = failed
        self.partial_result = partial_result
        self.is_partial = len(succeeded) > 0

    def _format_elapsed(self) -> str:
        if self.elapsed_ms >= 1000:
            return f"{self.elapsed_ms / 1000:.1f}s"
        return f"{self.elapsed_ms:.0f}ms"

    def render(self) -> Panel:
        border = CARD_BORDERS["error_partial"] if self.is_partial else CARD_BORDERS["error"]
        label = "PARTIAL" if self.is_partial else "FAILED"

        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" {label} ", style=f"bold {border}")
        title.append(f" {self._format_elapsed()} ", style=TEXT_DIM)

        body = Text()
        for name, err in self.failed.items():
            body.append(f"  ✗ {name}: {err[:60]}\n", style=ERROR)
        if self.succeeded:
            n = len(self.succeeded)
            count = self.partial_result.get("count", "?") if self.partial_result else 0
            body.append(f"  ✔ {n} succeeded ({count} results)\n", style=SUCCESS)
        body.append(f"\n  [r] Retry failed  [s] Skip  [Ctrl+O] Details", style=TEXT_DIM)

        return Panel(
            body,
            title=title,
            title_align="left",
            border_style=border,
            padding=(0, 1),
        )
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_error_retry.py -v`
Expected: All 2 PASS

**Step 5: Commit**

```bash
git add app/tui/widgets/cards.py tests/test_tui_error_retry.py
git commit -m "feat(tui): ErrorRetryCard with partial failure support"
```

---

### Task 15: Specialized result cards (Metrics, CALPHAD, Validation, Results Table, Plot)

**Files:**
- Modify: `app/tui/widgets/cards.py`
- Test: `tests/test_tui_specialized_cards.py`

**Step 1: Write failing test**

```python
# tests/test_tui_specialized_cards.py
"""Tests for specialized card widgets."""

def test_metrics_card():
    from app.tui.widgets.cards import MetricsCard
    card = MetricsCard(
        tool_name="train_model",
        elapsed_ms=4100,
        property_name="formation_energy",
        algorithm="random_forest",
        metrics={"mae": 0.0423, "rmse": 0.0612, "r2": 0.934, "n_train": 120, "n_test": 30},
        plot_path="parity.png",
    )
    assert card.property_name == "formation_energy"
    assert card.metrics["r2"] == 0.934

def test_calphad_card():
    from app.tui.widgets.cards import CalphadCard
    card = CalphadCard(
        tool_name="calculate_equilibrium",
        elapsed_ms=8200,
        system="W-Rh",
        conditions="T=1500K, P=101325Pa",
        phases={"BCC_A2": 0.62, "HCP_A3": 0.38},
        gibbs_energy=-45231.4,
    )
    assert "BCC_A2" in card.phases
    assert card.gibbs_energy == -45231.4

def test_validation_card():
    from app.tui.widgets.cards import ValidationCard
    card = ValidationCard(
        tool_name="validate_dataset",
        elapsed_ms=1200,
        quality_score=0.87,
        findings={
            "critical": [{"msg": "band_gap = -0.5 violates >= 0"}],
            "warning": [{"msg": "outlier z=3.4"}],
            "info": [{"msg": "density 45% completeness"}],
        },
    )
    assert card.quality_score == 0.87
    assert len(card.findings["critical"]) == 1

def test_results_table_card():
    from app.tui.widgets.cards import ResultsTableCard
    rows = [{"formula": f"W{i}Rh", "provider": "MP"} for i in range(20)]
    card = ResultsTableCard(
        tool_name="search_optimade",
        elapsed_ms=17000,
        rows=rows,
        total_count=49,
    )
    assert card.total_count == 49
    assert len(card.preview_rows) == 3  # default preview

def test_plot_card():
    from app.tui.widgets.cards import PlotCard
    card = PlotCard(
        tool_name="plot_materials_comparison",
        elapsed_ms=2300,
        description="Scatter: band_gap vs formation_energy",
        file_path="prism_scatter.png",
    )
    assert card.file_path == "prism_scatter.png"
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_tui_specialized_cards.py -v`
Expected: FAIL

**Step 3: Implement specialized cards**

Add these classes to `app/tui/widgets/cards.py`:

```python
class MetricsCard(Widget):
    """Displays ML training metrics."""

    DEFAULT_CSS = """
    MetricsCard { height: auto; margin: 0 0 1 0; }
    """

    def __init__(self, tool_name: str, elapsed_ms: float,
                 property_name: str, algorithm: str,
                 metrics: dict, plot_path: str = "", **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.property_name = property_name
        self.algorithm = algorithm
        self.metrics = metrics
        self.plot_path = plot_path

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" {self.algorithm} ", style=TEXT_DIM)

        table = Table(show_header=False, box=None, padding=(0, 1))
        table.add_column(style="bold")
        table.add_column()
        table.add_row("Property", self.property_name)
        for k, v in self.metrics.items():
            label = k.upper() if k in ("mae", "rmse", "r2") else k
            table.add_row(label, f"{v:.4f}" if isinstance(v, float) else str(v))
        if self.plot_path:
            table.add_row("Plot", self.plot_path)

        return Panel(table, title=title, title_align="left",
                     border_style=CARD_BORDERS["metrics"], padding=(0, 1))


class CalphadCard(Widget):
    """Displays CALPHAD equilibrium/phase results."""

    DEFAULT_CSS = """
    CalphadCard { height: auto; margin: 0 0 1 0; }
    """

    def __init__(self, tool_name: str, elapsed_ms: float,
                 system: str, conditions: str,
                 phases: dict, gibbs_energy: float, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.system = system
        self.conditions = conditions
        self.phases = phases
        self.gibbs_energy = gibbs_energy

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")

        body = Text()
        body.append(f"System: {self.system}  {self.conditions}\n", style="bold")
        for phase, frac in self.phases.items():
            body.append(f"  {phase}: {frac:.2f}\n")
        body.append(f"ΔG = {self.gibbs_energy:.1f} J/mol\n", style=TEXT_DIM)

        return Panel(body, title=title, title_align="left",
                     border_style=CARD_BORDERS["calphad"], padding=(0, 1))


class ValidationCard(Widget):
    """Displays validation/review findings with severity colors."""

    DEFAULT_CSS = """
    ValidationCard { height: auto; margin: 0 0 1 0; }
    """

    def __init__(self, tool_name: str, elapsed_ms: float,
                 quality_score: float, findings: dict, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.quality_score = quality_score
        self.findings = findings

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" Quality: {self.quality_score:.2f} ", style=TEXT_DIM)

        body = Text()
        colors = {"critical": ERROR, "warning": WARNING, "info": INFO}
        icons = {"critical": "🔴", "warning": "🟡", "info": "🔵"}
        for severity in ("critical", "warning", "info"):
            items = self.findings.get(severity, [])
            if items:
                body.append(f"  {icons[severity]} {len(items)} {severity.upper()}\n",
                            style=f"bold {colors[severity]}")
                for item in items[:2]:
                    body.append(f"    {item.get('msg', '')}\n", style=TEXT_DIM)
                if len(items) > 2:
                    body.append(f"    ... +{len(items) - 2} more\n", style=TEXT_DIM)

        return Panel(body, title=title, title_align="left",
                     subtitle=Text("Ctrl+O", style=TEXT_DIM),
                     subtitle_align="right",
                     border_style=CARD_BORDERS["validation_warning"],
                     padding=(0, 1))


class ResultsTableCard(Widget):
    """Displays tabular results with preview and export option."""

    DEFAULT_CSS = """
    ResultsTableCard { height: auto; margin: 0 0 1 0; }
    """

    def __init__(self, tool_name: str, elapsed_ms: float,
                 rows: list[dict], total_count: int,
                 preview_count: int = 3, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.rows = rows
        self.total_count = total_count
        self.preview_rows = rows[:preview_count]

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" {self.total_count} entries ", style=TEXT_DIM)

        if self.preview_rows:
            cols = list(self.preview_rows[0].keys())[:4]
            table = Table(box=None, padding=(0, 1))
            for c in cols:
                table.add_column(c, style=TEXT_DIM)
            for row in self.preview_rows:
                table.add_row(*(str(row.get(c, ""))[:20] for c in cols))
        else:
            table = Text("No preview available", style=TEXT_DIM)

        remaining = self.total_count - len(self.preview_rows)
        footer = Text()
        footer.append(f"+{remaining} more  ", style=TEXT_DIM)
        footer.append("[Ctrl+O] Full table  [e] Export", style=TEXT_DIM)

        return Panel(table, title=title, title_align="left",
                     subtitle=footer, subtitle_align="right",
                     border_style=CARD_BORDERS["results"], padding=(0, 1))


class PlotCard(Widget):
    """Displays a visualization output with file path."""

    DEFAULT_CSS = """
    PlotCard { height: auto; margin: 0 0 1 0; }
    """

    def __init__(self, tool_name: str, elapsed_ms: float,
                 description: str, file_path: str, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.description = description
        self.file_path = file_path

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")

        body = Text()
        body.append(f"✔ {self.description}\n", style=SUCCESS)
        body.append(f"📊 {self.file_path}\n", style=TEXT_DIM)

        return Panel(body, title=title, title_align="left",
                     subtitle=Text("Ctrl+O Preview", style=TEXT_DIM),
                     subtitle_align="right",
                     border_style=CARD_BORDERS["plot"], padding=(0, 1))
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_tui_specialized_cards.py -v`
Expected: All 5 PASS

**Step 5: Commit**

```bash
git add app/tui/widgets/cards.py tests/test_tui_specialized_cards.py
git commit -m "feat(tui): specialized cards — metrics, CALPHAD, validation, results table, plot"
```

---

### Task 16: Integration test — full TUI smoke test

**Files:**
- Test: `tests/test_tui_integration.py`

**Step 1: Write integration test**

```python
# tests/test_tui_integration.py
"""Integration smoke test for the full TUI."""
import pytest
from unittest.mock import MagicMock, patch


@pytest.mark.asyncio
async def test_full_tui_renders_all_zones():
    """PrismApp renders header, stream, status, and input."""
    from app.tui.app import PrismApp
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        app = pilot.app
        # All four zones present
        from app.tui.widgets.header import HeaderWidget
        from app.tui.widgets.stream import StreamView
        from app.tui.widgets.status_bar import StatusBar
        from app.tui.widgets.input_bar import InputBar
        assert app.query_one(HeaderWidget)
        assert app.query_one(StreamView)
        assert app.query_one(StatusBar)
        assert app.query_one(InputBar)
        # Input bar has focus
        assert isinstance(app.focused, InputBar)


@pytest.mark.asyncio
async def test_typing_and_submit_creates_input_card():
    """Submitting text creates an InputCard in the stream."""
    from app.tui.app import PrismApp
    from app.tui.widgets.cards import InputCard
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        await pilot.type("Find W-Rh alloys")
        await pilot.press("enter")
        cards = pilot.app.query(InputCard)
        assert len(cards) == 1
        assert cards[0].message == "Find W-Rh alloys"


@pytest.mark.asyncio
async def test_slash_help_command():
    """Typing /help creates an OutputCard with help text."""
    from app.tui.app import PrismApp
    from app.tui.widgets.cards import OutputCard
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        await pilot.type("/help")
        await pilot.press("enter")
        await pilot.pause()
        cards = pilot.app.query(OutputCard)
        assert len(cards) >= 1


@pytest.mark.asyncio
async def test_ctrl_l_clears_stream():
    """Ctrl+L clears the stream."""
    from app.tui.app import PrismApp
    from app.tui.widgets.stream import StreamView
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        await pilot.type("/help")
        await pilot.press("enter")
        await pilot.pause()
        await pilot.press("ctrl+l")
        await pilot.pause()
        stream = pilot.app.query_one(StreamView)
        assert len(stream.children) == 0
```

**Step 2: Run integration tests**

Run: `python3 -m pytest tests/test_tui_integration.py -v`
Expected: All 4 PASS

**Step 3: Commit**

```bash
git add tests/test_tui_integration.py
git commit -m "test(tui): integration smoke tests for full TUI"
```

---

### Task 17: Clean up prototype files and update exports

**Files:**
- Modify: `app/tui/__init__.py` (add public API)
- Modify: `app/tui/widgets/__init__.py` (export widgets)
- Delete: `test_mascot.py`, `test_mascot_screenshot.py`, `mascot_prototypes.svg` (if still present)

**Step 1: Update package exports**

```python
# app/tui/__init__.py
"""PRISM Textual TUI."""
from app.tui.app import PrismApp

__all__ = ["PrismApp"]
```

```python
# app/tui/widgets/__init__.py
"""TUI widgets."""
from app.tui.widgets.header import HeaderWidget
from app.tui.widgets.stream import StreamView
from app.tui.widgets.cards import (
    InputCard, OutputCard, ToolCard, ApprovalCard, PlanCard,
    ErrorRetryCard, MetricsCard, CalphadCard, ValidationCard,
    ResultsTableCard, PlotCard, detect_card_type,
)
from app.tui.widgets.status_bar import StatusBar
from app.tui.widgets.input_bar import InputBar

__all__ = [
    "HeaderWidget", "StreamView", "StatusBar", "InputBar",
    "InputCard", "OutputCard", "ToolCard", "ApprovalCard", "PlanCard",
    "ErrorRetryCard", "MetricsCard", "CalphadCard", "ValidationCard",
    "ResultsTableCard", "PlotCard", "detect_card_type",
]
```

**Step 2: Run full test suite**

Run: `python3 -m pytest tests/test_tui_*.py -v`
Expected: All PASS

**Step 3: Commit**

```bash
git add app/tui/__init__.py app/tui/widgets/__init__.py app/tui/screens/__init__.py
git commit -m "chore(tui): clean up exports and package structure"
```

---

## Task Dependency Graph

```
Task 1 (scaffold)
  ├── Task 2 (theme + config)
  ├── Task 3 (keymap)
  │
  ├── Task 4 (header)
  ├── Task 5 (cards) ──────────────────┐
  ├── Task 6 (stream)                  │
  ├── Task 7 (status bar)              │
  ├── Task 8 (input bar)               │
  ├── Task 9 (overlay)                 │
  │                                    │
  └── Task 10 (PrismApp) ─── depends on Tasks 2-9
        ├── Task 11 (CLI wiring)
        ├── Task 12 (agent wiring)
        ├── Task 13 (approval flow)
        └── Task 14 (error retry) ─── depends on Task 5
              └── Task 15 (specialized cards) ─── depends on Task 5
                    └── Task 16 (integration test)
                          └── Task 17 (cleanup)
```

Tasks 2-9 are independent and can be parallelized.
Tasks 10-17 are sequential.
