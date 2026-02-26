# Ink Frontend Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a Protocol-Driven UI architecture with TypeScript Ink frontend and shared UIEmitter, maintaining full parity with the Rich (--classic) REPL.

**Architecture:** Extract all presentation logic from `stream.py` into `app/backend/ui_emitter.py`. Both the TS Ink frontend (via stdio JSON-RPC) and the Rich frontend (via direct import) consume the same UIEmitter. Frontends are dumb renderers — zero business logic duplication.

**Tech Stack:** Python 3.11+ (backend), TypeScript + Ink/React (frontend), Bun (compile), JSON-RPC 2.0 (protocol)

**Design Doc:** `docs/plans/2026-02-26-ink-frontend-design.md`

---

## Phase 1: Extract UIEmitter (Python only, no TS)

Move presentation logic out of `stream.py` into a shared `app/backend/` module. The Rich REPL keeps working identically — it just consumes events from UIEmitter instead of raw AgentCore events. All 873 existing tests must continue to pass.

---

### Task 1: Create `app/backend/protocol.py` — event type definitions

The single source of truth for all UI event types. Both Python and TypeScript (generated) depend on this.

**Files:**
- Create: `app/backend/__init__.py`
- Create: `app/backend/protocol.py`
- Test: `tests/test_backend_protocol.py`

**Step 1: Write the failing test**

```python
# tests/test_backend_protocol.py
"""Tests for the UI protocol event definitions."""
import pytest


def test_ui_events_dict_exists():
    from app.backend.protocol import UI_EVENTS
    assert isinstance(UI_EVENTS, dict)
    assert "ui.text.delta" in UI_EVENTS
    assert "ui.card" in UI_EVENTS
    assert "ui.cost" in UI_EVENTS
    assert "ui.prompt" in UI_EVENTS
    assert "ui.welcome" in UI_EVENTS
    assert "ui.turn.complete" in UI_EVENTS


def test_input_events_dict_exists():
    from app.backend.protocol import INPUT_EVENTS
    assert isinstance(INPUT_EVENTS, dict)
    assert "init" in INPUT_EVENTS
    assert "input.message" in INPUT_EVENTS
    assert "input.command" in INPUT_EVENTS
    assert "input.prompt_response" in INPUT_EVENTS


def test_make_event_creates_valid_jsonrpc():
    from app.backend.protocol import make_event
    event = make_event("ui.text.delta", {"text": "hello"})
    assert event["jsonrpc"] == "2.0"
    assert event["method"] == "ui.text.delta"
    assert event["params"]["text"] == "hello"


def test_make_event_rejects_unknown_method():
    from app.backend.protocol import make_event
    with pytest.raises(ValueError, match="Unknown event"):
        make_event("ui.nonexistent", {})


def test_parse_input_parses_valid_jsonrpc():
    from app.backend.protocol import parse_input
    msg = '{"jsonrpc":"2.0","method":"input.message","params":{"text":"hello"},"id":1}'
    parsed = parse_input(msg)
    assert parsed["method"] == "input.message"
    assert parsed["params"]["text"] == "hello"
    assert parsed["id"] == 1


def test_parse_input_rejects_invalid_json():
    from app.backend.protocol import parse_input
    with pytest.raises(ValueError):
        parse_input("not json")


def test_all_event_methods_listed():
    """Ensure all 10 backend→frontend and 5 frontend→backend events exist."""
    from app.backend.protocol import UI_EVENTS, INPUT_EVENTS
    expected_ui = [
        "ui.text.delta", "ui.text.flush", "ui.tool.start", "ui.card",
        "ui.cost", "ui.prompt", "ui.welcome", "ui.status",
        "ui.turn.complete", "ui.session.list",
    ]
    expected_input = [
        "init", "input.message", "input.command",
        "input.prompt_response", "input.load_session",
    ]
    for method in expected_ui:
        assert method in UI_EVENTS, f"Missing UI event: {method}"
    for method in expected_input:
        assert method in INPUT_EVENTS, f"Missing input event: {method}"


def test_emit_ts_produces_typescript():
    """The --emit-ts flag should produce parseable TypeScript type declarations."""
    from app.backend.protocol import emit_typescript
    ts = emit_typescript()
    assert "export interface" in ts or "export type" in ts
    assert "UiTextDelta" in ts
    assert "UiCard" in ts
    assert "InputMessage" in ts
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_backend_protocol.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'app.backend'`

**Step 3: Write minimal implementation**

```python
# app/backend/__init__.py
"""PRISM Backend — JSON-RPC server and UIEmitter for Protocol-Driven UI."""

# app/backend/protocol.py
"""UI Protocol — single source of truth for all event types.

Both the Python Rich frontend and TypeScript Ink frontend
consume these definitions. TypeScript types are generated via:
    python3 -m app.backend.protocol --emit-ts
"""
import json
from typing import Any

# Backend → Frontend events
UI_EVENTS: dict[str, dict[str, type]] = {
    "ui.text.delta":    {"text": str},
    "ui.text.flush":    {"text": str},
    "ui.tool.start":    {"tool_name": str, "call_id": str, "verb": str},
    "ui.card":          {"card_type": str, "tool_name": str, "elapsed_ms": float,
                         "content": str, "data": dict},
    "ui.cost":          {"input_tokens": int, "output_tokens": int,
                         "turn_cost": float, "session_cost": float},
    "ui.prompt":        {"prompt_type": str, "message": str, "choices": list,
                         "tool_name": str, "tool_args": dict},
    "ui.welcome":       {"version": str, "provider": str, "capabilities": dict,
                         "tool_count": int, "skill_count": int, "auto_approve": bool},
    "ui.status":        {"auto_approve": bool, "message_count": int, "has_plan": bool},
    "ui.turn.complete": {},
    "ui.session.list":  {"sessions": list},
}

# Frontend → Backend events
INPUT_EVENTS: dict[str, dict[str, type]] = {
    "init":                   {"provider": str, "auto_approve": bool, "resume": str},
    "input.message":          {"text": str},
    "input.command":          {"command": str},
    "input.prompt_response":  {"prompt_type": str, "response": str},
    "input.load_session":     {"session_id": str},
}


def make_event(method: str, params: dict[str, Any] | None = None) -> dict:
    """Create a JSON-RPC 2.0 notification (backend → frontend)."""
    if method not in UI_EVENTS:
        raise ValueError(f"Unknown event method: {method}")
    return {
        "jsonrpc": "2.0",
        "method": method,
        "params": params or {},
    }


def parse_input(raw: str) -> dict:
    """Parse a JSON-RPC 2.0 message from the frontend."""
    try:
        msg = json.loads(raw)
    except json.JSONDecodeError as e:
        raise ValueError(f"Invalid JSON: {e}") from e
    if "method" not in msg:
        raise ValueError("Missing 'method' field")
    return msg


def _method_to_interface_name(method: str) -> str:
    """Convert 'ui.text.delta' → 'UiTextDelta'."""
    return "".join(part.capitalize() for part in method.replace(".", "_").split("_"))


def _python_type_to_ts(t: type) -> str:
    """Map Python type hints to TypeScript types."""
    mapping = {str: "string", int: "number", float: "number",
               bool: "boolean", list: "any[]", dict: "Record<string, any>"}
    return mapping.get(t, "any")


def emit_typescript() -> str:
    """Generate TypeScript type declarations from event definitions."""
    lines = [
        "// Auto-generated from app/backend/protocol.py — DO NOT EDIT",
        "// Regenerate: python3 -m app.backend.protocol --emit-ts",
        "",
    ]
    # Backend → Frontend
    for method, fields in UI_EVENTS.items():
        name = _method_to_interface_name(method)
        lines.append(f"export interface {name} {{")
        for field, ftype in fields.items():
            ts_type = _python_type_to_ts(ftype)
            lines.append(f"  {field}: {ts_type};")
        lines.append("}")
        lines.append("")

    # Frontend → Backend
    for method, fields in INPUT_EVENTS.items():
        name = _method_to_interface_name(method)
        lines.append(f"export interface {name} {{")
        for field, ftype in fields.items():
            ts_type = _python_type_to_ts(ftype)
            lines.append(f"  {field}?: {ts_type};")
        lines.append("}")
        lines.append("")

    # Union types
    ui_names = [_method_to_interface_name(m) for m in UI_EVENTS]
    input_names = [_method_to_interface_name(m) for m in INPUT_EVENTS]
    lines.append(f"export type UIEvent = {' | '.join(ui_names)};")
    lines.append(f"export type InputEvent = {' | '.join(input_names)};")
    lines.append("")

    # Method→Interface mapping
    lines.append("export const UI_EVENT_MAP: Record<string, string> = {")
    for method in UI_EVENTS:
        name = _method_to_interface_name(method)
        lines.append(f'  "{method}": "{name}",')
    lines.append("};")
    lines.append("")

    return "\n".join(lines)


if __name__ == "__main__":
    import sys
    if "--emit-ts" in sys.argv:
        print(emit_typescript())
    else:
        print("Usage: python3 -m app.backend.protocol --emit-ts")
```

**Step 4: Run test to verify it passes**

Run: `python3 -m pytest tests/test_backend_protocol.py -v`
Expected: PASS (all 8 tests)

**Step 5: Verify existing tests still pass**

Run: `python3 -m pytest tests/ -x -q --timeout=30`
Expected: 873+ passed. No regressions.

**Step 6: Commit**

