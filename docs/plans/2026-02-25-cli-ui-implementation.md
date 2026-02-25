# CLI UI Polish Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Polish the PRISM REPL and `prism run` terminal experience — live streaming, truncation, unified rendering, cost display, crystal fix, install.sh polish.

**Architecture:** Surgical patches to 6 existing files. No new modules. Card renderers stay as pure functions. Rich Live handles streaming text. All 861 existing tests must keep passing.

**Tech Stack:** Python 3.11+, Rich (Live, Console, Panel, Markdown), prompt_toolkit

---

### Task 1: Crystal Mascot Alignment Fix

**Files:**
- Modify: `app/cli/tui/theme.py:54-59`
- Test: `tests/test_repl_cards.py`

**Context:** The crystal mascot's top/bottom rows (3 glyphs) use 3-space indent but need 4-space to center under the 5-glyph middle rows. Each glyph occupies 2 character widths (glyph + space).

**Step 1: Write the failing test**

Add to `tests/test_repl_cards.py`:

```python
def test_crystal_mascot_alignment():
    """Top/bottom rows should be centered under middle rows."""
    from app.cli.tui.theme import MASCOT
    # Middle rows start at position 2 (2 spaces before first glyph)
    # Top/bottom rows have 3 glyphs centered under 5 = 1 extra glyph padding each side
    # Each glyph+space = 2 chars, so top/bottom need 2+2 = 4 leading spaces
    for i in [0, 3]:  # top and bottom rows
        leading_spaces = len(MASCOT[i]) - len(MASCOT[i].lstrip())
        assert leading_spaces == 4, f"Row {i} has {leading_spaces} leading spaces, expected 4"
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_repl_cards.py::test_crystal_mascot_alignment -v`
Expected: FAIL — rows 0 and 3 have 3 leading spaces, not 4.

**Step 3: Fix the indent in theme.py**

In `app/cli/tui/theme.py`, change lines 55 and 58. The MASCOT array currently has:
```python
MASCOT = [
    "   \u2b21 \u2b21 \u2b21",          # 3 spaces
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "   \u2b21 \u2b21 \u2b21",          # 3 spaces
]
```

Change to:
```python
MASCOT = [
    "    \u2b21 \u2b21 \u2b21",         # 4 spaces — centered under middle rows
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "    \u2b21 \u2b21 \u2b21",         # 4 spaces — centered under middle rows
]
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_repl_cards.py -v`
Expected: ALL PASS (new test + existing `test_mascot_lines` still passes)

**Step 5: Run full suite to confirm no regressions**

Run: `python3 -m pytest tests/ -x -q`
Expected: 861 passed

**Step 6: Commit**

```bash
git add app/cli/tui/theme.py tests/test_repl_cards.py
git commit -m "fix(tui): center crystal mascot top/bottom rows"
```

---

### Task 2: Add `TRUNCATION_CHARS` Constant + `render_cost_line` Helper

**Files:**
- Modify: `app/cli/tui/theme.py:92-94`
- Modify: `app/cli/tui/cards.py` (add new functions at bottom)
- Test: `tests/test_repl_cards.py`

**Context:** We need two new things: (1) a 50K character truncation threshold constant, (2) a cost line renderer. Both are used by later tasks. Build them first with tests.

**Step 1: Write the failing tests**

Add to `tests/test_repl_cards.py`:

```python
def test_truncation_chars_constant():
    from app.cli.tui.theme import TRUNCATION_CHARS
    assert TRUNCATION_CHARS == 50_000


def test_format_tokens_small():
    from app.cli.tui.cards import format_tokens
    assert format_tokens(500) == "500"


def test_format_tokens_large():
    from app.cli.tui.cards import format_tokens
    assert format_tokens(2100) == "2.1k"


def test_format_tokens_exact_k():
    from app.cli.tui.cards import format_tokens
    assert format_tokens(1000) == "1.0k"


def test_render_cost_line():
    from app.cli.tui.cards import render_cost_line
    from app.agent.events import UsageInfo
    from rich.console import Console
    from io import StringIO
    output = StringIO()
    console = Console(file=output, highlight=False, force_terminal=True, width=120)
    usage = UsageInfo(input_tokens=2100, output_tokens=340)
    render_cost_line(console, usage, turn_cost=0.0008, session_cost=0.0142)
    text = output.getvalue()
    assert "2.1k in" in text
    assert "340 out" in text
    assert "$0.0008" in text
    assert "total: $0.0142" in text


def test_render_cost_line_no_cost():
    from app.cli.tui.cards import render_cost_line
    from app.agent.events import UsageInfo
    from rich.console import Console
    from io import StringIO
    output = StringIO()
    console = Console(file=output, highlight=False, force_terminal=True, width=120)
    usage = UsageInfo(input_tokens=500, output_tokens=200)
    render_cost_line(console, usage, turn_cost=None, session_cost=0.0)
    text = output.getvalue()
    assert "500 in" in text
    assert "200 out" in text
    # No dollar signs when cost is None
    assert "$" not in text
```

