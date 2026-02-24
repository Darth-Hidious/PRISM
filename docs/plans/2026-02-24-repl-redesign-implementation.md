# PRISM REPL Redesign Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Redesign the PRISM REPL with a VIBGYOR block-letter banner, braille spinner, tool call cards, markdown response rendering, and a `❯` prompt — matching modern CLI agent aesthetics.

**Architecture:** Inline REPL using Rich (rendering) + prompt_toolkit (input). New `Spinner` class in a separate module. All visual changes in `repl.py`'s `_show_welcome`, `_handle_streaming_response`, `_approval_callback`, and command handlers. No new dependencies.

**Tech Stack:** Python 3.11+, Rich (Console, Text, Panel, Markdown, Live, Spinner, Table), prompt_toolkit (PromptSession, HTML formatted text), threading (for spinner).

**Design Reference:** `docs/assets/repl-concept.html` — open in browser to see the target visuals.

---

### Task 1: Create `Spinner` module with tests

**Files:**
- Create: `app/agent/spinner.py`
- Create: `tests/test_spinner.py`

**Step 1: Write the failing tests**

Create `tests/test_spinner.py`:

```python
"""Tests for the braille spinner."""

from unittest.mock import patch, MagicMock
from app.agent.spinner import Spinner, TOOL_VERBS


class TestSpinner:
    def test_default_verb(self):
        s = Spinner(console=MagicMock())
        assert s._verb == "Thinking..."

    def test_tool_verbs_mapping(self):
        assert "search_optimade" in TOOL_VERBS
        assert "predict_property" in TOOL_VERBS
        assert "calculate_phase_diagram" in TOOL_VERBS

    def test_verb_for_tool(self):
        s = Spinner(console=MagicMock())
        assert s.verb_for_tool("search_optimade") == TOOL_VERBS["search_optimade"]
        assert s.verb_for_tool("unknown_tool") == "Thinking..."

    def test_start_and_stop(self):
        console = MagicMock()
        s = Spinner(console=console)
        s.start("Testing...")
        assert s._running is True
        assert s._verb == "Testing..."
        s.stop()
        assert s._running is False

    def test_update_verb(self):
        console = MagicMock()
        s = Spinner(console=console)
        s.start("First...")
        s.update("Second...")
        assert s._verb == "Second..."
        s.stop()

    def test_stop_without_start(self):
        s = Spinner(console=MagicMock())
        s.stop()  # should not raise

    def test_braille_frames(self):
        from app.agent.spinner import BRAILLE_FRAMES
        assert len(BRAILLE_FRAMES) == 10
        assert BRAILLE_FRAMES[0] == "⠋"
```

**Step 2: Run tests to verify they fail**

Run: `python3 -m pytest tests/test_spinner.py -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'app.agent.spinner'`

**Step 3: Implement the Spinner**

Create `app/agent/spinner.py`:

```python
"""Braille dot spinner for the PRISM REPL."""

import threading
import time
from rich.console import Console
from rich.text import Text

BRAILLE_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]

TOOL_VERBS = {
    "search_optimade": "Searching OPTIMADE databases...",
    "query_materials_project": "Querying Materials Project...",
    "predict_property": "Training ML model...",
    "calculate_phase_diagram": "Computing phase diagram...",
    "calculate_equilibrium": "Computing equilibrium...",
    "calculate_gibbs_energy": "Computing Gibbs energy...",
    "literature_search": "Searching literature...",
    "patent_search": "Searching patents...",
    "run_simulation": "Running simulation...",
    "submit_hpc_job": "Submitting HPC job...",
    "validate_dataset": "Validating dataset...",
    "review_dataset": "Reviewing dataset...",
    "export_results_csv": "Exporting results...",
    "import_local_data": "Importing data...",
    "list_predictable_properties": "Analyzing properties...",
}


class Spinner:
    """Animated braille spinner with context-aware verbs."""

    def __init__(self, console: Console):
        self._console = console
        self._verb = "Thinking..."
        self._running = False
        self._thread = None
        self._frame_idx = 0

    def verb_for_tool(self, tool_name: str) -> str:
        return TOOL_VERBS.get(tool_name, "Thinking...")

    def start(self, verb: str = "Thinking..."):
        self._verb = verb
        self._running = True
        self._frame_idx = 0
        self._thread = threading.Thread(target=self._animate, daemon=True)
        self._thread.start()

    def update(self, verb: str):
        self._verb = verb

    def stop(self):
        self._running = False
        if self._thread is not None:
            self._thread.join(timeout=0.2)
            self._thread = None
        # Clear the spinner line
        self._console.print("\r\033[K", end="")

    def _animate(self):
        while self._running:
            frame = BRAILLE_FRAMES[self._frame_idx % len(BRAILLE_FRAMES)]
            text = Text()
            text.append(f" {frame} ", style="bold magenta")
            text.append(self._verb, style="dim italic")
            self._console.print(f"\r\033[K", end="")
            self._console.print(text, end="")
            self._frame_idx += 1
            time.sleep(0.08)
```

