"""Tests for tool consent / approval gates (Phase F-2)."""
import pytest
from unittest.mock import MagicMock
from app.tools.base import Tool, ToolRegistry
from app.agent.core import AgentCore
from app.agent.events import (
    AgentResponse, ToolCallEvent,
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest, ToolApprovalResponse,
)


def _make_registry():
    """Build a registry with one safe and one approval-required tool."""
    registry = ToolRegistry()
    registry.register(Tool(
        name="safe_search", description="Search", input_schema={},
        func=lambda **kw: {"count": 5},
    ))
    registry.register(Tool(
        name="expensive_sim", description="Run simulation", input_schema={},
        func=lambda **kw: {"status": "done"},
        requires_approval=True,
    ))
    return registry


class TestToolApprovalField:
    def test_default_no_approval(self):
        t = Tool(name="x", description="x", input_schema={}, func=lambda **kw: {})
        assert t.requires_approval is False

    def test_explicit_approval(self):
        t = Tool(name="x", description="x", input_schema={}, func=lambda **kw: {},
                 requires_approval=True)
        assert t.requires_approval is True


class TestConsentInProcess:
    def test_auto_approve_default(self):
        """With auto_approve=True (default), expensive tools run without callback."""
        registry = _make_registry()
        backend = MagicMock()
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="expensive_sim", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Simulation complete."),
        ]
        agent = AgentCore(backend=backend, tools=registry, auto_approve=True)
        result = agent.process("run sim")
        assert result == "Simulation complete."

    def test_callback_approves(self):
        """Callback returning True allows execution."""
        registry = _make_registry()
        backend = MagicMock()
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="expensive_sim", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Done."),
        ]
        cb = MagicMock(return_value=True)
        agent = AgentCore(backend=backend, tools=registry,
                          approval_callback=cb, auto_approve=False)
        result = agent.process("run sim")
        assert result == "Done."
        cb.assert_called_once()

    def test_callback_denies(self):
        """Callback returning False skips the tool."""
        registry = _make_registry()
        backend = MagicMock()
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="expensive_sim", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Skipped."),
        ]
        cb = MagicMock(return_value=False)
        agent = AgentCore(backend=backend, tools=registry,
                          approval_callback=cb, auto_approve=False)
        result = agent.process("run sim")
        assert result == "Skipped."
        # Check that the tool result says "skipped"
        tool_results = [m for m in agent.history if m.get("role") == "tool_result"]
        assert any("skipped" in str(m.get("result", "")).lower() for m in tool_results)

    def test_safe_tool_no_callback(self):
        """Safe tools (requires_approval=False) never trigger callback."""
        registry = _make_registry()
        backend = MagicMock()
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="safe_search", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Found 5 results."),
        ]
        cb = MagicMock(return_value=False)
        agent = AgentCore(backend=backend, tools=registry,
                          approval_callback=cb, auto_approve=False)
        result = agent.process("search")
        assert result == "Found 5 results."
        cb.assert_not_called()


class TestConsentInStream:
    def _make_streaming_backend(self, responses):
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

    def test_stream_approval_request_yielded(self):
        """ToolApprovalRequest is yielded for expensive tools in stream mode."""
        registry = _make_registry()
        responses = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="expensive_sim", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Done."),
        ]
        backend = self._make_streaming_backend(responses)
        cb = MagicMock(return_value=True)
        agent = AgentCore(backend=backend, tools=registry,
                          approval_callback=cb, auto_approve=False)
        events = list(agent.process_stream("run sim"))
        types = [type(e).__name__ for e in events]
        assert "ToolApprovalRequest" in types

    def test_stream_denial_skips_tool(self):
        """Denied tool in stream mode yields skipped result."""
        registry = _make_registry()
        responses = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="expensive_sim", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Ok."),
        ]
        backend = self._make_streaming_backend(responses)
        cb = MagicMock(return_value=False)
        agent = AgentCore(backend=backend, tools=registry,
                          approval_callback=cb, auto_approve=False)
        events = list(agent.process_stream("run sim"))
        tool_results = [e for e in events if isinstance(e, ToolCallResult)]
        assert any("skipped" in e.summary.lower() for e in tool_results)

    def test_stream_safe_tool_no_approval(self):
        """Safe tools in stream mode don't yield ToolApprovalRequest."""
        registry = _make_registry()
        responses = [
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="safe_search", tool_args={}, call_id="c1"),
            ]),
            AgentResponse(text="Results."),
        ]
        backend = self._make_streaming_backend(responses)
        agent = AgentCore(backend=backend, tools=registry, auto_approve=False)
        events = list(agent.process_stream("search"))
        types = [type(e).__name__ for e in events]
        assert "ToolApprovalRequest" not in types


class TestApprovalEvents:
    def test_approval_request_fields(self):
        req = ToolApprovalRequest(tool_name="sim", tool_args={"a": 1}, call_id="c1")
        assert req.tool_name == "sim"
        assert req.call_id == "c1"

    def test_approval_response_fields(self):
        resp = ToolApprovalResponse(call_id="c1", approved=True)
        assert resp.approved is True