**Step 2: Run tests to verify they fail**

Run: `python3 -m pytest tests/test_repl_cards.py::test_truncation_chars_constant tests/test_repl_cards.py::test_format_tokens_small tests/test_repl_cards.py::test_render_cost_line -v`
Expected: FAIL — `TRUNCATION_CHARS` not defined, `format_tokens` not defined, `render_cost_line` not defined.

**Step 3: Add the constant to theme.py**

In `app/cli/tui/theme.py`, after the existing `TRUNCATION_LINES = 6` line (line 94), add:

```python
TRUNCATION_CHARS = 50_000
```

**Step 4: Add `format_tokens` and `render_cost_line` to cards.py**

At the bottom of `app/cli/tui/cards.py`, after `render_approval_card`, add:

```python
def format_tokens(n: int) -> str:
    """Format token count: 500 -> '500', 2100 -> '2.1k'."""
    if n >= 1000:
        return f"{n / 1000:.1f}k"
    return str(n)


def render_cost_line(console: Console, usage, turn_cost: float | None = None,
                     session_cost: float = 0.0):
    """Print a dim cost/token summary line after a turn."""
    parts = [
        f"{format_tokens(usage.input_tokens)} in",
        f"{format_tokens(usage.output_tokens)} out",
    ]
    if turn_cost is not None:
        parts.append(f"${turn_cost:.4f}")
        parts.append(f"total: ${session_cost:.4f}")
    line = " \u00b7 ".join(parts)
    console.print(f" [dim]\u2500 {line} \u2500[/dim]")
```

**Step 5: Run tests**

Run: `python3 -m pytest tests/test_repl_cards.py -v`
Expected: ALL PASS

**Step 6: Run full suite**

Run: `python3 -m pytest tests/ -x -q`
Expected: All passing (861+)

**Step 7: Commit**

```bash
git add app/cli/tui/theme.py app/cli/tui/cards.py tests/test_repl_cards.py
git commit -m "feat(tui): add TRUNCATION_CHARS constant and render_cost_line helper"
```

---

### Task 3: Character-Based Truncation in Tool Results

**Files:**
- Modify: `app/cli/tui/cards.py:128-145` (the `render_tool_result` function)
- Test: `tests/test_repl_cards.py`

**Context:** When a tool result exceeds `TRUNCATION_CHARS` (50K chars when serialized as JSON), we persist the full result to `~/.prism/cache/results/` and show a truncation notice. This ties into the existing `ResultStore` pattern in `app/agent/core.py` (line 86) which already stores large results in memory — we add disk persistence for the display layer.

**Step 1: Write the failing tests**

Add to `tests/test_repl_cards.py`:

```python
def test_render_tool_result_truncation_notice(tmp_path, monkeypatch):
    """Large tool results should show a truncation notice."""
    from app.cli.tui.cards import render_tool_result
    from rich.console import Console
    from io import StringIO
    import json

    # Monkey-patch the cache dir so we don't pollute real ~/.prism
    monkeypatch.setenv("PRISM_CACHE_DIR", str(tmp_path))

    output = StringIO()
    console = Console(file=output, highlight=False, force_terminal=True, width=120)

    # Create a result that exceeds 50K chars when serialized
    big_result = {"results": [{"id": f"mp-{i}", "formula": "Si"} for i in range(2000)], "count": 2000}
    assert len(json.dumps(big_result)) > 50_000

    render_tool_result(console, "search_materials", "2000 results", 1500.0, big_result)
    text = output.getvalue()
    assert "chars truncated" in text or "stored as" in text


def test_render_tool_result_small_no_truncation():
    """Small tool results should NOT show truncation notice."""
    from app.cli.tui.cards import render_tool_result
    from rich.console import Console
    from io import StringIO

    output = StringIO()
    console = Console(file=output, highlight=False, force_terminal=True, width=120)
    small_result = {"results": [{"id": "mp-1", "formula": "Si"}] * 5, "count": 5}
    render_tool_result(console, "search_materials", "5 results", 500.0, small_result)
    text = output.getvalue()
    assert "chars truncated" not in text
```

