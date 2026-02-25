"""Tests for AgentCore TAOR loop."""
import pytest
from unittest.mock import MagicMock
from app.agent.core import AgentCore
from app.agent.events import (
    AgentResponse, ToolCallEvent,
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
)
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
        # System prompt starts with the provided base, with capabilities appended
        assert call_kwargs.kwargs.get("system_prompt").startswith("You are PRISM.")

    def test_max_iterations_safety(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(tool_calls=[ToolCallEvent(tool_name="x", tool_args={}, call_id="c")])
        registry = ToolRegistry()
        registry.register(Tool(name="x", description="X", input_schema={}, func=lambda **kw: {}))
        agent = AgentCore(backend=backend, tools=registry, max_iterations=3)
        result = agent.process("loop forever")
        assert backend.complete.call_count == 3
        assert "max iterations" in result.lower()


class TestAgentCoreStream:
    def _make_registry_with_tool(self):
        registry = ToolRegistry()
        registry.register(Tool(name="add", description="Add numbers",
            input_schema={"type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}}},
            func=lambda **kw: {"sum": kw["a"] + kw["b"]}))
        return registry

    def _make_streaming_backend(self, responses):
        """Build a mock backend that yields streaming events from a list of AgentResponses."""
        backend = MagicMock()
        call_count = [0]

        def fake_stream(messages, tools, system_prompt=None):
            idx = call_count[0]
            call_count[0] += 1
            resp = responses[idx]
            backend._last_stream_response = resp
            if resp.text:
                yield TextDelta(text=resp.text)
            for tc in resp.tool_calls:
                yield ToolCallStart(tool_name=tc.tool_name, call_id=tc.call_id)
            yield TurnComplete(text=resp.text, has_more=resp.has_tool_calls)

        backend.complete_stream = fake_stream
        return backend

    def test_simple_text_stream(self):
        backend = self._make_streaming_backend([AgentResponse(text="Hello")])
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        events = list(agent.process_stream("hi"))
        types = [type(e).__name__ for e in events]
        assert "TextDelta" in types
        assert "TurnComplete" in types
        assert events[-1].has_more is False

    def test_tool_call_then_text_stream(self):
        registry = self._make_registry_with_tool()
        responses = [
            AgentResponse(tool_calls=[ToolCallEvent(tool_name="add", tool_args={"a": 2, "b": 3}, call_id="c1")]),
            AgentResponse(text="The sum is 5."),
        ]
        backend = self._make_streaming_backend(responses)
        agent = AgentCore(backend=backend, tools=registry)
        events = list(agent.process_stream("What is 2+3?"))
        types = [type(e).__name__ for e in events]
        assert "ToolCallStart" in types
        assert "ToolCallResult" in types
        assert "TurnComplete" in types
        tool_result = next(e for e in events if isinstance(e, ToolCallResult))
        assert tool_result.result == {"sum": 5}
        assert "add" in tool_result.summary

    def test_summarize_tool_result_error(self):
        summary = AgentCore._summarize_tool_result("search", {"error": "API timeout"})
        assert "error" in summary

    def test_summarize_tool_result_count(self):
        summary = AgentCore._summarize_tool_result("search", {"count": 42})
        assert "42" in summary

    def test_summarize_tool_result_filename(self):
        summary = AgentCore._summarize_tool_result("export", {"filename": "out.csv"})
        assert "out.csv" in summary