**Step 4: Run tests to verify they pass**

Run: `python3 -m pytest tests/test_spinner.py -v`
Expected: 7 passed

**Step 5: Commit**

```bash
git add app/agent/spinner.py tests/test_spinner.py
git commit -m "feat: add braille spinner module for REPL"
```

---

### Task 2: Implement VIBGYOR banner in `_show_welcome`

**Files:**
- Modify: `app/agent/repl.py:206-251` (the `_show_welcome` method)

**Step 1: Write the failing test**

Add to `tests/test_repl.py`:

```python
@patch("app.agent.repl.Console")
def test_welcome_banner_has_prism_art(self, mock_console_cls):
    repl = _make_repl()
    output = StringIO()
    repl.console = Console(file=output, highlight=False, force_terminal=True)
    repl._show_welcome()
    text = output.getvalue()
    # Block letters should spell PRISM across multiple lines
    assert "██" in text
    assert "PRISM" not in text or "██" in text  # block art, not plain text
```

Add `from io import StringIO` and `from rich.console import Console` to imports at top of `tests/test_repl.py` if not already present.

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_repl.py::TestAgentREPL::test_welcome_banner_has_prism_art -v`
Expected: FAIL (current welcome has no `██` block characters)

**Step 3: Rewrite `_show_welcome` in `app/agent/repl.py`**

Replace the `_show_welcome` method (lines 206-251) with:

```python
def _show_welcome(self):
    from app import __version__
    caps = self._detect_capabilities()

    # VIBGYOR letter colors
    colors = {
        "P": "#ff0000", "R": "#ff7700", "I": "#ffdd00",
        "S": "#00cc44", "M": "#0066ff",
    }
    # 5-line block-letter art  (each inner list = one letter's column block)
    art_lines = [
        " ██████  ██████  ██  ███████ ███    ███",
        " ██   ██ ██   ██ ██  ██      ████  ████",
        " ██████  ██████  ██  ███████ ██ ████ ██",
        " ██      ██   ██ ██       ██ ██  ██  ██",
        " ██      ██   ██ ██  ███████ ██      ██",
    ]
    # Character ranges for each letter in the art
    letter_spans = [
        ("P", 0, 8), ("R", 8, 16), ("I", 16, 20),
        ("S", 20, 28), ("M", 28, 39),
    ]

    self.console.print()
    for line in art_lines:
        text = Text()
        for letter, start, end in letter_spans:
            segment = line[start:end] if end <= len(line) else line[start:]
            text.append(segment, style=f"bold {colors[letter]}")
        self.console.print(text)

    # Prism triangle + info
    self.console.print()
    triangle = Text()
    triangle.append("           ▲\n", style="dim")
    triangle.append("          ╱ ╲\n", style="dim")
    triangle.append("         ╱   ╲\n", style="dim")
    triangle.append("        ▔▔▔▔▔▔▔\n", style="dim")

    # Rainbow bar
    rainbow_colors = [
        "#ff0000", "#ff3300", "#ff6600", "#ff9900", "#ffcc00",
        "#ffff00", "#ccff00", "#66ff00", "#00cc44", "#00aa88",
        "#0088cc", "#0055ff", "#2200ff", "#4400cc", "#6600aa",
    ]
    bar = Text("     ")
    for c in rainbow_colors:
        bar.append("━", style=c)
    self.console.print(triangle, end="")
    self.console.print(bar)
    self.console.print()

    # Provider
    provider = None
    if os.getenv("MARC27_TOKEN"):
        provider = "MARC27"
    elif os.getenv("ANTHROPIC_API_KEY"):
        provider = "Claude"
    elif os.getenv("OPENAI_API_KEY"):
        provider = "GPT"
    elif os.getenv("OPENROUTER_API_KEY"):
        provider = "OpenRouter"

    info = Text()
    info.append(f"  v{__version__}", style="dim")
    if provider:
        info.append("  ·  ", style="dim")
        info.append(provider, style="bold magenta")
    self.console.print(info)

    # Capabilities line
    parts = []
    tool_count = len(self.agent.tools.list_tools())
    parts.append(f"{tool_count} tools")
    try:
        from app.skills.registry import load_builtin_skills
        skill_count = len(load_builtin_skills().list_skills())
        parts.append(f"{skill_count} skills")
    except Exception:
        pass
    for name, ok in caps.items():
        if ok:
            parts.append(f"[green]{name}[/green]")
        else:
            parts.append(f"[dim]{name}[/dim]")
    if self._auto_approve:
        parts.append("[yellow]auto-approve[/yellow]")
    self.console.print("[dim]  " + " · ".join(parts) + "[/dim]")
    self.console.print()
