"""Tests for feedback loops (Phase F-5)."""
import pytest
from unittest.mock import MagicMock
from app.agent.core import AgentCore
from app.agent.events import AgentResponse, ToolCallEvent
from app.tools.base import Tool, ToolRegistry


def _make_agent_with_tool(tool_name, tool_func, tool_approval=False):
    """Build an agent with a single tool and a mock backend."""
    registry = ToolRegistry()
    registry.register(Tool(
        name=tool_name, description="test", input_schema={},
        func=tool_func, requires_approval=tool_approval,
    ))
    backend = MagicMock()
    agent = AgentCore(backend=backend, tools=registry, auto_approve=True)
    return agent, backend


class TestPostToolHook:
    def test_validation_feedback_injected(self):
        """After validate_dataset, a system message is injected."""
        agent, backend = _make_agent_with_tool(
            "validate_dataset",
            lambda **kw: {"summary": "3 outliers in band_gap", "total_findings": 3},
        )
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="validate_dataset", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Validation done."),
        ]
        agent.process("validate")
        system_msgs = [m for m in agent.history if m.get("role") == "system"]
        assert len(system_msgs) == 1
        assert "Validation feedback" in system_msgs[0]["content"]
        assert "3 outliers" in system_msgs[0]["content"]

    def test_review_feedback_injected(self):
        """After review_dataset, quality score is injected."""
        agent, backend = _make_agent_with_tool(
            "review_dataset",
            lambda **kw: {"quality_score": 0.85, "review_prompt": "Review this dataset..."},
        )
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="review_dataset", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Review done."),
        ]
        agent.process("review")
        system_msgs = [m for m in agent.history if m.get("role") == "system"]
        assert len(system_msgs) == 1
        assert "Review feedback" in system_msgs[0]["content"]
        assert "0.85" in system_msgs[0]["content"]

    def test_calphad_feedback_injected(self):
        """After analyze_phases, phases info is injected."""
        agent, backend = _make_agent_with_tool(
            "analyze_phases",
            lambda **kw: {"phases": ["BCC", "FCC"], "summary": "Two stable phases found."},
        )
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="analyze_phases", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Analysis done."),
        ]
        agent.process("analyze")
        system_msgs = [m for m in agent.history if m.get("role") == "system"]
        assert len(system_msgs) == 1
        assert "CALPHAD feedback" in system_msgs[0]["content"]

    def test_no_feedback_for_safe_tools(self):
        """Regular tools don't trigger feedback injection."""
        agent, backend = _make_agent_with_tool(
            "search_materials",
            lambda **kw: {"count": 5, "results": []},
        )
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="search_materials", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Found 5."),
        ]
        agent.process("search")
        system_msgs = [m for m in agent.history if m.get("role") == "system"]
        assert len(system_msgs) == 0

    def test_no_feedback_on_error(self):
        """Tools returning errors don't trigger feedback."""
        agent, backend = _make_agent_with_tool(
            "validate_dataset",
            lambda **kw: {"error": "Dataset not found"},
        )
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="validate_dataset", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Error."),
        ]
        agent.process("validate")
        system_msgs = [m for m in agent.history if m.get("role") == "system"]
        assert len(system_msgs) == 0
