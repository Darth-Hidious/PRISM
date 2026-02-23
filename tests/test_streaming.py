"""Streaming integration test: AgentCore.process_stream() with mock backend + real tool."""
import pytest
from unittest.mock import MagicMock
from app.agent.core import AgentCore
from app.agent.events import (
    AgentResponse, ToolCallEvent,
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
)
from app.tools.base import Tool, ToolRegistry


class TestStreamingIntegration:
    """Wire AgentCore.process_stream() with a mock streaming backend and a real tool,
    verifying the full event sequence end-to-end."""

    def _make_registry(self):
        registry = ToolRegistry()
        registry.register(Tool(
            name="search_elements",
            description="Search for elements",
            input_schema={"type": "object", "properties": {"element": {"type": "string"}}, "required": ["element"]},
            func=lambda **kw: {"results": [{"id": "mp-1", "formula": kw["element"]}], "count": 1},
        ))
        return registry

    def _make_streaming_backend(self, responses):
        """Create a mock backend that yields streaming events from AgentResponse list."""
        backend = MagicMock()
        call_count = [0]

        def fake_complete_stream(messages, tools, system_prompt=None):
            idx = call_count[0]
            call_count[0] += 1
            resp = responses[idx]
            backend._last_stream_response = resp
            if resp.text:
                # Simulate chunked text delivery
                words = resp.text.split()
                for word in words:
                    yield TextDelta(text=word + " ")
            for tc in resp.tool_calls:
                yield ToolCallStart(tool_name=tc.tool_name, call_id=tc.call_id)
            yield TurnComplete(text=resp.text, has_more=resp.has_tool_calls)

        backend.complete_stream = fake_complete_stream
        return backend

    def test_full_stream_sequence_with_tool(self):
        """Verify: TextDelta* -> ToolCallStart -> ToolCallResult -> TextDelta* -> TurnComplete."""
        registry = self._make_registry()
        responses = [
            AgentResponse(
                text="Let me search",
                tool_calls=[ToolCallEvent(tool_name="search_elements", tool_args={"element": "Si"}, call_id="c1")],
            ),
            AgentResponse(text="Found 1 result for Silicon."),
        ]
        backend = self._make_streaming_backend(responses)
        agent = AgentCore(backend=backend, tools=registry)

        events = list(agent.process_stream("Find silicon materials"))

        # Check event types in order
        event_types = [type(e).__name__ for e in events]

        # Should have text deltas, tool call start, tool call result, more text deltas, turn complete
        assert "TextDelta" in event_types
        assert "ToolCallStart" in event_types
        assert "ToolCallResult" in event_types
        assert "TurnComplete" in event_types

        # ToolCallStart should come before ToolCallResult
        start_idx = event_types.index("ToolCallStart")
        result_idx = event_types.index("ToolCallResult")
        assert start_idx < result_idx

        # ToolCallResult should contain real tool output
        tool_result_event = events[result_idx]
        assert tool_result_event.tool_name == "search_elements"
        assert tool_result_event.result["count"] == 1
        assert "1" in tool_result_event.summary

        # TurnComplete should be last and not has_more
        assert event_types[-1] == "TurnComplete"
        assert events[-1].has_more is False

        # History should contain the full conversation
        assert len(agent.history) >= 4  # user, tool_calls, tool_result, assistant

    def test_text_only_stream(self):
        """Simple text response with no tool calls."""
        backend = self._make_streaming_backend([AgentResponse(text="Hello there!")])
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        events = list(agent.process_stream("Hi"))
        event_types = [type(e).__name__ for e in events]
        assert "TextDelta" in event_types
        assert "TurnComplete" in event_types
        assert "ToolCallStart" not in event_types
        assert "ToolCallResult" not in event_types

    def test_multiple_tool_calls_stream(self):
        """Multiple tool calls in one response."""
        registry = ToolRegistry()
        registry.register(Tool(name="a", description="A", input_schema={}, func=lambda **kw: {"v": 1}))
        registry.register(Tool(name="b", description="B", input_schema={}, func=lambda **kw: {"v": 2}))
        responses = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="a", tool_args={}, call_id="c1"),
                ToolCallEvent(tool_name="b", tool_args={}, call_id="c2"),
            ]),
            AgentResponse(text="Both done."),
        ]
        backend = self._make_streaming_backend(responses)
        agent = AgentCore(backend=backend, tools=registry)
        events = list(agent.process_stream("Do both"))
        tool_results = [e for e in events if isinstance(e, ToolCallResult)]
        assert len(tool_results) == 2
        assert {tr.tool_name for tr in tool_results} == {"a", "b"}

    def test_tool_error_stream(self):
        """Tool that raises an exception produces error ToolCallResult."""
        registry = ToolRegistry()
        registry.register(Tool(name="fail", description="Fails", input_schema={}, func=lambda **kw: (_ for _ in ()).throw(RuntimeError("boom"))))
        responses = [
            AgentResponse(tool_calls=[ToolCallEvent(tool_name="fail", tool_args={}, call_id="c1")]),
            AgentResponse(text="Tool failed."),
        ]
        backend = self._make_streaming_backend(responses)
        agent = AgentCore(backend=backend, tools=registry)
        events = list(agent.process_stream("Try this"))
        tool_result = next(e for e in events if isinstance(e, ToolCallResult))
        assert "error" in tool_result.result
        assert "error" in tool_result.summary