```

**Step 4: Run tests to verify they pass**

Run: `python3 -m pytest tests/test_repl.py -v`
Expected: All pass

**Step 5: Commit**

```bash
git add app/agent/repl.py tests/test_repl.py
git commit -m "feat: VIBGYOR block-letter banner with prism triangle"
```

---

### Task 3: Change prompt character to `❯` and integrate spinner

**Files:**
- Modify: `app/agent/repl.py:1-5` (add import)
- Modify: `app/agent/repl.py:95-117` (the `run` method — prompt character)
- Modify: `app/agent/repl.py:121-186` (the `_handle_streaming_response` method — spinner + tool cards)

**Step 1: Write the failing test**

Add to `tests/test_repl.py`:

```python
def test_spinner_imported(self):
    from app.agent.spinner import Spinner
    assert Spinner is not None
```

**Step 2: Run test to verify it passes (already built in Task 1)**

Run: `python3 -m pytest tests/test_repl.py::TestAgentREPL::test_spinner_imported -v`
Expected: PASS (Spinner was built in Task 1)

**Step 3: Modify `app/agent/repl.py`**

At the top of `repl.py`, add the import after the existing imports:

```python
from app.agent.spinner import Spinner
```

Change the prompt in the `run()` method from:

```python
user_input = self._session.prompt(
    HTML('<b>> </b>'),
).strip()
```

to:

```python
user_input = self._session.prompt(
    HTML('<ansimagenta><b>❯ </b></ansimagenta>'),
).strip()
```

Rewrite `_handle_streaming_response` to use the spinner and tool call cards:

```python
def _handle_streaming_response(self, user_input: str):
    accumulated_text = ""
    plan_buffer = ""
    in_plan = False
    tool_start_time = None
    current_tool_name = None
    spinner = Spinner(console=self.console)

    for event in self.agent.process_stream(user_input):
        if isinstance(event, TextDelta):
            accumulated_text += event.text
            if "<plan>" in accumulated_text and not in_plan:
                in_plan = True
                plan_buffer = accumulated_text.split("<plan>", 1)[1]
                pre = accumulated_text.split("<plan>", 1)[0].strip()
                if pre:
                    self.console.print(Markdown(pre))
                accumulated_text = ""
            elif in_plan:
                if "</plan>" in event.text:
                    plan_buffer += event.text.split("</plan>")[0]
                    in_plan = False
                    self.console.print()
                    self.console.print(Panel(
                        Markdown(plan_buffer.strip()),
                        title="[bold]Plan[/bold]",
                        border_style="dim",
                        padding=(1, 2),
                    ))
                    if self.scratchpad:
                        self.scratchpad.log("plan", summary="Plan proposed", data={"plan": plan_buffer.strip()})
                    if not Confirm.ask("  Execute?", default=True):
                        self.console.print("[dim]Cancelled.[/dim]")
                        return
                    remainder = event.text.split("</plan>", 1)[1] if "</plan>" in event.text else ""
                    accumulated_text = remainder
                else:
                    plan_buffer += event.text
                continue
            # Don't write raw text — we'll render as Markdown at breaks

        elif isinstance(event, ToolCallStart):
            # Flush accumulated text as Markdown before tool card
            if accumulated_text.strip():
                self.console.print()
                self.console.print(Markdown(accumulated_text.strip()))
                accumulated_text = ""
            tool_start_time = time.monotonic()
            current_tool_name = event.tool_name
            verb = spinner.verb_for_tool(event.tool_name)
            spinner.start(verb)

        elif isinstance(event, ToolApprovalRequest):
            pass

        elif isinstance(event, ToolCallResult):
            spinner.stop()
            elapsed_str = ""
            if tool_start_time:
                ms = (time.monotonic() - tool_start_time) * 1000
                elapsed_str = f"{ms:.0f}ms" if ms < 2000 else f"{ms / 1000:.1f}s"
                tool_start_time = None
            summary = event.summary if hasattr(event, "summary") else "done"
            # Render tool card
            self._render_tool_card(
                event.tool_name, summary, elapsed_str
            )
            current_tool_name = None

        elif isinstance(event, TurnComplete):
            spinner.stop()
            tool_start_time = None

    # Flush any remaining text as Markdown
    if accumulated_text.strip():
        self.console.print()
        self.console.print(Markdown(accumulated_text.strip()))
        self.console.print()