**Step 2: Run tests to verify they fail**

Run: `python3 -m pytest tests/test_repl_cards.py::test_render_tool_result_truncation_notice tests/test_repl_cards.py::test_render_tool_result_small_no_truncation -v`
Expected: FAIL — no truncation logic in `render_tool_result` yet.

**Step 3: Implement character-based truncation**

In `app/cli/tui/cards.py`, add the import at the top (after existing imports):

```python
import json
import hashlib
import os
```

Then modify `render_tool_result` (currently lines 128-145) to add truncation check at the end:

```python
def render_tool_result(console: Console, tool_name: str, summary: str,
                       elapsed_ms: float, result: dict):
    """Dispatch to the right card renderer based on result shape."""
    result_type = detect_result_type(result)
    if result_type == "error":
        render_error_card(console, tool_name, elapsed_ms, result)
    elif result_type == "metrics":
        render_metrics_card(console, tool_name, elapsed_ms, result)
    elif result_type == "calphad":
        render_calphad_card(console, tool_name, elapsed_ms, result)
    elif result_type == "validation":
        render_validation_card(console, tool_name, elapsed_ms, result)
    elif result_type == "results":
        render_results_card(console, tool_name, summary, elapsed_ms, result)
    elif result_type == "plot":
        render_plot_card(console, tool_name, elapsed_ms, result)
    else:
        render_success_card(console, tool_name, summary, elapsed_ms)

    # Character-based truncation notice for large results
    _check_large_result(console, result)


def _check_large_result(console: Console, result: dict):
    """If result exceeds TRUNCATION_CHARS, persist to disk and show notice."""
    from app.cli.tui.theme import TRUNCATION_CHARS
    try:
        serialized = json.dumps(result, default=str)
    except (TypeError, ValueError):
        return
    if len(serialized) <= TRUNCATION_CHARS:
        return
    # Persist to cache
    result_id = hashlib.md5(serialized[:1000].encode()).hexdigest()[:8]
    cache_dir = os.path.join(
        os.environ.get("PRISM_CACHE_DIR", os.path.expanduser("~/.prism/cache")),
        "results",
    )
    os.makedirs(cache_dir, exist_ok=True)
    path = os.path.join(cache_dir, f"{result_id}.json")
    try:
        with open(path, "w") as f:
            f.write(serialized)
    except OSError:
        pass
    console.print(
        f" [dim]\u2500 {len(serialized):,} chars truncated \u00b7 "
        f"stored as {result_id} \u00b7 "
        f"use peek_result(\"{result_id}\") \u2500[/dim]"
    )
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_repl_cards.py -v`
Expected: ALL PASS

**Step 5: Run full suite**

Run: `python3 -m pytest tests/ -x -q`
Expected: All passing

**Step 6: Commit**

```bash
git add app/cli/tui/cards.py tests/test_repl_cards.py
git commit -m "feat(tui): character-based truncation for large tool results (50K threshold)"
```

---

### Task 4: Live Text Streaming in REPL

**Files:**
- Modify: `app/cli/tui/stream.py` (full rewrite of `handle_streaming_response`)
- Test: `tests/test_repl_cards.py`

**Context:** Currently `stream.py` accumulates all `TextDelta` events into a string, then renders one output card at the end. We need to use `Rich Live` to display tokens as they arrive. When a tool call starts or the turn ends, we "freeze" the text via `console.print()`.

The key insight: `Rich Live` updates a single renderable in-place (our streaming area). `console.print()` adds permanent output above it (our "Static" equivalent). When we stop the Live context, the content stays visible.

**Step 1: Write the failing test**

Add to `tests/test_repl_cards.py`:

