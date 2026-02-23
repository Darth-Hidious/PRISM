"""Tests for agent event types and backend interface."""
import pytest
from app.agent.events import (
    AgentResponse, ToolCallEvent,
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
)


class TestToolCallEvent:
    def test_creation(self):
        event = ToolCallEvent(tool_name="search", tool_args={"query": "silicon"}, call_id="call_123")
        assert event.tool_name == "search"
        assert event.tool_args == {"query": "silicon"}
        assert event.call_id == "call_123"


class TestAgentResponse:
    def test_text_only(self):
        resp = AgentResponse(text="Hello")
        assert resp.text == "Hello"
        assert resp.has_tool_calls is False
        assert resp.tool_calls == []

    def test_with_tool_calls(self):
        calls = [ToolCallEvent(tool_name="search", tool_args={"q": "Si"}, call_id="c1")]
        resp = AgentResponse(text="Searching...", tool_calls=calls)
        assert resp.has_tool_calls is True
        assert len(resp.tool_calls) == 1

    def test_empty_response(self):
        resp = AgentResponse()
        assert resp.text is None
        assert resp.has_tool_calls is False


class TestStreamEvents:
    def test_text_delta(self):
        delta = TextDelta(text="Hello")
        assert delta.text == "Hello"

    def test_tool_call_start(self):
        event = ToolCallStart(tool_name="search_optimade", call_id="c1")
        assert event.tool_name == "search_optimade"
        assert event.call_id == "c1"

    def test_tool_call_result(self):
        event = ToolCallResult(call_id="c1", tool_name="search_optimade", result={"count": 5}, summary="Found 5 materials")
        assert event.call_id == "c1"
        assert event.tool_name == "search_optimade"
        assert event.result == {"count": 5}
        assert event.summary == "Found 5 materials"

    def test_turn_complete_defaults(self):
        event = TurnComplete()
        assert event.text is None
        assert event.has_more is False

    def test_turn_complete_with_values(self):
        event = TurnComplete(text="Done.", has_more=True)
        assert event.text == "Done."
        assert event.has_more is True


class TestBackendABC:
    def test_cannot_instantiate_directly(self):
        from app.agent.backends.base import Backend
        with pytest.raises(TypeError):
            Backend()


class TestBackendStreamFallback:
    def test_complete_stream_fallback(self):
        """Default complete_stream() wraps complete() in TextDelta + TurnComplete."""
        from app.agent.backends.base import Backend
        from app.agent.events import AgentResponse

        class StubBackend(Backend):
            def complete(self, messages, tools, system_prompt=None):
                return AgentResponse(text="Hello world")

        backend = StubBackend()
        events = list(backend.complete_stream(messages=[], tools=[]))
        assert len(events) == 2
        assert isinstance(events[0], TextDelta)
        assert events[0].text == "Hello world"
        assert isinstance(events[1], TurnComplete)
        assert events[1].text == "Hello world"
        assert events[1].has_more is False
        assert backend._last_stream_response.text == "Hello world"

    def test_complete_stream_fallback_with_tool_calls(self):
        from app.agent.backends.base import Backend
        from app.agent.events import AgentResponse, ToolCallEvent

        class StubBackend(Backend):
            def complete(self, messages, tools, system_prompt=None):
                return AgentResponse(tool_calls=[ToolCallEvent(tool_name="t", tool_args={}, call_id="c1")])

        backend = StubBackend()
        events = list(backend.complete_stream(messages=[], tools=[]))
        assert len(events) == 1  # no TextDelta for empty text
        assert isinstance(events[0], TurnComplete)
        assert events[0].has_more is True