```

Add the `_render_tool_card` helper method to the class:

```python
def _render_tool_card(self, tool_name: str, summary: str, elapsed: str):
    """Render a bordered tool call result card."""
    header = Text()
    header.append(f" {tool_name} ", style="bold magenta")
    if elapsed:
        header.append(f" {elapsed}", style="dim")

    body = Text()
    body.append(" ✓ ", style="bold green")
    body.append(summary, style="dim")

    self.console.print(Panel(
        body,
        title=header,
        title_align="left",
        border_style="dim",
        padding=(0, 1),
    ))
```

**Step 4: Run all tests**

Run: `python3 -m pytest tests/test_repl.py tests/test_spinner.py -v`
Expected: All pass

**Step 5: Commit**

```bash
git add app/agent/repl.py
git commit -m "feat: ❯ prompt, braille spinner, tool call cards, Markdown responses"
```

---

### Task 4: Redesign approval callback with `[y/n/a]` UX

**Files:**
- Modify: `app/agent/repl.py:80-83` (the `_approval_callback` method)

**Step 1: Write the failing test**

Add to `tests/test_repl.py`:

```python
@patch("builtins.input", return_value="a")
def test_approval_always_sets_auto(self, mock_input):
    repl = _make_repl()
    repl._auto_approve_tools = set()
    # Simulate calling approval for a tool
    result = repl._approval_callback("predict_property", {"target": "band_gap"})
    assert result is True
    assert "predict_property" in repl._auto_approve_tools
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_repl.py::TestAgentREPL::test_approval_always_sets_auto -v`
Expected: FAIL (`_auto_approve_tools` doesn't exist yet)

**Step 3: Implement the new approval callback**

Add `self._auto_approve_tools: set = set()` in `__init__` after `self._auto_approve = auto_approve`.

Replace the `_approval_callback` method:

```python
def _approval_callback(self, tool_name: str, tool_args: dict) -> bool:
    if tool_name in self._auto_approve_tools:
        return True
    args_summary = ", ".join(f"{k}={v!r}" for k, v in list(tool_args.items())[:3])
    header = Text()
    header.append(f" {tool_name} ", style="bold magenta")
    header.append(" approval ", style="bold #d29922")
    body = Text()
    body.append(f" {args_summary}", style="dim")
    self.console.print(Panel(
        body,
        title=header,
        title_align="left",
        border_style="#d29922",
        padding=(0, 1),
    ))
    self.console.print("  [#d29922]?[/#d29922] Allow?  [bold]y[/bold] yes  [bold]n[/bold] no  [bold]a[/bold] always")
    try:
        answer = input("    ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        return False
    if answer == "a":
        self._auto_approve_tools.add(tool_name)
        return True
    return answer in ("y", "yes", "")
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_repl.py -v`
Expected: All pass

**Step 5: Commit**

```bash
git add app/agent/repl.py tests/test_repl.py
git commit -m "feat: approval cards with [y/n/a] always option"
```

---

### Task 5: Update `/tools` to show approval badge with `★`

**Files:**
- Modify: `app/agent/repl.py:344-351` (the `_handle_tools` method)

**Step 1: Write the failing test**

Add to `tests/test_repl_skills.py`:

```python
def test_tools_shows_approval_star(self):
    repl = _make_repl()
    output = StringIO()
    repl.console = Console(file=output, highlight=False, force_terminal=True)
    repl._handle_tools()
    text = output.getvalue()
    assert "★" in text
    assert "approval" in text.lower() or "★" in text
```

Add `from rich.console import Console` to `tests/test_repl_skills.py` imports.

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_repl_skills.py::TestReplSkillCommands::test_tools_shows_approval_star -v`
Expected: FAIL (current `/tools` uses `*` not `★`)

**Step 3: Update `_handle_tools`**

Replace the method:

```python
def _handle_tools(self):
    self.console.print()
    tools = self.agent.tools.list_tools()
    for tool in tools:
        name_style = "bold #d29922" if tool.requires_approval else "bold"
        flag = " [#d29922]★[/#d29922]" if tool.requires_approval else ""
        self.console.print(f"  [{name_style}]{tool.name:<28}[/{name_style}] [dim]{tool.description[:55]}[/dim]{flag}")
    self.console.print(f"\n  [dim]{len(tools)} tools[/dim]  [#d29922]★[/#d29922] [dim]= requires approval[/dim]")
    self.console.print()
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_repl_skills.py -v`
Expected: All pass

**Step 5: Commit**

```bash
git add app/agent/repl.py tests/test_repl_skills.py
git commit -m "feat: /tools shows amber ★ for approval-required tools"
```

---

### Task 6: Full test suite + final commit

**Files:**
- All previously modified files

**Step 1: Run the full test suite**

Run: `python3 -m pytest tests/ -v --ignore=tests/test_mcp_roundtrip.py --ignore=tests/test_mcp_server.py`
Expected: 525+ tests pass (519 existing + ~7 new spinner + ~2 new repl tests)

**Step 2: Fix any failures**

If any test failures occur, fix them. Common issues:
- Tests that check for exact output strings may need updating
- Tests that mock `Console` may need adjustments for new `Panel` calls
- The `_make_repl` helper in test files may need to also mock `Spinner`

**Step 3: Final commit**

```bash
git add -A
git commit -m "feat: PRISM REPL redesign — VIBGYOR banner, spinner, tool cards, Markdown

- VIBGYOR block-letter PRISM banner with prism triangle and rainbow bar
- Braille dot spinner with context-aware verbs during tool calls
- Bordered tool call cards with timing and result summaries
- Amber approval cards with [y/n/a] (always) option
- AI responses rendered as Markdown (syntax highlighting, bold, lists)
- ❯ prompt character in purple
- /tools shows ★ for approval-required tools

Co-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>"
```

---

## Summary

| Task | What | New Tests |
|------|------|-----------|
| 1 | `Spinner` module | 7 tests |
| 2 | VIBGYOR banner | 1 test |
| 3 | `❯` prompt + spinner integration + Markdown | 1 test |
| 4 | Approval `[y/n/a]` UX | 1 test |
| 5 | `/tools` with `★` badge | 1 test |
| 6 | Full suite run + polish | — |

Total: 6 tasks, ~11 new tests, 2 files created, 3 files modified.
