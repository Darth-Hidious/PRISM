"""Tests for doom loop detection."""
import json
from unittest.mock import MagicMock
from app.agent.core import AgentCore
from app.agent.events import AgentResponse, ToolCallEvent, TurnComplete, ToolCallResult
from app.tools.base import Tool, ToolRegistry


def _make_failing_tool():
    registry = ToolRegistry()
    registry.register(Tool(
        name="search", description="Search", input_schema={},
        func=lambda **kw: (_ for _ in ()).throw(RuntimeError("API timeout")),
    ))
    return registry


class TestCheckDoomLoop:
    def _make_agent(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="done")
        return AgentCore(backend=backend, tools=ToolRegistry())

    def test_no_doom_loop_on_success(self):
        agent = self._make_agent()
        msg = agent._check_doom_loop("search", {"q": "Fe"}, {"count": 5})
        assert msg is None

    def test_no_doom_loop_under_threshold(self):
        agent = self._make_agent()
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        msg = agent._check_doom_loop("search", {"q": "O"}, {"error": "timeout"})
        assert msg is None

    def test_doom_loop_detected_after_3_same_errors(self):
        agent = self._make_agent()
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        msg = agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        assert msg is not None
        assert "DOOM LOOP" in msg

    def test_doom_loop_resets_after_success(self):
        agent = self._make_agent()
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        agent._check_doom_loop("search", {"q": "Fe"}, {"count": 5})  # success
        msg = agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        assert msg is None

    def test_different_tools_independent(self):
        agent = self._make_agent()
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        agent._check_doom_loop("predict", {"formula": "Fe"}, {"error": "timeout"})
        msg = agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        assert msg is not None


class TestDoomLoopInTAORLoop:
    def test_doom_loop_injects_system_message(self):
        registry = _make_failing_tool()
        backend = MagicMock()
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[ToolCallEvent(tool_name="search", tool_args={}, call_id="c1")]),
            AgentResponse(tool_calls=[ToolCallEvent(tool_name="search", tool_args={}, call_id="c2")]),
            AgentResponse(tool_calls=[ToolCallEvent(tool_name="search", tool_args={}, call_id="c3")]),
            AgentResponse(text="I'll try something else."),
        ]
        agent = AgentCore(backend=backend, tools=registry, max_iterations=5)
        result = agent.process("search for iron")
        system_msgs = [m for m in agent.history if m.get("role") == "system" and "DOOM LOOP" in m.get("content", "")]
        assert len(system_msgs) >= 1

    def test_reset_clears_recent_calls(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="done")
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        agent.reset()
        msg = agent._check_doom_loop("search", {"q": "Fe"}, {"error": "timeout"})
        assert msg is None  # only 1 after reset, not 3
