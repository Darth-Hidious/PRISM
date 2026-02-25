"""Tests for RLM-inspired ResultStore and peek_result tool."""
import json
from unittest.mock import MagicMock
from app.agent.core import AgentCore, MAX_TOOL_RESULT_CHARS
from app.agent.events import AgentResponse, ToolCallEvent, ToolCallResult, TurnComplete
from app.tools.base import Tool, ToolRegistry


class TestProcessToolResult:
    def _make_agent(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="done")
        return AgentCore(backend=backend, tools=ToolRegistry())

    def test_small_result_unchanged(self):
        agent = self._make_agent()
        result = {"count": 5, "data": "small"}
        processed = agent._process_tool_result(result, call_id="c1")
        assert processed == result
        assert "_stored" not in processed
        assert "c1" not in agent._result_store

    def test_large_result_stored(self):
        agent = self._make_agent()
        result = {"data": "x" * 40_000}
        processed = agent._process_tool_result(result, call_id="c1")
        assert processed["_stored"] is True
        assert processed["_result_id"] == "c1"
        assert processed["_total_size"] > 30_000
        assert "preview" in processed
        assert "peek_result" in processed["notice"]
        assert "c1" in agent._result_store
        assert len(agent._result_store["c1"]) > 30_000

    def test_preview_is_bounded(self):
        agent = self._make_agent()
        result = {"data": "abcdef" * 10_000}
        processed = agent._process_tool_result(result, call_id="c1")
        assert len(processed["preview"]) <= 2000

    def test_string_result_stored(self):
        agent = self._make_agent()
        result = "x" * 40_000
        processed = agent._process_tool_result(result, call_id="c1")
        assert isinstance(processed, dict)
        assert processed["_stored"] is True


class TestPeekResult:
    def _make_agent_with_stored(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="done")
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        agent._result_store["c1"] = "A" * 10_000 + "B" * 10_000 + "C" * 10_000
        return agent

    def test_peek_first_chunk(self):
        agent = self._make_agent_with_stored()
        result = agent._peek_result(result_id="c1", offset=0, limit=5000)
        assert result["offset"] == 0
        assert result["total_size"] == 30_000
        assert result["has_more"] is True
        assert len(result["chunk"]) == 5000
        assert result["chunk"] == "A" * 5000

    def test_peek_middle_chunk(self):
        agent = self._make_agent_with_stored()
        result = agent._peek_result(result_id="c1", offset=10_000, limit=5000)
        assert result["chunk"] == "B" * 5000

    def test_peek_last_chunk(self):
        agent = self._make_agent_with_stored()
        result = agent._peek_result(result_id="c1", offset=25_000, limit=10_000)
        assert result["has_more"] is False
        assert len(result["chunk"]) == 5000

    def test_peek_nonexistent(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="done")
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        result = agent._peek_result(result_id="nope")
        assert "error" in result

    def test_peek_default_params(self):
        agent = self._make_agent_with_stored()
        result = agent._peek_result(result_id="c1")
        assert result["offset"] == 0
        assert result["limit"] == 5000


class TestResultStoreInTAORLoop:
    def test_large_result_stored_in_process(self):
        def big_tool(**kw):
            return {"data": "x" * 40_000}

        registry = ToolRegistry()
        registry.register(Tool(name="big", description="Big", input_schema={}, func=big_tool))
        backend = MagicMock()
        backend.complete.side_effect = [
            AgentResponse(tool_calls=[ToolCallEvent(tool_name="big", tool_args={}, call_id="c1")]),
            AgentResponse(text="Done."),
        ]
        agent = AgentCore(backend=backend, tools=registry)
        agent.process("get big data")

        tool_results = [m for m in agent.history if m.get("role") == "tool_result"]
        assert len(tool_results) == 1
        assert tool_results[0]["result"].get("_stored") is True
        assert "c1" in agent._result_store

    def test_large_result_stored_in_stream(self):
        def big_tool(**kw):
            return {"data": "x" * 40_000}

        registry = ToolRegistry()
        registry.register(Tool(name="big", description="Big", input_schema={}, func=big_tool))
        backend = MagicMock()
        call_count = [0]
        resp_tool = AgentResponse(tool_calls=[ToolCallEvent(tool_name="big", tool_args={}, call_id="c1")])
        resp_text = AgentResponse(text="Done.")

        def fake_stream(messages, tools, system_prompt=None):
            idx = call_count[0]
            call_count[0] += 1
            resp = [resp_tool, resp_text][idx]
            backend._last_stream_response = resp
            yield TurnComplete(text=resp.text, has_more=resp.has_tool_calls)

        backend.complete_stream = fake_stream
        agent = AgentCore(backend=backend, tools=registry)
        events = list(agent.process_stream("get big data"))

        tool_results = [e for e in events if isinstance(e, ToolCallResult)]
        assert len(tool_results) == 1
        assert tool_results[0].result.get("_stored") is True

    def test_peek_result_in_tool_defs_when_store_has_data(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="done")
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        agent._result_store["c1"] = "data"
        defs = agent._get_tool_defs()
        names = [t["name"] for t in defs]
        assert "peek_result" in names

    def test_no_peek_result_when_store_empty(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="done")
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        defs = agent._get_tool_defs()
        names = [t["name"] for t in defs]
        assert "peek_result" not in names

    def test_reset_clears_store(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="done")
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        agent._result_store["c1"] = "data"
        agent.reset()
        assert len(agent._result_store) == 0