```bash
git add app/backend/__init__.py app/backend/protocol.py
git add -f tests/test_backend_protocol.py
git commit -m "feat(backend): add protocol.py — UI event type definitions (source of truth)"
```

---

### Task 2: Create `app/backend/ui_emitter.py` — extract logic from stream.py

Move plan detection, card type detection, cost accumulation, verb mapping from `stream.py` into UIEmitter. UIEmitter yields `dict` events (the same JSON-RPC shape both frontends consume).

**Files:**
- Create: `app/backend/ui_emitter.py`
- Test: `tests/test_ui_emitter.py`

**Step 1: Write the failing test**

```python
# tests/test_ui_emitter.py
"""Tests for UIEmitter — the shared presentation logic layer."""
import pytest
from unittest.mock import MagicMock
from app.agent.events import (
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete, UsageInfo,
)


def _make_agent_mock(events):
    """Create a mock agent whose process_stream yields the given events."""
    agent = MagicMock()
    agent.process_stream.return_value = iter(events)
    agent.tools.list_tools.return_value = [MagicMock(name=f"tool_{i}") for i in range(5)]
    agent.history = []
    agent.auto_approve = False
    return agent


def test_emitter_yields_text_delta():
    from app.backend.ui_emitter import UIEmitter
    agent = _make_agent_mock([
        TextDelta(text="Hello "),
        TextDelta(text="world"),
        TurnComplete(text="Hello world", usage=UsageInfo(input_tokens=100, output_tokens=20)),
    ])
    emitter = UIEmitter(agent)
    events = list(emitter.process("hi"))
    text_deltas = [e for e in events if e["method"] == "ui.text.delta"]
    assert len(text_deltas) == 2
    assert text_deltas[0]["params"]["text"] == "Hello "
    assert text_deltas[1]["params"]["text"] == "world"


def test_emitter_yields_tool_start_and_card():
    from app.backend.ui_emitter import UIEmitter
    agent = _make_agent_mock([
        ToolCallStart(tool_name="search_materials", call_id="abc"),
        ToolCallResult(call_id="abc", tool_name="search_materials",
                       result={"count": 5, "results": [{"id": f"mp-{i}"} for i in range(5)]},
                       summary="search_materials: 5 results"),
        TurnComplete(text=None, usage=UsageInfo(input_tokens=200, output_tokens=50)),
    ])
    emitter = UIEmitter(agent)
    events = list(emitter.process("find Fe"))
    tool_starts = [e for e in events if e["method"] == "ui.tool.start"]
    cards = [e for e in events if e["method"] == "ui.card"]
    assert len(tool_starts) == 1
    assert tool_starts[0]["params"]["tool_name"] == "search_materials"
    assert tool_starts[0]["params"]["verb"] == "Searching materials databases\u2026"
    assert len(cards) == 1
    assert cards[0]["params"]["card_type"] == "results"


def test_emitter_yields_cost_and_turn_complete():
    from app.backend.ui_emitter import UIEmitter
    agent = _make_agent_mock([
        TextDelta(text="answer"),
        TurnComplete(text="answer", usage=UsageInfo(input_tokens=100, output_tokens=20),
                     estimated_cost=0.001),
    ])
    emitter = UIEmitter(agent)
    events = list(emitter.process("question"))
    costs = [e for e in events if e["method"] == "ui.cost"]
    assert len(costs) == 1
    assert costs[0]["params"]["input_tokens"] == 100
    assert costs[0]["params"]["turn_cost"] == 0.001
    assert costs[0]["params"]["session_cost"] == 0.001
    completes = [e for e in events if e["method"] == "ui.turn.complete"]
    assert len(completes) == 1


def test_emitter_accumulates_session_cost():
    from app.backend.ui_emitter import UIEmitter
    agent = MagicMock()
    agent.tools.list_tools.return_value = []
    agent.history = []

    # First turn
    agent.process_stream.return_value = iter([
        TurnComplete(text="a", usage=UsageInfo(100, 20), estimated_cost=0.01),
    ])
    emitter = UIEmitter(agent)
    events1 = list(emitter.process("q1"))
    cost1 = [e for e in events1 if e["method"] == "ui.cost"][0]
    assert cost1["params"]["session_cost"] == pytest.approx(0.01)

    # Second turn
    agent.process_stream.return_value = iter([
        TurnComplete(text="b", usage=UsageInfo(100, 20), estimated_cost=0.02),
    ])
    events2 = list(emitter.process("q2"))
    cost2 = [e for e in events2 if e["method"] == "ui.cost"][0]
    assert cost2["params"]["session_cost"] == pytest.approx(0.03)


def test_emitter_flushes_text_on_tool_start():
    from app.backend.ui_emitter import UIEmitter
    agent = _make_agent_mock([
        TextDelta(text="thinking..."),
        ToolCallStart(tool_name="search_materials", call_id="x"),
        ToolCallResult(call_id="x", tool_name="search_materials",
                       result={"count": 0}, summary="0 results"),
        TurnComplete(text=None, usage=UsageInfo(100, 20)),
    ])
    emitter = UIEmitter(agent)
    events = list(emitter.process("go"))
    flushes = [e for e in events if e["method"] == "ui.text.flush"]
    assert len(flushes) == 1
    assert flushes[0]["params"]["text"] == "thinking..."


def test_emitter_detects_plan():
    from app.backend.ui_emitter import UIEmitter
    agent = _make_agent_mock([
        TextDelta(text="Here is my plan:\n<plan>"),
        TextDelta(text="1. Search\n2. Predict"),
        TextDelta(text="</plan>\nOK"),
        TurnComplete(text="done", usage=UsageInfo(100, 20)),
    ])
    emitter = UIEmitter(agent)
    events = list(emitter.process("complex goal"))
    plan_cards = [e for e in events if e["method"] == "ui.card" and e["params"].get("card_type") == "plan"]
    prompts = [e for e in events if e["method"] == "ui.prompt"]
    assert len(plan_cards) == 1
    assert "Search" in plan_cards[0]["params"]["content"]
    assert len(prompts) == 1
    assert prompts[0]["params"]["prompt_type"] == "plan_confirm"


def test_emitter_welcome():
    from app.backend.ui_emitter import UIEmitter
    agent = MagicMock()
    agent.tools.list_tools.return_value = [MagicMock() for _ in range(10)]
    agent.history = []
    emitter = UIEmitter(agent)
    event = emitter.welcome()
    assert event["method"] == "ui.welcome"
    assert event["params"]["tool_count"] == 10
    assert "version" in event["params"]
    assert "capabilities" in event["params"]


def test_emitter_detects_error_card_type():
    from app.backend.ui_emitter import UIEmitter
    agent = _make_agent_mock([
        ToolCallStart(tool_name="bad_tool", call_id="e1"),
        ToolCallResult(call_id="e1", tool_name="bad_tool",
                       result={"error": "something broke"},
                       summary="bad_tool: error"),
        TurnComplete(text=None, usage=UsageInfo(100, 20)),
    ])
    emitter = UIEmitter(agent)
    events = list(emitter.process("fail"))
    cards = [e for e in events if e["method"] == "ui.card"]
    assert cards[0]["params"]["card_type"] == "error"


def test_emitter_detects_metrics_card_type():
    from app.backend.ui_emitter import UIEmitter
    agent = _make_agent_mock([
        ToolCallStart(tool_name="predict_property", call_id="m1"),
        ToolCallResult(call_id="m1", tool_name="predict_property",
                       result={"metrics": {"mae": 0.1, "r2": 0.9}, "algorithm": "XGBoost"},
                       summary="predict_property: done"),
        TurnComplete(text=None, usage=UsageInfo(100, 20)),
    ])
    emitter = UIEmitter(agent)
    events = list(emitter.process("predict"))
    cards = [e for e in events if e["method"] == "ui.card"]
    assert cards[0]["params"]["card_type"] == "metrics"
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_ui_emitter.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'app.backend.ui_emitter'`

**Step 3: Write minimal implementation**

