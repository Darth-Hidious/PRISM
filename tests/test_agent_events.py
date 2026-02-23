"""Tests for agent event types and backend interface."""
import pytest
from app.agent.events import AgentResponse, ToolCallEvent


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


class TestBackendABC:
    def test_cannot_instantiate_directly(self):
        from app.agent.backends.base import Backend
        with pytest.raises(TypeError):
            Backend()