```python
def test_streaming_response_renders_live_text():
    """handle_streaming_response should use Live for streaming, not accumulate."""
    from unittest.mock import MagicMock, patch
    from app.cli.tui.stream import handle_streaming_response
    from app.agent.events import TextDelta, TurnComplete, UsageInfo
    from rich.console import Console
    from io import StringIO

    output = StringIO()
    console = Console(file=output, highlight=False, force_terminal=True, width=120)
    session = MagicMock()

    agent = MagicMock()
    agent.process_stream.return_value = iter([
        TextDelta(text="Hello "),
        TextDelta(text="world!"),
        TurnComplete(
            text="Hello world!",
            usage=UsageInfo(input_tokens=100, output_tokens=20),
            total_usage=UsageInfo(input_tokens=100, output_tokens=20),
            estimated_cost=0.0001,
        ),
    ])

    handle_streaming_response(console, agent, "test input", session, session_cost=0.0)
    text = output.getvalue()
    # The streamed text should appear in the output
    assert "Hello" in text
    assert "world" in text
    # Cost line should appear (from TurnComplete)
    assert "$0.0001" in text or "100 in" in text
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_repl_cards.py::test_streaming_response_renders_live_text -v`
Expected: FAIL — current signature doesn't accept `session_cost`, and no cost line rendered.

**Step 3: Rewrite stream.py**

Replace the entire content of `app/cli/tui/stream.py`:

```python
"""Streaming event handler — bridges AgentCore events to card renderers."""

import time
from prompt_toolkit import PromptSession
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown
from rich.text import Text

from app.agent.events import (
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest,
)
from app.agent.scratchpad import Scratchpad
from app.cli.tui.cards import (
    render_input_card, render_output_card,
    render_plan_card, render_tool_result, render_cost_line,
)
from app.cli.tui.prompt import ask_plan_confirmation
from app.cli.tui.spinner import Spinner


def _flush_live(live: Live, console: Console, text: str):
    """Freeze streamed text: stop Live, print permanently, clear buffer."""
    live.update("")
    if text.strip():
        console.print(Markdown(text))


def handle_streaming_response(
    console: Console,
    agent,
    user_input: str,
    session: PromptSession,
    scratchpad: Scratchpad | None = None,
    session_cost: float = 0.0,
) -> float:
    """Process an agent stream, rendering events as Rich cards.

    Returns the updated session_cost (accumulated).
    """
    render_input_card(console, user_input)

    accumulated_text = ""
    plan_buffer = ""
    in_plan = False
    tool_start_time = None
    spinner = Spinner(console=console)

    with Live("", console=console, refresh_per_second=15,
              vertical_overflow="visible") as live:

        for event in agent.process_stream(user_input):
            if isinstance(event, TextDelta):
                accumulated_text += event.text

                # ── Plan tag detection ──
                if "<plan>" in accumulated_text and not in_plan:
                    in_plan = True
                    plan_buffer = accumulated_text.split("<plan>", 1)[1]
                    pre = accumulated_text.split("<plan>", 1)[0].strip()
                    if pre:
                        _flush_live(live, console, pre)
                    accumulated_text = ""
                    continue
                elif in_plan:
                    if "</plan>" in event.text:
                        plan_buffer += event.text.split("</plan>")[0]
                        in_plan = False
                        live.update("")
                        render_plan_card(console, plan_buffer.strip())
                        if scratchpad:
                            scratchpad.log(
                                "plan",
                                summary="Plan proposed",
                                data={"plan": plan_buffer.strip()},
                            )
                        if not ask_plan_confirmation(session):
                            console.print("[dim]Cancelled.[/dim]")
                            return session_cost
                        remainder = (
                            event.text.split("</plan>", 1)[1]
                            if "</plan>" in event.text
                            else ""
                        )
                        accumulated_text = remainder
                    else:
                        plan_buffer += event.text
                    continue

                # ── Live-update streaming text ──
                if not in_plan:
                    live.update(Markdown(accumulated_text))

            elif isinstance(event, ToolCallStart):
                _flush_live(live, console, accumulated_text)
                accumulated_text = ""
                tool_start_time = time.monotonic()
                verb = spinner.verb_for_tool(event.tool_name)
                spinner.start(verb)

            elif isinstance(event, ToolApprovalRequest):
                pass  # Handled by approval_callback

            elif isinstance(event, ToolCallResult):
                spinner.stop()
                elapsed_ms = 0.0
                if tool_start_time:
                    elapsed_ms = (time.monotonic() - tool_start_time) * 1000
                    tool_start_time = None
                result = event.result if isinstance(event.result, dict) else {}
                render_tool_result(
                    console, event.tool_name, event.summary, elapsed_ms, result,
                )

            elif isinstance(event, TurnComplete):
                spinner.stop()
                _flush_live(live, console, accumulated_text)
                accumulated_text = ""
                tool_start_time = None
                # Cost line
                if event.usage:
                    turn_cost = event.estimated_cost
                    if turn_cost is not None:
                        session_cost += turn_cost
                    render_cost_line(console, event.usage, turn_cost, session_cost)

    # Flush any remaining text (shouldn't happen normally, but safety net)
    if accumulated_text.strip():
        console.print(Markdown(accumulated_text))

    return session_cost
```