```python
# app/backend/ui_emitter.py
"""UIEmitter — the shared presentation logic layer.

Translates AgentCore streaming events into UI protocol events.
Both the Ink frontend (via stdio) and Rich frontend (via direct import)
consume this same class. All presentation logic lives HERE:
- Plan detection (<plan> tag parsing)
- Card type detection (metrics/calphad/error/etc.)
- Cost accumulation (session total)
- Tool verb mapping
- Text flush on tool start
"""
import time
from typing import Generator

from app.agent.events import (
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest,
)
from app.backend.protocol import make_event
from app.cli.tui.cards import detect_result_type
from app.cli.tui.spinner import TOOL_VERBS


class UIEmitter:
    """Consume AgentCore events, yield ui.* protocol events."""

    def __init__(self, agent, auto_approve: bool = False):
        self.agent = agent
        self.session_cost: float = 0.0
        self.message_count: int = 0
        self.auto_approve = auto_approve

    def welcome(self) -> dict:
        """Emit a ui.welcome event with system info."""
        from app import __version__
        from app.cli.tui.welcome import detect_capabilities, _detect_provider
        caps = detect_capabilities()
        provider = _detect_provider()
        skill_count = 0
        try:
            from app.skills.registry import load_builtin_skills
            skill_count = len(load_builtin_skills().list_skills())
        except Exception:
            pass
        return make_event("ui.welcome", {
            "version": __version__,
            "provider": provider or "",
            "capabilities": caps,
            "tool_count": len(self.agent.tools.list_tools()),
            "skill_count": skill_count,
            "auto_approve": self.auto_approve,
        })

    def process(self, user_input: str) -> Generator[dict, None, None]:
        """Yield ui.* events for a user message."""
        self.message_count += 1
        accumulated_text = ""
        plan_buffer = ""
        in_plan = False
        tool_start_time = None

        for event in self.agent.process_stream(user_input):

            if isinstance(event, TextDelta):
                accumulated_text += event.text

                # Plan detection
                if "<plan>" in accumulated_text and not in_plan:
                    in_plan = True
                    plan_buffer = accumulated_text.split("<plan>", 1)[1]
                    pre = accumulated_text.split("<plan>", 1)[0].strip()
                    if pre:
                        yield make_event("ui.text.flush", {"text": pre})
                    accumulated_text = ""
                    continue
                elif in_plan:
                    if "</plan>" in event.text:
                        plan_buffer += event.text.split("</plan>")[0]
                        in_plan = False
                        yield make_event("ui.card", {
                            "card_type": "plan",
                            "tool_name": "",
                            "elapsed_ms": 0,
                            "content": plan_buffer.strip(),
                            "data": {},
                        })
                        yield make_event("ui.prompt", {
                            "prompt_type": "plan_confirm",
                            "message": "Execute this plan?",
                            "choices": ["y", "n"],
                            "tool_name": "",
                            "tool_args": {},
                        })
                        remainder = (
                            event.text.split("</plan>", 1)[1]
                            if "</plan>" in event.text else ""
                        )
                        accumulated_text = remainder
                    else:
                        plan_buffer += event.text
                    continue

                # Normal streaming
                if not in_plan:
                    yield make_event("ui.text.delta", {"text": event.text})

            elif isinstance(event, ToolCallStart):
                # Flush accumulated text
                if accumulated_text.strip():
                    yield make_event("ui.text.flush", {"text": accumulated_text.strip()})
                    accumulated_text = ""
                tool_start_time = time.monotonic()
                verb = TOOL_VERBS.get(event.tool_name, "Thinking\u2026")
                yield make_event("ui.tool.start", {
                    "tool_name": event.tool_name,
                    "call_id": event.call_id,
                    "verb": verb,
                })

            elif isinstance(event, ToolApprovalRequest):
                yield make_event("ui.prompt", {
                    "prompt_type": "approval",
                    "message": f"Allow {event.tool_name}?",
                    "choices": ["y", "n", "a"],
                    "tool_name": event.tool_name,
                    "tool_args": event.tool_args,
                })

            elif isinstance(event, ToolCallResult):
                elapsed_ms = 0.0
                if tool_start_time:
                    elapsed_ms = (time.monotonic() - tool_start_time) * 1000
                    tool_start_time = None
                result = event.result if isinstance(event.result, dict) else {}
                card_type = detect_result_type(result)
                yield make_event("ui.card", {
                    "card_type": card_type,
                    "tool_name": event.tool_name,
                    "elapsed_ms": elapsed_ms,
                    "content": event.summary or "",
                    "data": result,
                })

            elif isinstance(event, TurnComplete):
                if accumulated_text.strip():
                    yield make_event("ui.text.flush", {"text": accumulated_text.strip()})
                    accumulated_text = ""
                tool_start_time = None
                turn_cost = event.estimated_cost or 0.0
                self.session_cost += turn_cost
                if event.usage:
                    yield make_event("ui.cost", {
                        "input_tokens": event.usage.input_tokens,
                        "output_tokens": event.usage.output_tokens,
                        "turn_cost": turn_cost,
                        "session_cost": self.session_cost,
                    })
                yield make_event("ui.turn.complete", {})
```

**Step 4: Run test to verify it passes**

Run: `python3 -m pytest tests/test_ui_emitter.py -v`
Expected: PASS (all 10 tests)

**Step 5: Verify existing tests still pass**

Run: `python3 -m pytest tests/ -x -q --timeout=30`
Expected: 873+ passed. UIEmitter is additive — nothing changed yet.

**Step 6: Commit**

```bash
git add app/backend/ui_emitter.py
git add -f tests/test_ui_emitter.py
git commit -m "feat(backend): add UIEmitter — shared presentation logic layer"
```

---

### Task 3: Refactor `stream.py` to consume UIEmitter

Replace the raw AgentCore event processing in `stream.py` with UIEmitter consumption. Behavior is identical — same visual output. This is the critical refactor that proves the Protocol-Driven UI pattern works.

**Files:**
- Modify: `app/cli/tui/stream.py` (full rewrite)
- Modify: `app/cli/tui/app.py` (use UIEmitter)
- Test: `tests/test_stream_refactor.py`

**Step 1: Write the failing test**

```python
# tests/test_stream_refactor.py
"""Verify stream.py now consumes UIEmitter events, not raw AgentCore events."""
from unittest.mock import MagicMock, patch


def test_stream_uses_ui_emitter():
    """Confirm handle_streaming_response creates/uses a UIEmitter."""
    with patch("app.cli.tui.stream.UIEmitter") as MockEmitter:
        mock_instance = MagicMock()
        mock_instance.process.return_value = iter([
            {"jsonrpc": "2.0", "method": "ui.text.delta", "params": {"text": "hi"}},
            {"jsonrpc": "2.0", "method": "ui.text.flush", "params": {"text": "hi"}},
            {"jsonrpc": "2.0", "method": "ui.cost", "params": {
                "input_tokens": 100, "output_tokens": 20,
                "turn_cost": 0.001, "session_cost": 0.001,
            }},
            {"jsonrpc": "2.0", "method": "ui.turn.complete", "params": {}},
        ])
        mock_instance.session_cost = 0.001
        MockEmitter.return_value = mock_instance

        from app.cli.tui.stream import handle_streaming_response
        console = MagicMock()
        agent = MagicMock()
        session = MagicMock()

        result = handle_streaming_response(console, agent, "test", session)
        MockEmitter.assert_called_once()
        mock_instance.process.assert_called_once_with("test")
        assert result == 0.001


def test_stream_renders_tool_card():
    """Confirm ui.card events dispatch to render_tool_result."""
    with patch("app.cli.tui.stream.UIEmitter") as MockEmitter, \
         patch("app.cli.tui.stream.render_tool_result") as mock_render:
        mock_instance = MagicMock()
        mock_instance.process.return_value = iter([
            {"jsonrpc": "2.0", "method": "ui.tool.start",
             "params": {"tool_name": "search", "call_id": "a", "verb": "Searching..."}},
            {"jsonrpc": "2.0", "method": "ui.card",
             "params": {"card_type": "tool", "tool_name": "search",
                        "elapsed_ms": 500, "content": "done", "data": {"count": 3}}},
            {"jsonrpc": "2.0", "method": "ui.turn.complete", "params": {}},
        ])
        mock_instance.session_cost = 0.0
        MockEmitter.return_value = mock_instance

        from app.cli.tui.stream import handle_streaming_response
        console = MagicMock()
        handle_streaming_response(console, MagicMock(), "q", MagicMock())
        mock_render.assert_called_once()
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_stream_refactor.py -v`
Expected: FAIL — `stream.py` doesn't import UIEmitter yet.

**Step 3: Rewrite `stream.py`**

```python
# app/cli/tui/stream.py
"""Streaming event handler — Rich frontend renderer for UIEmitter events.

This is the Rich (--classic) renderer. It consumes ui.* protocol events
from UIEmitter and renders them using Rich console/panels/cards.
All logic (plan detection, card detection, cost calc) is in UIEmitter.
"""
from prompt_toolkit import PromptSession
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown

from app.agent.scratchpad import Scratchpad
from app.backend.ui_emitter import UIEmitter
from app.cli.tui.cards import (
    render_input_card, render_plan_card, render_tool_result,
    render_cost_line,
)
from app.cli.tui.prompt import ask_plan_confirmation
from app.cli.tui.spinner import Spinner


def handle_streaming_response(
    console: Console,
    agent,
    user_input: str,
    session: PromptSession,
    scratchpad: Scratchpad | None = None,
    session_cost: float = 0.0,
) -> float:
    """Process an agent stream via UIEmitter, rendering as Rich cards.

    Returns the updated session_cost (accumulated).
    """
    render_input_card(console, user_input)

    emitter = UIEmitter(agent)
    emitter.session_cost = session_cost
    spinner = Spinner(console=console)
    accumulated_text = ""

    with Live("", console=console, refresh_per_second=15,
              vertical_overflow="visible") as live:

        for event in emitter.process(user_input):
            method = event["method"]
            params = event["params"]

            if method == "ui.text.delta":
                accumulated_text += params["text"]
                live.update(Markdown(accumulated_text))

            elif method == "ui.text.flush":
                live.update("")
                accumulated_text = ""
                if params["text"].strip():
                    console.print(Markdown(params["text"]))

            elif method == "ui.tool.start":
                spinner.start(params["verb"])

            elif method == "ui.card":
                spinner.stop()
                if params["card_type"] == "plan":
                    render_plan_card(console, params["content"])
                    if scratchpad:
                        scratchpad.log(
                            "plan", summary="Plan proposed",
                            data={"plan": params["content"]},
                        )
                    if not ask_plan_confirmation(session):
                        console.print("[dim]Cancelled.[/dim]")
                        return emitter.session_cost
                else:
                    render_tool_result(
                        console,
                        params["tool_name"],
                        params["content"],
                        params["elapsed_ms"],
                        params.get("data", {}),
                    )

            elif method == "ui.prompt":
                if params["prompt_type"] == "plan_confirm":
                    pass  # Handled by ui.card plan above
                # Other prompts (approval) handled by approval_callback

            elif method == "ui.cost":
                from app.agent.events import UsageInfo
                usage = UsageInfo(
                    input_tokens=params["input_tokens"],
                    output_tokens=params["output_tokens"],
                )
                render_cost_line(
                    console, usage,
                    params.get("turn_cost"),
                    params.get("session_cost", 0.0),
                )

            elif method == "ui.turn.complete":
                spinner.stop()

    # Flush any remaining text
    if accumulated_text.strip():
        console.print(Markdown(accumulated_text))

    return emitter.session_cost
```

