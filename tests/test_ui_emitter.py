"""Tests for UIEmitter — shared presentation logic layer.

Each test mocks agent.process_stream() to yield controlled agent events
and verifies that UIEmitter.process() yields the correct ui.* protocol events.
"""
import pytest
from unittest.mock import MagicMock, patch

from app.agent.events import (
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest, UsageInfo,
)
from app.backend.protocol import make_event


def _make_agent(stream_events):
    """Create a mock agent whose process_stream yields the given events."""
    agent = MagicMock()
    agent.process_stream.return_value = iter(stream_events)
    agent.tools.list_tools.return_value = [MagicMock()] * 5
    return agent


def _collect(emitter, user_input="hello"):
    """Collect all events from emitter.process() into a list."""
    return list(emitter.process(user_input))


class TestUIEmitter:

    def test_emitter_yields_text_delta(self):
        from app.backend.ui_emitter import UIEmitter

        agent = _make_agent([
            TextDelta(text="Hello "),
            TextDelta(text="world"),
            TurnComplete(text="Hello world", usage=UsageInfo(input_tokens=10, output_tokens=5), estimated_cost=0.001),
        ])
        emitter = UIEmitter(agent)
        events = _collect(emitter)

        deltas = [e for e in events if e["method"] == "ui.text.delta"]
        assert len(deltas) == 2
        assert deltas[0]["params"]["text"] == "Hello "
        assert deltas[1]["params"]["text"] == "world"

    def test_emitter_yields_tool_start_and_card(self):
        from app.backend.ui_emitter import UIEmitter

        agent = _make_agent([
            ToolCallStart(tool_name="search_materials", call_id="c1"),
            ToolCallResult(
                call_id="c1", tool_name="search_materials",
                result={"results": [1, 2, 3, 4], "count": 4},
                summary="search_materials: 4 results",
            ),
            TurnComplete(text=None, usage=UsageInfo(input_tokens=100, output_tokens=50), estimated_cost=0.01),
        ])
        emitter = UIEmitter(agent)
        events = _collect(emitter)

        starts = [e for e in events if e["method"] == "ui.tool.start"]
        assert len(starts) == 1
        assert starts[0]["params"]["tool_name"] == "search_materials"
        assert starts[0]["params"]["call_id"] == "c1"
        assert starts[0]["params"]["verb"]  # should have a verb string

        cards = [e for e in events if e["method"] == "ui.card"]
        assert len(cards) == 1
        assert cards[0]["params"]["tool_name"] == "search_materials"

    def test_emitter_yields_cost_and_turn_complete(self):
        from app.backend.ui_emitter import UIEmitter

        agent = _make_agent([
            TextDelta(text="Done."),
            TurnComplete(
                text="Done.",
                usage=UsageInfo(input_tokens=200, output_tokens=100),
                estimated_cost=0.005,
            ),
        ])
        emitter = UIEmitter(agent)
        events = _collect(emitter)

        costs = [e for e in events if e["method"] == "ui.cost"]
        assert len(costs) == 1
        assert costs[0]["params"]["input_tokens"] == 200
        assert costs[0]["params"]["output_tokens"] == 100
        assert costs[0]["params"]["turn_cost"] == 0.005
        assert costs[0]["params"]["session_cost"] == 0.005

        completes = [e for e in events if e["method"] == "ui.turn.complete"]
        assert len(completes) == 1

    def test_emitter_accumulates_session_cost(self):
        from app.backend.ui_emitter import UIEmitter

        agent = MagicMock()
        agent.tools.list_tools.return_value = []

        # First turn
        agent.process_stream.return_value = iter([
            TextDelta(text="Turn 1"),
            TurnComplete(text="Turn 1", usage=UsageInfo(input_tokens=10, output_tokens=5), estimated_cost=0.002),
        ])
        emitter = UIEmitter(agent)
        events1 = _collect(emitter, "first")

        cost1 = [e for e in events1 if e["method"] == "ui.cost"][0]
        assert cost1["params"]["session_cost"] == 0.002

        # Second turn
        agent.process_stream.return_value = iter([
            TextDelta(text="Turn 2"),
            TurnComplete(text="Turn 2", usage=UsageInfo(input_tokens=20, output_tokens=10), estimated_cost=0.003),
        ])
        events2 = _collect(emitter, "second")

        cost2 = [e for e in events2 if e["method"] == "ui.cost"][0]
        assert cost2["params"]["session_cost"] == pytest.approx(0.005)

    def test_emitter_flushes_text_on_tool_start(self):
        """When a ToolCallStart arrives, any accumulated text should be flushed first."""
        from app.backend.ui_emitter import UIEmitter

        agent = _make_agent([
            TextDelta(text="Let me search"),
            TextDelta(text=" for that."),
            ToolCallStart(tool_name="search_materials", call_id="c1"),
            ToolCallResult(
                call_id="c1", tool_name="search_materials",
                result={"count": 1}, summary="done",
            ),
            TurnComplete(text=None, usage=UsageInfo(input_tokens=10, output_tokens=5), estimated_cost=0.001),
        ])
        emitter = UIEmitter(agent)
        events = _collect(emitter)

        # Find the flush that should appear before tool.start
        methods = [e["method"] for e in events]
        flush_idx = methods.index("ui.text.flush")
        start_idx = methods.index("ui.tool.start")
        assert flush_idx < start_idx
        flush = events[flush_idx]
        assert flush["params"]["text"] == "Let me search for that."

    def test_emitter_detects_plan(self):
        from app.backend.ui_emitter import UIEmitter

        agent = _make_agent([
            TextDelta(text="I'll create a plan.\n<plan>\n"),
            TextDelta(text="1. Search databases\n"),
            TextDelta(text="2. Predict properties\n</plan>\nContinuing"),
            TurnComplete(text="done", usage=UsageInfo(input_tokens=10, output_tokens=5), estimated_cost=0.001),
        ])
        emitter = UIEmitter(agent)
        events = _collect(emitter)

        cards = [e for e in events if e["method"] == "ui.card"]
        plan_cards = [c for c in cards if c["params"]["card_type"] == "plan"]
        assert len(plan_cards) == 1
        assert "Search databases" in plan_cards[0]["params"]["content"]

        prompts = [e for e in events if e["method"] == "ui.prompt"]
        plan_prompts = [p for p in prompts if p["params"]["prompt_type"] == "plan_confirm"]
        assert len(plan_prompts) == 1

    def test_emitter_welcome(self):
        from app.backend.ui_emitter import UIEmitter

        agent = MagicMock()
        agent.tools.list_tools.return_value = [MagicMock()] * 12

        emitter = UIEmitter(agent, auto_approve=True)

        with patch("app.backend.ui_emitter.detect_capabilities", return_value={"ML": True, "CALPHAD": False}), \
             patch("app.backend.ui_emitter._detect_provider", return_value="Claude"):
            event = emitter.welcome()

        assert event["method"] == "ui.welcome"
        assert event["params"]["version"]
        assert event["params"]["provider"] == "Claude"
        assert event["params"]["capabilities"] == {"ML": True, "CALPHAD": False}
        assert event["params"]["tool_count"] == 12
        assert event["params"]["auto_approve"] is True

    def test_emitter_detects_error_card_type(self):
        from app.backend.ui_emitter import UIEmitter

        agent = _make_agent([
            ToolCallStart(tool_name="search_materials", call_id="c1"),
            ToolCallResult(
                call_id="c1", tool_name="search_materials",
                result={"error": "API timeout"},
                summary="search_materials: error",
            ),
            TurnComplete(text=None, usage=UsageInfo(input_tokens=10, output_tokens=5), estimated_cost=0.001),
        ])
        emitter = UIEmitter(agent)
        events = _collect(emitter)

        cards = [e for e in events if e["method"] == "ui.card"]
        assert len(cards) == 1
        assert cards[0]["params"]["card_type"] == "error"

    def test_emitter_detects_metrics_card_type(self):
        from app.backend.ui_emitter import UIEmitter

        agent = _make_agent([
            ToolCallStart(tool_name="predict_property", call_id="c1"),
            ToolCallResult(
                call_id="c1", tool_name="predict_property",
                result={"metrics": {"mae": 0.1, "r2": 0.95}, "algorithm": "RF"},
                summary="predict_property: completed",
            ),
            TurnComplete(text=None, usage=UsageInfo(input_tokens=10, output_tokens=5), estimated_cost=0.001),
        ])
        emitter = UIEmitter(agent)
        events = _collect(emitter)

        cards = [e for e in events if e["method"] == "ui.card"]
        assert len(cards) == 1
        assert cards[0]["params"]["card_type"] == "metrics"

    def test_emitter_yields_approval_prompt(self):
        """ToolApprovalRequest should yield a ui.prompt with prompt_type='approval'."""
        from app.backend.ui_emitter import UIEmitter

        agent = _make_agent([
            ToolCallStart(tool_name="submit_hpc_job", call_id="c1"),
            ToolApprovalRequest(
                tool_name="submit_hpc_job",
                tool_args={"nodes": 4, "walltime": "2h"},
                call_id="c1",
            ),
            ToolCallResult(
                call_id="c1", tool_name="submit_hpc_job",
                result={"job_id": "12345"}, summary="submitted",
            ),
            TurnComplete(text=None, usage=UsageInfo(input_tokens=10, output_tokens=5), estimated_cost=0.001),
        ])
        emitter = UIEmitter(agent, auto_approve=False)
        events = _collect(emitter)

        prompts = [e for e in events if e["method"] == "ui.prompt"]
        approval_prompts = [p for p in prompts if p["params"]["prompt_type"] == "approval"]
        assert len(approval_prompts) == 1
        assert approval_prompts[0]["params"]["tool_name"] == "submit_hpc_job"
        assert approval_prompts[0]["params"]["tool_args"] == {"nodes": 4, "walltime": "2h"}