**Step 4: Run test**

Run: `python3 -m pytest tests/test_repl_cards.py::test_streaming_response_renders_live_text -v`
Expected: PASS

**Step 5: Run full suite to check for regressions**

Run: `python3 -m pytest tests/ -x -q`
Expected: All passing. Watch for existing tests in `test_repl.py` — they mock `handle_streaming_response` indirectly (via `agent.process_stream`), so the signature change (`session_cost` parameter with default 0.0) should be backward compatible.

**Step 6: Commit**

```bash
git add app/cli/tui/stream.py tests/test_repl_cards.py
git commit -m "feat(tui): live text streaming via Rich Live"
```

---

### Task 5: Wire Session Cost Tracking into AgentREPL

**Files:**
- Modify: `app/cli/tui/app.py:26-121`
- Test: `tests/test_repl.py`

**Context:** `handle_streaming_response` now returns the updated `session_cost`. `AgentREPL` needs to track it across turns and pass it in.

**Step 1: Write the failing test**

Add to `tests/test_repl.py`:

```python
def test_repl_has_session_cost():
    repl = _make_repl()
    assert hasattr(repl, "session_cost")
    assert repl.session_cost == 0.0
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_repl.py::test_repl_has_session_cost -v`
Expected: FAIL — `session_cost` attribute doesn't exist.

**Step 3: Add session_cost to AgentREPL**

In `app/cli/tui/app.py`, add `self.session_cost = 0.0` in `__init__` (after line 42, after `self._auto_approve_tools`):

```python
        self._auto_approve_tools: set = set()
        self.session_cost = 0.0
```

Then in the `run()` method, change the `handle_streaming_response` call (around line 113-116) to capture the returned cost:

```python
            try:
                self.session_cost = handle_streaming_response(
                    self.console, self.agent, user_input,
                    self.session, self.scratchpad,
                    session_cost=self.session_cost,
                )
            except KeyboardInterrupt:
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_repl.py -v`
Expected: ALL PASS

**Step 5: Run full suite**

Run: `python3 -m pytest tests/ -x -q`
Expected: All passing

**Step 6: Commit**

```bash
git add app/cli/tui/app.py tests/test_repl.py
git commit -m "feat(tui): track session cost across REPL turns"
```

---

### Task 6: Unify `prism run` Rendering

**Files:**
- Modify: `app/commands/run.py`
- Test: `tests/test_autonomous.py`

**Context:** `run.py` currently has its own inline rendering (yellow/green panels, its own Live context, `_flush_text` helper). Replace with card renderers + Spinner + cost line from the TUI modules. `prism run` doesn't have a PromptSession or plan detection — it's a simpler event loop.

**Step 1: Write the failing test**

Add to `tests/test_autonomous.py`:

```python
def test_run_uses_card_renderers():
    """prism run should import from app.cli.tui, not use inline panels."""
    import inspect
    from app.commands.run import run_goal
    source = inspect.getsource(run_goal)
    # Should NOT have inline yellow/green panels
    assert "border_style=\"yellow\"" not in source
    assert "border_style=\"green\"" not in source
    # Should import card renderers
    assert "render_tool_result" in source or "app.cli.tui" in source
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_autonomous.py::test_run_uses_card_renderers -v`
Expected: FAIL — current run.py has `border_style="yellow"` and `border_style="green"`.

**Step 3: Rewrite run.py event loop**

Replace the full content of `app/commands/run.py`:

```python
"""Run CLI command: autonomous agent mode."""
import time
import click
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown
from rich.panel import Panel


@click.command("run")
@click.argument("goal")
@click.option("--agent", default=None, help="Use a named agent config from the registry")
@click.option("--provider", default=None, help="LLM provider (anthropic/openai/openrouter)")
@click.option("--model", default=None, help="Model name override")
@click.option("--confirm", is_flag=True, help="Require confirmation for expensive tools")
@click.option("--dangerously-accept-all", "accept_all", is_flag=True, help="Auto-approve all tool calls")
@click.pass_context
def run_goal(ctx, goal, agent, provider, model, confirm, accept_all):
    """Run PRISM agent autonomously on a research goal."""
    from app.agent.events import TextDelta, ToolCallStart, ToolCallResult, TurnComplete
    from app.agent.factory import create_backend
    from app.agent.autonomous import run_autonomous_stream
    from app.plugins.bootstrap import build_full_registry
    from app.cli.tui.cards import render_tool_result, render_cost_line
    from app.cli.tui.spinner import Spinner

    no_mcp = ctx.obj.get("no_mcp", False) if ctx.obj else False
    run_console = Console(highlight=False)

    # Build registries
    tool_reg, _provider_reg, agent_reg = build_full_registry(enable_mcp=not no_mcp)

    # Resolve agent config if specified
    system_prompt = None
    if agent:
        agent_config = agent_reg.get(agent)
        if not agent_config:
            run_console.print(f"[red]Unknown agent: {agent}[/red]")
            run_console.print(f"[dim]Available: {', '.join(c.id for c in agent_reg.get_all())}[/dim]")
            return
        if not agent_config.enabled:
            run_console.print(f"[red]Agent '{agent}' is not enabled.[/red]")
            return
        system_prompt = agent_config.system_prompt or None
        run_console.print(Panel.fit(
            f"[bold]Agent:[/bold] {agent_config.name}\n[bold]Goal:[/bold] {goal}",
            border_style="cyan",
        ))
    else:
        run_console.print(Panel.fit(f"[bold]Goal:[/bold] {goal}", border_style="cyan"))

    try:
        backend = create_backend(provider=provider, model=model)
        spinner = Spinner(console=run_console)
        accumulated_text = ""
        session_cost = 0.0
        tool_start_time = None

        with Live("", console=run_console, refresh_per_second=15,
                  vertical_overflow="visible") as live:
            effective_confirm = confirm and not accept_all
            for event in run_autonomous_stream(
                goal=goal, backend=backend, tools=tool_reg,
                system_prompt=system_prompt,
                enable_mcp=not no_mcp, confirm=effective_confirm,
            ):
                if isinstance(event, TextDelta):
                    accumulated_text += event.text
                    live.update(Markdown(accumulated_text))

                elif isinstance(event, ToolCallStart):
                    # Freeze streamed text
                    live.update("")
                    if accumulated_text.strip():
                        run_console.print(Markdown(accumulated_text))
                    accumulated_text = ""
                    tool_start_time = time.monotonic()
                    verb = spinner.verb_for_tool(event.tool_name)
                    spinner.start(verb)

                elif isinstance(event, ToolCallResult):
                    spinner.stop()
                    elapsed_ms = 0.0
                    if tool_start_time:
                        elapsed_ms = (time.monotonic() - tool_start_time) * 1000
                        tool_start_time = None
                    result = event.result if isinstance(event.result, dict) else {}
                    render_tool_result(
                        run_console, event.tool_name, event.summary, elapsed_ms, result,
                    )

                elif isinstance(event, TurnComplete):
                    spinner.stop()
                    live.update("")
                    if accumulated_text.strip():
                        run_console.print(Markdown(accumulated_text))
                    accumulated_text = ""
                    tool_start_time = None
                    if event.usage:
                        turn_cost = event.estimated_cost
                        if turn_cost is not None:
                            session_cost += turn_cost
                        render_cost_line(run_console, event.usage, turn_cost, session_cost)

    except ValueError as e:
        run_console.print(f"[red]Error: {e}[/red]")
    except Exception as e:
        run_console.print(f"[red]Agent error: {e}[/red]")
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_autonomous.py -v`
Expected: ALL PASS

**Step 5: Run full suite**

Run: `python3 -m pytest tests/ -x -q`
Expected: All passing

**Step 6: Commit**

```bash
git add app/commands/run.py tests/test_autonomous.py
git commit -m "feat(run): unify rendering with REPL card system"
```

---

### Task 7: install.sh Banner Polish

**Files:**
- Modify: `install.sh`

**Context:** Cosmetic polish only. Tighten banner, use unicode step markers (✓/→/✗), consistent ANSI colors. No changes to detection logic, PATH setup, strategy selection, or fallback order.

**Step 1: No automated test for shell scripts** — verify manually.

**Step 2: Update the helpers and banner**

In `install.sh`, replace the helpers section (lines 22-25) and banner (lines 28-31):