**Step 4: Run test to verify it passes**

Run: `python3 -m pytest tests/test_stream_refactor.py -v`
Expected: PASS

**Step 5: Run ALL existing tests**

Run: `python3 -m pytest tests/ -x -q --timeout=30`
Expected: 873+ passed. The Rich REPL behaves identically — same event flow, same visual output. The only difference is the indirection through UIEmitter.

**Step 6: Commit**

```bash
git add app/cli/tui/stream.py
git add -f tests/test_stream_refactor.py
git commit -m "refactor(stream): consume UIEmitter events instead of raw AgentCore events"
```

---

### Task 4: Update `pyproject.toml` and verify

Add `app.backend` to setuptools packages so it's included in the wheel.

**Files:**
- Modify: `pyproject.toml:102` (add `"app.backend"` to packages list)

**Step 1: Edit pyproject.toml**

In `pyproject.toml` line 102, add `"app.backend"` to the packages list:

```
[tool.setuptools]
packages = ["app", "app.config", "app.db", "app.agent", "app.agent.backends", "app.tools", "app.data", "app.commands", "app.ml", "app.simulation", "app.skills", "app.plugins", "app.validation", "app.cli", "app.cli.tui", "app.cli.slash", "app.search", "app.search.cache", "app.search.providers", "app.search.resilience", "app.backend"]
```

**Step 2: Run full test suite**

Run: `python3 -m pytest tests/ -x -q --timeout=30`
Expected: 873+ passed.

**Step 3: Verify protocol codegen works**

Run: `python3 -m app.backend.protocol --emit-ts | head -20`
Expected: TypeScript interface output.

**Step 4: Commit**

```bash
git add pyproject.toml
git commit -m "chore: add app.backend to setuptools packages"
```

---

## Phase 2: stdio Server

Wire UIEmitter to stdin/stdout JSON-RPC. Testable without any TypeScript.

---

### Task 5: Create `app/backend/server.py` — JSON-RPC stdio server

Thin loop: read JSON from stdin, dispatch to UIEmitter, write JSON to stdout.

**Files:**
- Create: `app/backend/server.py`
- Test: `tests/test_backend_server.py`

**Step 1: Write the failing test**

```python
# tests/test_backend_server.py
"""Tests for the JSON-RPC stdio server."""
import json
from unittest.mock import MagicMock, patch
from io import StringIO


def test_server_handles_init():
    from app.backend.server import StdioServer
    server = StdioServer()
    output = StringIO()

    with patch("app.backend.server.create_backend") as mock_cb, \
         patch("app.backend.server.AgentCore") as mock_ac, \
         patch("app.backend.server.build_full_registry") as mock_reg:
        mock_reg.return_value = (MagicMock(), None, None)
        mock_agent = MagicMock()
        mock_agent.tools.list_tools.return_value = []
        mock_ac.return_value = mock_agent

        msg = json.dumps({"jsonrpc": "2.0", "method": "init", "params": {}, "id": 1})
        server.handle_message(msg, output)

    lines = output.getvalue().strip().split("\n")
    # Should get: result for init + welcome event
    result = json.loads(lines[0])
    assert result["id"] == 1
    assert result["result"]["ok"] is True


def test_server_handles_input_message():
    from app.backend.server import StdioServer
    server = StdioServer()
    # Pre-set emitter
    mock_emitter = MagicMock()
    mock_emitter.process.return_value = iter([
        {"jsonrpc": "2.0", "method": "ui.text.delta", "params": {"text": "hi"}},
        {"jsonrpc": "2.0", "method": "ui.turn.complete", "params": {}},
    ])
    server.emitter = mock_emitter

    output = StringIO()
    msg = json.dumps({"jsonrpc": "2.0", "method": "input.message",
                       "params": {"text": "hello"}, "id": 2})
    server.handle_message(msg, output)

    lines = output.getvalue().strip().split("\n")
    events = [json.loads(l) for l in lines]
    methods = [e.get("method") for e in events]
    assert "ui.text.delta" in methods
    assert "ui.turn.complete" in methods


def test_server_handles_input_command():
    from app.backend.server import StdioServer
    server = StdioServer()
    mock_emitter = MagicMock()
    mock_emitter.handle_command.return_value = iter([
        {"jsonrpc": "2.0", "method": "ui.card",
         "params": {"card_type": "info", "content": "help text",
                    "tool_name": "", "elapsed_ms": 0, "data": {}}},
    ])
    server.emitter = mock_emitter

    output = StringIO()
    msg = json.dumps({"jsonrpc": "2.0", "method": "input.command",
                       "params": {"command": "/help"}, "id": 3})
    server.handle_message(msg, output)
    mock_emitter.handle_command.assert_called_once_with("/help")


def test_server_rejects_unknown_method():
    from app.backend.server import StdioServer
    server = StdioServer()
    output = StringIO()
    msg = json.dumps({"jsonrpc": "2.0", "method": "unknown.method",
                       "params": {}, "id": 4})
    server.handle_message(msg, output)
    result = json.loads(output.getvalue().strip())
    assert "error" in result
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_backend_server.py -v`
Expected: FAIL — module not found.

**Step 3: Write implementation**

```python
# app/backend/server.py
"""JSON-RPC 2.0 server over stdio — thin wrapper around UIEmitter.

Usage: python3 -m app.backend
The Ink frontend spawns this as a child process and communicates
via stdin (JSON lines) / stdout (JSON lines).
"""
import json
import sys
from typing import TextIO

from app.backend.protocol import parse_input


class StdioServer:
    """JSON-RPC server on stdin/stdout."""

    def __init__(self):
        self.emitter = None

    def handle_message(self, raw: str, output: TextIO):
        """Process a single JSON-RPC message and write responses to output."""
        try:
            msg = parse_input(raw)
        except ValueError as e:
            self._send_error(output, None, -32700, str(e))
            return

        method = msg["method"]
        params = msg.get("params", {})
        msg_id = msg.get("id")

        if method == "init":
            self._handle_init(params, msg_id, output)
        elif method == "input.message":
            self._handle_input(params, output)
        elif method == "input.command":
            self._handle_command(params, output)
        elif method == "input.prompt_response":
            self._handle_prompt_response(params, output)
        elif method == "input.load_session":
            self._handle_load_session(params, msg_id, output)
        else:
            self._send_error(output, msg_id, -32601, f"Unknown method: {method}")

    def _handle_init(self, params: dict, msg_id, output: TextIO):
        from app.agent.factory import create_backend
        from app.agent.core import AgentCore
        from app.plugins.bootstrap import build_full_registry

        provider = params.get("provider") or None
        auto_approve = params.get("auto_approve", False)

        backend = create_backend(provider=provider)
        tools, _, _ = build_full_registry(enable_mcp=True)
        agent = AgentCore(backend=backend, tools=tools, auto_approve=auto_approve)

        from app.backend.ui_emitter import UIEmitter
        self.emitter = UIEmitter(agent, auto_approve=auto_approve)

        self._send_result(output, msg_id, {"ok": True})
        self._emit(output, self.emitter.welcome())

    def _handle_input(self, params: dict, output: TextIO):
        if not self.emitter:
            return
        for event in self.emitter.process(params.get("text", "")):
            self._emit(output, event)

    def _handle_command(self, params: dict, output: TextIO):
        if not self.emitter:
            return
        for event in self.emitter.handle_command(params.get("command", "")):
            self._emit(output, event)

    def _handle_prompt_response(self, params: dict, output: TextIO):
        # Store response for UIEmitter to consume
        pass

    def _handle_load_session(self, params: dict, msg_id, output: TextIO):
        if not self.emitter:
            self._send_error(output, msg_id, -32000, "Not initialized")
            return
        try:
            from app.agent.memory import SessionMemory
            memory = SessionMemory()
            memory.load(params["session_id"])
            self.emitter.agent.history = list(memory.get_history())
            self._send_result(output, msg_id, {"ok": True, "messages": len(self.emitter.agent.history)})
        except Exception as e:
            self._send_error(output, msg_id, -32000, str(e))

    def _emit(self, output: TextIO, event: dict):
        output.write(json.dumps(event) + "\n")
        output.flush()

    def _send_result(self, output: TextIO, msg_id, result):
        output.write(json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result}) + "\n")
        output.flush()

    def _send_error(self, output: TextIO, msg_id, code: int, message: str):
        output.write(json.dumps({
            "jsonrpc": "2.0", "id": msg_id,
            "error": {"code": code, "message": message},
        }) + "\n")
        output.flush()

    def run(self):
        """Main loop: read stdin line by line, dispatch, write stdout."""
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            self.handle_message(line, sys.stdout)
```

**Step 4: Run test to verify it passes**

Run: `python3 -m pytest tests/test_backend_server.py -v`
Expected: PASS (all 4 tests)

**Step 5: Commit**

