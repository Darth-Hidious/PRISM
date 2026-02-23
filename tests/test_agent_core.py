"""Tests for AgentCore TAOR loop."""
import pytest
from unittest.mock import MagicMock
from app.agent.core import AgentCore
from app.agent.events import AgentResponse, ToolCallEvent
from app.tools.base import Tool, ToolRegistry


class TestAgentCore:
    def _make_registry_with_tool(self):
        registry = ToolRegistry()
        registry.register(Tool(name="add", description="Add numbers",
            input_schema={"type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}}},
            func=lambda **kw: {"sum": kw["a"] + kw["b"]}))
        return registry

    def test_simple_text_response(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="Silicon is a semiconductor.")
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        result = agent.process("Tell me about silicon")
        assert result == "Silicon is a semiconductor."
        assert len(agent.history) == 2

    def test_tool_call_then_text(self):
        registry = self._make_registry_with_tool()
        backend = MagicMock()
        backend.complete.side_effect = [
            AgentResponse(text=None, tool_calls=[ToolCallEvent(tool_name="add", tool_args={"a": 2, "b": 3}, call_id="c1")]),
            AgentResponse(text="The sum is 5."),
        ]
        agent = AgentCore(backend=backend, tools=registry)
        result = agent.process("What is 2+3?")
        assert result == "The sum is 5."
        assert backend.complete.call_count == 2
        assert len(agent.history) == 4

    def test_multiple_tool_calls_in_one_response(self):
        registry = ToolRegistry()
        registry.register(Tool(name="a", description="A", input_schema={}, func=lambda **kw: {"v": 1}))
        registry.register(Tool(name="b", description="B", input_schema={}, func=lambda **kw: {"v": 2}))
        backend = MagicMock()
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="a", tool_args={}, call_id="c1"),
                ToolCallEvent(tool_name="b", tool_args={}, call_id="c2"),
            ]),
            AgentResponse(text="Done."),
        ]
        agent = AgentCore(backend=backend, tools=registry)
        result = agent.process("Do both")
        assert result == "Done."

    def test_system_prompt_passed_to_backend(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="ok")
        agent = AgentCore(backend=backend, tools=ToolRegistry(), system_prompt="You are PRISM.")
        agent.process("hi")
        call_kwargs = backend.complete.call_args
        assert call_kwargs.kwargs.get("system_prompt") == "You are PRISM."

    def test_max_iterations_safety(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(tool_calls=[ToolCallEvent(tool_name="x", tool_args={}, call_id="c")])
        registry = ToolRegistry()
        registry.register(Tool(name="x", description="X", input_schema={}, func=lambda **kw: {}))
        agent = AgentCore(backend=backend, tools=registry, max_iterations=3)
        result = agent.process("loop forever")
        assert backend.complete.call_count == 3
        assert "max iterations" in result.lower()