Replace:
```sh
info()  { printf '  \033[1;34m%s\033[0m %s\n' "$1" "$2"; }
ok()    { printf '  \033[1;32m%s\033[0m %s\n' "$1" "$2"; }
warn()  { printf '  \033[1;33m%s\033[0m %s\n' "$1" "$2"; }
err()   { printf '  \033[1;31m%s\033[0m %s\n' "ERROR:" "$1" >&2; exit 1; }
```

With:
```sh
info()  { printf '  \033[2m→\033[0m %s %s\n' "$1" "$2"; }
ok()    { printf '  \033[1;32m✓\033[0m %s %s\n' "$1" "$2"; }
warn()  { printf '  \033[1;33m!\033[0m %s %s\n' "$1" "$2"; }
err()   { printf '  \033[1;31m✗\033[0m %s\n' "$1" >&2; exit 1; }
```

Replace the banner (lines 28-31):
```sh
printf '\n'
printf '  \033[1;36m⬡ PRISM\033[0m v%s\n' "$CURRENT_VERSION"
printf '  \033[2mPlatform for Research in Intelligent Synthesis of Materials\033[0m\n'
printf '  \033[2mBy MARC27 — marc27.com\033[0m\n\n'
```

With:
```sh
printf '\n'
printf '  \033[1;36m⬡ PRISM\033[0m v%s \033[2m— Materials Discovery Platform\033[0m\n\n' "$CURRENT_VERSION"
```

**Step 3: Update upgrade message**

In the install section (around line 258-262), change:
```sh
if [ "$UPGRADE" -eq 1 ]; then
    info "Upgrading:" "PRISM from GitHub..."
else
    info "Installing:" "PRISM from GitHub..."
fi
```

To:
```sh
if [ "$UPGRADE" -eq 1 ]; then
    info "Upgrading" "PRISM..."
else
    info "Installing" "PRISM..."
fi
```

**Step 4: Update the "Get started" section**

Replace lines 319-324:
```sh
    printf '  \033[1;36m⬡\033[0m Get started:\n'
    printf '\n'
    info "  Run:" "prism"
    info "  Help:" "prism --help"
    info "  Docs:" "https://github.com/Darth-Hidious/PRISM"
```

With:
```sh
    printf '\n'
    ok "Run" "'prism' to start"
    info "Help:" "prism --help"
```

**Step 5: Verify manually**

Run: `bash install.sh --help 2>&1 | head -5` (should show banner without errors)
Or: `bash -n install.sh` (syntax check)

**Step 6: Commit**

```bash
git add install.sh
git commit -m "style(install): polish banner with unicode step markers"
```

---

### Task 8: Final Integration Test

**Files:**
- Test: `tests/test_repl_cards.py`, `tests/test_repl.py`, `tests/test_autonomous.py`

**Step 1: Run the full test suite**

Run: `python3 -m pytest tests/ -x -q`
Expected: All passing (861 + new tests we added)

**Step 2: Verify card imports in run.py work**

Run: `python3 -c "from app.commands.run import run_goal; print('OK')"`
Expected: prints `OK`

**Step 3: Verify stream.py imports work**

Run: `python3 -c "from app.cli.tui.stream import handle_streaming_response; print('OK')"`
Expected: prints `OK`

**Step 4: Verify backward compat shims still work**

Run: `python3 -c "from app.agent.repl import AgentREPL; from app.agent.spinner import Spinner; print('OK')"`
Expected: prints `OK`

**Step 5: Syntax-check install.sh**

Run: `bash -n install.sh && echo OK`
Expected: `OK`

**Step 6: Final commit (if any fixups needed)**

```bash
git add -A && git commit -m "test: final integration verification for CLI UI polish"
```

---

## Task Summary

| Task | What | Files | Tests |
|------|------|-------|-------|
| 1 | Crystal alignment fix | theme.py | 1 new |
| 2 | TRUNCATION_CHARS + render_cost_line | theme.py, cards.py | 6 new |
| 3 | Char-based truncation in tool results | cards.py | 2 new |
| 4 | Live text streaming | stream.py | 1 new |
| 5 | Session cost tracking | app.py | 1 new |
| 6 | Unified `prism run` rendering | run.py | 1 new |
| 7 | install.sh banner polish | install.sh | manual |
| 8 | Final integration test | — | verification |

**Total: 6 files modified, 0 new files, 12 new tests, 8 commits.**