```bash
git add app/backend/server.py
git add -f tests/test_backend_server.py
git commit -m "feat(backend): add JSON-RPC stdio server"
```

---

### Task 6: Create `app/backend/__main__.py` + slash command handler in UIEmitter

Wire up `python -m app.backend` entry point and add `handle_command` to UIEmitter.

**Files:**
- Create: `app/backend/__main__.py`
- Modify: `app/backend/ui_emitter.py` (add `handle_command` method)
- Test: `tests/test_backend_main.py`

**Step 1: Write the failing test**

```python
# tests/test_backend_main.py
"""Test that python -m app.backend is runnable."""
import subprocess
import json
import sys


def test_backend_main_responds_to_init(tmp_path):
    """Send init via stdin, expect JSON response on stdout."""
    init_msg = json.dumps({"jsonrpc": "2.0", "method": "init", "params": {}, "id": 1})
    proc = subprocess.run(
        [sys.executable, "-m", "app.backend"],
        input=init_msg + "\n",
        capture_output=True, text=True, timeout=15,
    )
    # Should have at least one line of output (the init result)
    lines = [l for l in proc.stdout.strip().split("\n") if l]
    assert len(lines) >= 1
    first = json.loads(lines[0])
    assert first.get("id") == 1
    # Either success or a predictable error (no API key in test env)
    assert "result" in first or "error" in first
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_backend_main.py -v`
Expected: FAIL — `No module named 'app.backend.__main__'`

**Step 3: Write implementation**

```python
# app/backend/__main__.py
"""Entry point: python3 -m app.backend

Starts the JSON-RPC stdio server for the Ink frontend.
"""
from app.backend.server import StdioServer

if __name__ == "__main__":
    server = StdioServer()
    server.run()
```

Add `handle_command` to UIEmitter:

```python
# Add to app/backend/ui_emitter.py

    def handle_command(self, command: str) -> Generator[dict, None, None]:
        """Handle slash commands. Yields ui.* events."""
        from app.backend.protocol import make_event
        parts = command.strip().split(maxsplit=1)
        base_cmd = parts[0].lower()
        arg = parts[1].strip() if len(parts) > 1 else ""

        if base_cmd in ("/exit", "/quit"):
            yield make_event("ui.status", {
                "auto_approve": self.auto_approve,
                "message_count": self.message_count,
                "has_plan": False,
            })
            return

        if base_cmd == "/help":
            from app.cli.slash.registry import REPL_COMMANDS
            lines = []
            for name, desc in REPL_COMMANDS.items():
                if name == "/quit":
                    continue
                lines.append(f"  {name:<16} {desc}")
            yield make_event("ui.card", {
                "card_type": "info",
                "tool_name": "",
                "elapsed_ms": 0,
                "content": "\n".join(lines),
                "data": {},
            })
            return

        if base_cmd == "/tools":
            tools = self.agent.tools.list_tools()
            lines = []
            for tool in tools:
                flag = " \u2605" if tool.requires_approval else ""
                lines.append(f"  {tool.name:<28} {tool.description[:55]}{flag}")
            lines.append(f"\n  {len(tools)} tools  \u2605 = requires approval")
            yield make_event("ui.card", {
                "card_type": "info",
                "tool_name": "",
                "elapsed_ms": 0,
                "content": "\n".join(lines),
                "data": {},
            })
            return

        if base_cmd == "/approve-all":
            self.auto_approve = True
            self.agent.auto_approve = True
            yield make_event("ui.status", {
                "auto_approve": True,
                "message_count": self.message_count,
                "has_plan": False,
            })
            return

        if base_cmd == "/history":
            yield make_event("ui.card", {
                "card_type": "info", "tool_name": "", "elapsed_ms": 0,
                "content": f"{len(self.agent.history)} messages",
                "data": {},
            })
            return

        if base_cmd == "/sessions":
            from app.agent.memory import SessionMemory
            sessions = SessionMemory.list_sessions()
            yield make_event("ui.session.list", {
                "sessions": [
                    {"session_id": s["session_id"],
                     "timestamp": s.get("timestamp", ""),
                     "message_count": s.get("message_count", 0)}
                    for s in sessions[:20]
                ],
            })
            return

        # Unknown command
        yield make_event("ui.card", {
            "card_type": "info", "tool_name": "", "elapsed_ms": 0,
            "content": f"Unknown: {base_cmd}  \u2014  /help for commands",
            "data": {},
        })
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_backend_main.py tests/test_backend_server.py tests/test_ui_emitter.py -v`
Expected: PASS

**Step 5: Commit**

```bash
git add app/backend/__main__.py app/backend/ui_emitter.py
git add -f tests/test_backend_main.py
git commit -m "feat(backend): add __main__ entry point and slash command handling"
```

---

### Task 7: Integration test — full stdio roundtrip

End-to-end test that sends multiple JSON-RPC messages and verifies the protocol works correctly.

**Files:**
- Test: `tests/test_backend_integration.py`

**Step 1: Write the integration test**

```python
# tests/test_backend_integration.py
"""Integration test: full JSON-RPC roundtrip over StdioServer."""
import json
from io import StringIO
from unittest.mock import MagicMock, patch
from app.agent.events import TextDelta, TurnComplete, UsageInfo


def test_full_roundtrip():
    """init → input.message → verify protocol events."""
    from app.backend.server import StdioServer

    server = StdioServer()
    output = StringIO()

    # Mock the backend creation
    with patch("app.backend.server.create_backend"), \
         patch("app.backend.server.AgentCore") as MockAgent, \
         patch("app.backend.server.build_full_registry") as mock_reg:

        mock_tools = MagicMock()
        mock_tools.list_tools.return_value = [MagicMock(name="t1")]
        mock_reg.return_value = (mock_tools, None, None)

        mock_agent = MagicMock()
        mock_agent.tools = mock_tools
        mock_agent.history = []
        mock_agent.auto_approve = False
        mock_agent.process_stream.return_value = iter([
            TextDelta(text="The answer is 42."),
            TurnComplete(text="The answer is 42.",
                         usage=UsageInfo(500, 100),
                         estimated_cost=0.005),
        ])
        MockAgent.return_value = mock_agent

        # 1. Init
        init = json.dumps({"jsonrpc": "2.0", "method": "init", "params": {}, "id": 1})
        server.handle_message(init, output)

        # 2. Send a message
        msg = json.dumps({"jsonrpc": "2.0", "method": "input.message",
                           "params": {"text": "What is 6*7?"}, "id": 2})
        server.handle_message(msg, output)

    # Parse all output
    lines = [l for l in output.getvalue().strip().split("\n") if l]
    events = [json.loads(l) for l in lines]

    # Verify init response
    init_resp = events[0]
    assert init_resp["id"] == 1
    assert init_resp["result"]["ok"] is True

    # Verify welcome
    welcome = events[1]
    assert welcome["method"] == "ui.welcome"

    # Find streaming events
    methods = [e.get("method") for e in events[2:]]
    assert "ui.text.delta" in methods
    assert "ui.cost" in methods
    assert "ui.turn.complete" in methods

    # Verify cost event
    cost_event = [e for e in events if e.get("method") == "ui.cost"][0]
    assert cost_event["params"]["input_tokens"] == 500
    assert cost_event["params"]["turn_cost"] == 0.005


def test_command_roundtrip():
    """init → /help → verify info card."""
    from app.backend.server import StdioServer

    server = StdioServer()
    output = StringIO()

    with patch("app.backend.server.create_backend"), \
         patch("app.backend.server.AgentCore") as MockAgent, \
         patch("app.backend.server.build_full_registry") as mock_reg:

        mock_tools = MagicMock()
        mock_tools.list_tools.return_value = []
        mock_reg.return_value = (mock_tools, None, None)
        mock_agent = MagicMock()
        mock_agent.tools = mock_tools
        mock_agent.history = []
        MockAgent.return_value = mock_agent

        init = json.dumps({"jsonrpc": "2.0", "method": "init", "params": {}, "id": 1})
        server.handle_message(init, output)

        cmd = json.dumps({"jsonrpc": "2.0", "method": "input.command",
                           "params": {"command": "/help"}, "id": 2})
        server.handle_message(cmd, output)

    lines = [l for l in output.getvalue().strip().split("\n") if l]
    events = [json.loads(l) for l in lines]

    # Find the help card
    cards = [e for e in events if e.get("method") == "ui.card"]
    assert len(cards) >= 1
    assert "/help" in cards[0]["params"]["content"]
```

**Step 2: Run test**

Run: `python3 -m pytest tests/test_backend_integration.py -v`
Expected: PASS

**Step 3: Run full suite**

Run: `python3 -m pytest tests/ -x -q --timeout=30`
Expected: 873+ passed (plus new tests ≈ 890+)

**Step 4: Commit**

```bash
git add -f tests/test_backend_integration.py
git commit -m "test(backend): add integration test for full stdio roundtrip"
```

---

## Phase 3: Scaffold Ink Frontend

Set up the TypeScript project and build components incrementally.

---

### Task 8: Scaffold `frontend/` project

Initialize the Ink project with all dependencies, tsconfig, and build script.

**Files:**
- Create: `frontend/package.json`
- Create: `frontend/tsconfig.json`
- Create: `frontend/build.ts`
- Create: `frontend/.gitignore`

**Step 1: Create package.json**

```json
{
  "name": "prism-tui",
  "version": "2.5.0",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "bun run src/index.tsx",
    "build": "bun run build.ts",
    "typecheck": "tsc --noEmit"
  },
  "dependencies": {
    "ink": "^5.1.0",
    "ink-markdown": "^2.0.0",
    "ink-spinner": "^5.0.0",
    "ink-text-input": "^6.0.0",
    "react": "^18.3.0"
  },
  "devDependencies": {
    "@types/react": "^18.3.0",
    "typescript": "^5.7.0",
    "@types/bun": "latest"
  }
}
```

**Step 2: Create tsconfig.json**

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "outDir": "dist",
    "rootDir": "src",
    "esModuleInterop": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true,
    "resolveJsonModule": true,
    "declaration": true,
    "declarationMap": true,
    "sourceMap": true
  },
  "include": ["src/**/*"],
  "exclude": ["node_modules", "dist"]
}
```

**Step 3: Create build.ts**

```typescript
// frontend/build.ts
// Compile the Ink TUI to standalone binaries for each platform.
import { $ } from "bun";

const targets = [
  { target: "bun-darwin-arm64", out: "prism-tui-darwin-arm64" },
  { target: "bun-darwin-x64", out: "prism-tui-darwin-x64" },
  { target: "bun-linux-x64", out: "prism-tui-linux-x64" },
  { target: "bun-linux-arm64", out: "prism-tui-linux-arm64" },
];

const buildTarget = process.argv[2]; // optional: build only one target

for (const { target, out } of targets) {
  if (buildTarget && !target.includes(buildTarget)) continue;
  console.log(`Building ${target}...`);
  await $`bun build src/index.tsx --compile --target=${target} --outfile dist/${out}`;
}

console.log("Done.");
```

**Step 4: Create .gitignore**

```
node_modules/
dist/
*.tsbuildinfo
```

**Step 5: Install deps**

Run: `cd frontend && bun install`
Expected: node_modules created, lockfile generated.

**Step 6: Commit**

```bash
git add frontend/package.json frontend/tsconfig.json frontend/build.ts frontend/.gitignore frontend/bun.lockb
git commit -m "chore(frontend): scaffold Ink project with deps and build script"
```

---

### Task 9: Create bridge/client.ts — JSON-RPC client over stdio

The communication layer that spawns the Python backend and talks to it.

**Files:**
- Create: `frontend/src/bridge/client.ts`
- Create: `frontend/src/bridge/types.ts` (generated)

**Step 1: Generate types from protocol.py**

Run: `python3 -m app.backend.protocol --emit-ts > frontend/src/bridge/types.ts`

**Step 2: Write client.ts**

```typescript
// frontend/src/bridge/client.ts
import { spawn, type ChildProcess } from "child_process";
import { createInterface, type Interface } from "readline";
import { EventEmitter } from "events";

export interface JsonRpcMessage {
  jsonrpc: "2.0";
  method?: string;
  params?: Record<string, any>;
  id?: number;
  result?: any;
  error?: { code: number; message: string };
}

export class BackendClient extends EventEmitter {
  private process: ChildProcess;
  private rl: Interface;
  private nextId = 1;
  private pending = new Map<number, { resolve: Function; reject: Function }>();

  constructor(pythonPath: string) {
    super();
    this.process = spawn(pythonPath, ["-m", "app.backend"], {
      stdio: ["pipe", "pipe", "inherit"],
    });

    this.rl = createInterface({ input: this.process.stdout! });
    this.rl.on("line", (line) => this.handleLine(line));

    this.process.on("exit", (code) => {
      this.emit("exit", code);
    });
  }

  private handleLine(line: string) {
    try {
      const msg: JsonRpcMessage = JSON.parse(line);
      // Response to a request
      if (msg.id !== undefined && (msg.result !== undefined || msg.error !== undefined)) {
        const pending = this.pending.get(msg.id);
        if (pending) {
          this.pending.delete(msg.id);
          if (msg.error) pending.reject(new Error(msg.error.message));
          else pending.resolve(msg.result);
        }
        return;
      }
      // Notification (backend → frontend event)
      if (msg.method) {
        this.emit("event", { method: msg.method, params: msg.params || {} });
      }
    } catch {
      // Ignore unparseable lines
    }
  }

  async request(method: string, params: Record<string, any> = {}): Promise<any> {
    const id = this.nextId++;
    const msg = JSON.stringify({ jsonrpc: "2.0", method, params, id });
    this.process.stdin!.write(msg + "\n");

    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`Request ${method} timed out`));
        }
      }, 30000);
    });
  }

  send(method: string, params: Record<string, any> = {}) {
    const id = this.nextId++;
    const msg = JSON.stringify({ jsonrpc: "2.0", method, params, id });
    this.process.stdin!.write(msg + "\n");
  }

  destroy() {
    this.process.kill();
  }
}
```

**Step 3: Verify typecheck**

Run: `cd frontend && bun run typecheck`
Expected: No errors.

**Step 4: Commit**

```bash
git add frontend/src/bridge/client.ts frontend/src/bridge/types.ts
git commit -m "feat(frontend): add JSON-RPC client and generated types"
```

---

### Task 10: Create theme.ts and core components

Mirror `theme.py` hex values and build the first components: `<Prompt />`, `<StreamingText />`, `<Spinner />`, `<CostLine />`.

**Files:**
- Create: `frontend/src/theme.ts`
- Create: `frontend/src/components/Prompt.tsx`
- Create: `frontend/src/components/StreamingText.tsx`
- Create: `frontend/src/components/Spinner.tsx`
- Create: `frontend/src/components/CostLine.tsx`

**Step 1: Create theme.ts** — mirror `app/cli/tui/theme.py` values exactly.

```typescript
// frontend/src/theme.ts
// Mirrors app/cli/tui/theme.py — keep in sync!
export const PRIMARY = "#fab283";
export const SECONDARY = "#5c9cf5";
export const ACCENT = "#56b6c2";
export const ACCENT_CYAN = "#56b6c2";
export const ACCENT_MAGENTA = "#bb86fc";
export const SUCCESS = "#7fd88f";
export const WARNING = "#e5c07b";
export const ERROR = "#e06c75";
export const INFO = "#61afef";
export const DIM = "#808080";
export const TEXT = "#e0e0e0";
export const MUTED = "#808080";

export const ICONS: Record<string, string> = {
  input: "\u276F",    // ❯
  output: "\u25CB",   // ○
  tool: "\u2714",     // ✔
  error: "\u2718",    // ✗
  success: "\u2714",
  metrics: "\u25A0",  // ■
  calphad: "\u0394",  // Δ
  validation: "\u25CF",// ●
  results: "\u2261",  // ≡
  plot: "\u25A3",     // ▣
  approval: "\u26A0", // ⚠
  plan: "\u25B7",     // ▷
};

export const BORDERS: Record<string, string> = {
  input: ACCENT_CYAN,
  output: MUTED,
  tool: SUCCESS,
  error: ERROR,
  error_partial: WARNING,
  metrics: SECONDARY,
  calphad: SECONDARY,
  validation_critical: ERROR,
  validation_warning: WARNING,
  validation_info: INFO,
  results: MUTED,
  plot: SUCCESS,
  approval: WARNING,
  plan: ACCENT_MAGENTA,
};
```

**Step 2: Create Prompt.tsx**

```tsx
// frontend/src/components/Prompt.tsx
import React, { useState } from "react";
import { Box, Text } from "ink";
import TextInput from "ink-text-input";
import { ACCENT_CYAN } from "../theme.js";

interface Props {
  onSubmit: (text: string) => void;
}

export function Prompt({ onSubmit }: Props) {
  const [value, setValue] = useState("");

  const handleSubmit = (text: string) => {
    if (text.trim()) {
      onSubmit(text.trim());
      setValue("");
    }
  };

  return (
    <Box>
      <Text color={ACCENT_CYAN} bold>{"\u276F "}</Text>
      <TextInput value={value} onChange={setValue} onSubmit={handleSubmit} />
    </Box>
  );
}
```

**Step 3: Create StreamingText.tsx**

```tsx
// frontend/src/components/StreamingText.tsx
import React from "react";
import { Text } from "ink";

interface Props {
  text: string;
}

export function StreamingText({ text }: Props) {
  if (!text) return null;
  // Simple text render — ink-markdown can be added later for rich formatting
  return <Text>{text}</Text>;
}
```

**Step 4: Create Spinner.tsx**

```tsx
// frontend/src/components/Spinner.tsx
import React from "react";
import { Text, Box } from "ink";
import InkSpinner from "ink-spinner";
import { PRIMARY } from "../theme.js";

interface Props {
  verb: string;
}

export function Spinner({ verb }: Props) {
  return (
    <Box>
      <Text color={PRIMARY}><InkSpinner type="dots" /></Text>
      <Text> {verb}</Text>
    </Box>
  );
}
```

**Step 5: Create CostLine.tsx**

```tsx
// frontend/src/components/CostLine.tsx
import React from "react";
import { Text } from "ink";

interface Props {
  inputTokens: number;
  outputTokens: number;
  turnCost?: number;
  sessionCost?: number;
}

function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

export function CostLine({ inputTokens, outputTokens, turnCost, sessionCost }: Props) {
  const parts = [
    `${formatTokens(inputTokens)} in`,
    `${formatTokens(outputTokens)} out`,
  ];
  if (turnCost !== undefined) {
    parts.push(`$${turnCost.toFixed(4)}`);
  }
  if (sessionCost !== undefined) {
    parts.push(`total: $${sessionCost.toFixed(4)}`);
  }

  return <Text dimColor>{`\u2500 ${parts.join(" \u00B7 ")} \u2500`}</Text>;
}
```

**Step 6: Verify typecheck**

Run: `cd frontend && bun run typecheck`
Expected: No errors.

**Step 7: Commit**

```bash
git add frontend/src/theme.ts frontend/src/components/
git commit -m "feat(frontend): add theme + Prompt, StreamingText, Spinner, CostLine components"
```

---

### Task 11: Create ToolCard, Welcome, ApprovalPrompt, PlanCard, StatusLine

The remaining card components.

**Files:**
- Create: `frontend/src/components/ToolCard.tsx`
- Create: `frontend/src/components/Welcome.tsx`
- Create: `frontend/src/components/ApprovalPrompt.tsx`
- Create: `frontend/src/components/PlanCard.tsx`
- Create: `frontend/src/components/StatusLine.tsx`
- Create: `frontend/src/components/InputCard.tsx`
- Create: `frontend/src/components/SessionList.tsx`

**Step 1: Create ToolCard.tsx** — single component rendering all card types.

```tsx
// frontend/src/components/ToolCard.tsx
import React from "react";
import { Box, Text } from "ink";
import { BORDERS, ICONS, MUTED, DIM } from "../theme.js";

interface Props {
  cardType: string;
  toolName: string;
  elapsedMs: number;
  content: string;
  data: Record<string, any>;
}

function formatElapsed(ms: number): string {
  if (ms >= 2000) return `${(ms / 1000).toFixed(1)}s`;
  if (ms > 0) return `${Math.round(ms)}ms`;
  return "";
}

export function ToolCard({ cardType, toolName, elapsedMs, content, data }: Props) {
  const border = BORDERS[cardType] || MUTED;
  const icon = ICONS[cardType] || "";
  const elapsed = formatElapsed(elapsedMs);

  return (
    <Box borderStyle="round" borderColor={border} paddingX={1} flexDirection="column">
      <Box>
        <Text color={border}>{icon} </Text>
        <Text dimColor>{cardType} </Text>
        {toolName && <Text color={MUTED}>{toolName} </Text>}
        {elapsed && <Text dimColor>{elapsed}</Text>}
      </Box>
      {content && <Text>{content}</Text>}
    </Box>
  );
}
```

**Step 2: Create Welcome.tsx**

```tsx
// frontend/src/components/Welcome.tsx
import React from "react";
import { Box, Text } from "ink";
import { PRIMARY, MUTED, DIM, SECONDARY, SUCCESS } from "../theme.js";

interface Props {
  version: string;
  provider: string;
  capabilities: Record<string, boolean>;
  toolCount: number;
  skillCount: number;
  autoApprove: boolean;
}

export function Welcome({ version, provider, capabilities, toolCount, skillCount, autoApprove }: Props) {
  return (
    <Box flexDirection="column" marginY={1}>
      <Text color={PRIMARY} bold>{"  PRISM"}</Text>
      <Text dimColor>{`  v${version}`}</Text>
      <Text dimColor>{"  AI-Native Autonomous Materials Discovery"}</Text>
      <Box marginTop={1}>
        <Text>{"  "}</Text>
        {provider && <Text color={SECONDARY} bold>{provider}</Text>}
        {provider && <Text dimColor>{" \u2502 "}</Text>}
        {Object.entries(capabilities).map(([name, ok]) => (
          <React.Fragment key={name}>
            <Text dimColor>{name} </Text>
            <Text color={ok ? SUCCESS : MUTED}>{ok ? "\u25CF" : "\u25CB"}</Text>
            <Text>{"  "}</Text>
          </React.Fragment>
        ))}
        <Text dimColor>{`${toolCount} tools`}</Text>
        {skillCount > 0 && <Text dimColor>{` \u2502 ${skillCount} skills`}</Text>}
      </Box>
    </Box>
  );
}
```

**Step 3: Create remaining components** (ApprovalPrompt, PlanCard, StatusLine, InputCard, SessionList) following the same pattern — each mirrors its Rich counterpart. Each is a functional component receiving props from the protocol event params.

Create each file following the ToolCard pattern. See design doc Section 3 for the complete component mapping.

**Step 4: Verify typecheck**

Run: `cd frontend && bun run typecheck`
Expected: No errors.

**Step 5: Commit**

```bash
git add frontend/src/components/
git commit -m "feat(frontend): add ToolCard, Welcome, and remaining card components"
```

---

### Task 12: Create hooks and App root component

Wire everything together: `useBackend` hook, `app.tsx` root, `index.tsx` entry.

**Files:**
- Create: `frontend/src/hooks/useBackend.ts`
- Create: `frontend/src/hooks/useStreaming.ts`
- Create: `frontend/src/hooks/useSession.ts`
- Create: `frontend/src/app.tsx`
- Create: `frontend/src/index.tsx`

**Step 1: Create useBackend.ts**

```typescript
// frontend/src/hooks/useBackend.ts
import { useState, useEffect, useCallback, useRef } from "react";
import { BackendClient, type JsonRpcMessage } from "../bridge/client.js";

interface BackendEvent {
  method: string;
  params: Record<string, any>;
}

export function useBackend(pythonPath: string) {
  const clientRef = useRef<BackendClient | null>(null);
  const [ready, setReady] = useState(false);
  const [events, setEvents] = useState<BackendEvent[]>([]);

  useEffect(() => {
    const client = new BackendClient(pythonPath);
    clientRef.current = client;

    client.on("event", (event: BackendEvent) => {
      setEvents((prev) => [...prev, event]);
    });

    client.request("init", {}).then(() => setReady(true));

    return () => client.destroy();
  }, [pythonPath]);

  const sendMessage = useCallback((text: string) => {
    clientRef.current?.send("input.message", { text });
  }, []);

  const sendCommand = useCallback((command: string) => {
    clientRef.current?.send("input.command", { command });
  }, []);

  const sendPromptResponse = useCallback((promptType: string, response: string) => {
    clientRef.current?.send("input.prompt_response", { prompt_type: promptType, response });
  }, []);

  return { ready, events, sendMessage, sendCommand, sendPromptResponse };
}
```

**Step 2: Create app.tsx**

```tsx
// frontend/src/app.tsx
import React, { useState, useCallback } from "react";
import { Box, Static } from "ink";
import { useBackend } from "./hooks/useBackend.js";
import { Welcome } from "./components/Welcome.js";
import { Prompt } from "./components/Prompt.js";
import { StreamingText } from "./components/StreamingText.js";
import { ToolCard } from "./components/ToolCard.js";
import { CostLine } from "./components/CostLine.js";
import { Spinner } from "./components/Spinner.js";
import { StatusLine } from "./components/StatusLine.js";

interface HistoryItem {
  id: number;
  type: string;
  data: any;
}

interface Props {
  pythonPath: string;
  autoApprove?: boolean;
}

export function App({ pythonPath, autoApprove }: Props) {
  const { ready, events, sendMessage, sendCommand, sendPromptResponse } = useBackend(pythonPath);
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [streamingText, setStreamingText] = useState("");
  const [spinnerVerb, setSpinnerVerb] = useState<string | null>(null);
  const [nextId, setNextId] = useState(0);

  // Process events into history + live state
  React.useEffect(() => {
    if (events.length === 0) return;
    const latest = events[events.length - 1];

    switch (latest.method) {
      case "ui.welcome":
        setHistory((h) => [...h, { id: nextId, type: "welcome", data: latest.params }]);
        setNextId((n) => n + 1);
        break;
      case "ui.text.delta":
        setStreamingText((t) => t + latest.params.text);
        break;
      case "ui.text.flush":
        if (latest.params.text.trim()) {
          setHistory((h) => [...h, { id: nextId, type: "text", data: { text: latest.params.text } }]);
          setNextId((n) => n + 1);
        }
        setStreamingText("");
        break;
      case "ui.tool.start":
        setSpinnerVerb(latest.params.verb);
        break;
      case "ui.card":
        setSpinnerVerb(null);
        setHistory((h) => [...h, { id: nextId, type: "card", data: latest.params }]);
        setNextId((n) => n + 1);
        break;
      case "ui.cost":
        setHistory((h) => [...h, { id: nextId, type: "cost", data: latest.params }]);
        setNextId((n) => n + 1);
        break;
      case "ui.turn.complete":
        setSpinnerVerb(null);
        setStreamingText("");
        break;
    }
  }, [events.length]);

  const handleSubmit = useCallback((text: string) => {
    if (text.startsWith("/")) {
      sendCommand(text);
    } else {
      setHistory((h) => [...h, { id: nextId, type: "input", data: { text } }]);
      setNextId((n) => n + 1);
      sendMessage(text);
    }
  }, [sendMessage, sendCommand, nextId]);

  if (!ready) return <Spinner verb="Starting PRISM..." />;

  return (
    <Box flexDirection="column">
      <Static items={history}>
        {(item) => <HistoryRenderer key={item.id} item={item} />}
      </Static>

      {streamingText && <StreamingText text={streamingText} />}
      {spinnerVerb && <Spinner verb={spinnerVerb} />}

      <Prompt onSubmit={handleSubmit} />
    </Box>
  );
}

function HistoryRenderer({ item }: { item: HistoryItem }) {
  switch (item.type) {
    case "welcome":
      return <Welcome {...item.data} />;
    case "input":
      return (
        <Box>
          <Box borderStyle="round" borderColor="#56b6c2" paddingX={1}>
            <StreamingText text={item.data.text} />
          </Box>
        </Box>
      );
    case "text":
      return <StreamingText text={item.data.text} />;
    case "card":
      return <ToolCard {...item.data} />;
    case "cost":
      return (
        <CostLine
          inputTokens={item.data.input_tokens}
          outputTokens={item.data.output_tokens}
          turnCost={item.data.turn_cost}
          sessionCost={item.data.session_cost}
        />
      );
    default:
      return null;
  }
}
```

**Step 3: Create index.tsx**

```tsx
// frontend/src/index.tsx
#!/usr/bin/env bun
import React from "react";
import { render } from "ink";
import { App } from "./app.js";

const args = process.argv.slice(2);
let pythonPath = "python3";
let autoApprove = false;

for (let i = 0; i < args.length; i++) {
  if (args[i] === "--python" && args[i + 1]) {
    pythonPath = args[i + 1];
    i++;
  }
  if (args[i] === "--auto-approve") {
    autoApprove = true;
  }
}

render(<App pythonPath={pythonPath} autoApprove={autoApprove} />);
```

**Step 4: Verify typecheck**

Run: `cd frontend && bun run typecheck`
Expected: No errors.

**Step 5: Manual smoke test**

Run: `cd frontend && bun run dev -- --python python3`
Expected: PRISM welcome banner appears, prompt works, can type and get responses. (Requires API key configured.)

**Step 6: Commit**

```bash
git add frontend/src/
git commit -m "feat(frontend): add hooks, App root, and index entry point — full Ink REPL"
```

---

## Phase 4: Build & Package

---

### Task 13: Add binary discovery and `--classic` flag

Wire the entry point to detect the compiled binary and add the `--classic` fallback.

**Files:**
- Create: `app/cli/_binary.py`
- Modify: `app/cli/main.py` (add `--classic` option)
- Test: `tests/test_binary_discovery.py`

**Step 1: Write the failing test**

```python
# tests/test_binary_discovery.py
"""Test binary discovery for the compiled Ink TUI."""
from unittest.mock import patch
from pathlib import Path


def test_has_tui_binary_returns_false_when_missing():
    from app.cli._binary import has_tui_binary
    with patch("app.cli._binary.tui_binary_path", return_value=None):
        assert has_tui_binary() is False


def test_has_tui_binary_returns_true_when_present(tmp_path):
    binary = tmp_path / "prism-tui"
    binary.write_text("#!/bin/sh\necho hi")
    binary.chmod(0o755)
    from app.cli._binary import has_tui_binary
    with patch("app.cli._binary._bin_dir", return_value=tmp_path):
        assert has_tui_binary() is True


def test_tui_binary_path_checks_user_override(tmp_path):
    binary = tmp_path / "prism-tui"
    binary.write_text("#!/bin/sh")
    binary.chmod(0o755)
    from app.cli._binary import tui_binary_path
    with patch("app.cli._binary._user_bin_dir", return_value=tmp_path), \
         patch("app.cli._binary._bin_dir", return_value=Path("/nonexistent")):
        result = tui_binary_path()
        assert result == binary
```

**Step 2: Write implementation**

```python
# app/cli/_binary.py
"""Compiled Ink TUI binary discovery."""
import os
from pathlib import Path


def _bin_dir() -> Path:
    """Package-bundled binary location."""
    return Path(__file__).parent.parent / "_bin"


def _user_bin_dir() -> Path:
    """User-installed binary override location."""
    return Path.home() / ".prism" / "bin"


def tui_binary_path() -> Path | None:
    """Find the compiled TUI binary. Returns None if not found."""
    name = "prism-tui"

    # Check package-bundled
    candidate = _bin_dir() / name
    if candidate.exists() and os.access(candidate, os.X_OK):
        return candidate

    # Check user override
    candidate = _user_bin_dir() / name
    if candidate.exists() and os.access(candidate, os.X_OK):
        return candidate

    return None


def has_tui_binary() -> bool:
    """Check if a compiled TUI binary is available."""
    return tui_binary_path() is not None
```

**Step 3: Update main.py** — add `--classic` flag and binary launch.

Add to `cli()` function:
- New option: `@click.option('--classic', is_flag=True, help='Use classic Rich terminal UI')`
- In the `ctx.invoked_subcommand is None` block, check for `--classic` or missing binary

```python
# In the REPL launch section of cli(), replace the existing block with:
from app.cli._binary import has_tui_binary, tui_binary_path

if not classic and has_tui_binary():
    import sys
    binary = tui_binary_path()
    args = [str(binary), "--python", sys.executable]
    if dangerously_accept_all:
        args.append("--auto-approve")
    if resume:
        args.extend(["--resume", resume])
    os.execvp(str(binary), args)
else:
    # Existing Rich REPL launch code
    backend = create_backend()
    repl = AgentREPL(backend=backend, enable_mcp=not no_mcp,
                     auto_approve=dangerously_accept_all)
    if resume:
        repl.load_session(resume)
    repl.run()
```

**Step 4: Run tests**

Run: `python3 -m pytest tests/test_binary_discovery.py -v`
Expected: PASS

Run: `python3 -m pytest tests/ -x -q --timeout=30`
Expected: All pass. The binary doesn't exist in dev, so Rich REPL is always used.

**Step 5: Commit**

```bash
git add app/cli/_binary.py app/cli/main.py
git add -f tests/test_binary_discovery.py
git commit -m "feat(cli): add --classic flag and Ink binary discovery"
```

---

### Task 14: CI workflow for building platform wheels

GitHub Actions workflow that compiles the Ink binary and produces platform-specific wheels.

**Files:**
- Create: `.github/workflows/build-frontend.yml`

**Step 1: Write the workflow**

```yaml
# .github/workflows/build-frontend.yml
name: Build Frontend

on:
  push:
    tags: ["v*"]
  workflow_dispatch:

jobs:
  build-frontend:
    strategy:
      matrix:
        include:
          - os: macos-14
            target: bun-darwin-arm64
            wheel_plat: macosx_11_0_arm64
          - os: macos-13
            target: bun-darwin-x64
            wheel_plat: macosx_11_0_x86_64
          - os: ubuntu-latest
            target: bun-linux-x64
            wheel_plat: manylinux_2_17_x86_64

    runs-on: ${{ matrix.os }}

    steps:
      - uses: actions/checkout@v4

      - uses: oven-sh/setup-bun@v2

      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"

      - name: Install frontend deps
        run: cd frontend && bun install

      - name: Compile binary
        run: |
          cd frontend
          bun build src/index.tsx --compile \
            --target=${{ matrix.target }} \
            --outfile ../app/_bin/prism-tui

      - name: Build wheel
        run: |
          pip install build wheel
          python -m build --wheel

      - uses: actions/upload-artifact@v4
        with:
          name: wheel-${{ matrix.wheel_plat }}
          path: dist/*.whl

  build-pure:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"
      - name: Build pure Python wheel
        run: |
          rm -rf app/_bin/
          pip install build
          python -m build --wheel
      - uses: actions/upload-artifact@v4
        with:
          name: wheel-pure
          path: dist/*.whl
```

**Step 2: Commit**

```bash
mkdir -p .github/workflows
git add .github/workflows/build-frontend.yml
git commit -m "ci: add build-frontend workflow for platform wheels"
```

---

### Task 15: Final integration — compile and smoke test locally

Build the binary locally and verify the full `prism` → Ink → Python roundtrip.

**Step 1: Build locally**

Run: `cd frontend && bun run build -- darwin-arm64` (adjust for your platform)

**Step 2: Copy binary**

Run: `mkdir -p app/_bin && cp frontend/dist/prism-tui-darwin-arm64 app/_bin/prism-tui && chmod +x app/_bin/prism-tui`

**Step 3: Smoke test — Ink mode**

Run: `python3 -m app.cli` (should launch Ink TUI)
Verify: Welcome banner appears, prompt works, streaming text works.

**Step 4: Smoke test — classic mode**

Run: `python3 -m app.cli --classic` (should launch Rich REPL)
Verify: Same behavior as before, identical visual output.

**Step 5: Clean up binary (don't commit)**

Run: `rm -rf app/_bin/prism-tui`

**Step 6: Run full test suite**

Run: `python3 -m pytest tests/ -x -q --timeout=30`
Expected: All 890+ tests pass.

**Step 7: Final commit**

```bash
git add -A
git commit -m "feat: complete Ink frontend — full parity with Rich REPL"
```

---

## Phase 5: HTTP+SSE (Future — NOT in scope)

Deferred. Architecture slot is designed. When ready:
- Add `app/backend/http_server.py` (FastAPI + SSE)
- Same UIEmitter, different transport
- Web frontend would be a third renderer

---

## Summary

| Phase | Tasks | Creates | Key Validation |
|---|---|---|---|
| 1: Extract UIEmitter | 1-4 | `app/backend/` (protocol, emitter) | All 873 tests pass, Rich REPL identical |
| 2: stdio Server | 5-7 | `server.py`, `__main__.py` | JSON-RPC roundtrip works |
| 3: Ink Frontend | 8-12 | `frontend/` (full component tree) | Typecheck passes, smoke test works |
| 4: Build & Package | 13-15 | `_binary.py`, CI workflow, `--classic` flag | Both modes work, full test suite passes |
| 5: HTTP+SSE | — | Future | — |

**Total new tests:** ~30 (protocol: 8, emitter: 10, server: 4, integration: 2, binary: 3, stream refactor: 2)

**Critical invariant:** Every change in either frontend must also exist in the other. UIEmitter is the shared brain — frontends are dumb renderers.
