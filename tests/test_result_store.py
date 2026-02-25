"""Tests for RLM-inspired ResultStore and peek_result tool."""
import json
from unittest.mock import MagicMock
from app.agent.core import AgentCore, MAX_TOOL_RESULT_CHARS
from app.agent.events import AgentResponse, ToolCallEvent, ToolCallResult, TurnComplete, UsageInfo
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


class TestPagingWorkflow:
    """Test the full 'output more' paging workflow: large result -> store -> peek_result calls."""

    def _make_big_data(self, size=50_000):
        """Create a data string large enough to exceed MAX_TOOL_RESULT_CHARS."""
        return "D" * size

    def test_agent_pages_through_large_result_via_process(self):
        """Simulate agent calling a tool that returns large data, then paging
        through it with multiple peek_result calls in the TAOR loop.
        """
        big_data = self._make_big_data(50_000)

        def big_tool(**kw):
            return {"data": big_data}

        registry = ToolRegistry()
        registry.register(Tool(name="fetch", description="Fetch", input_schema={}, func=big_tool))

        backend = MagicMock()
        backend.complete.side_effect = [
            # Turn 1: LLM calls the big tool
            AgentResponse(tool_calls=[ToolCallEvent(tool_name="fetch", tool_args={}, call_id="c1")]),
            # Turn 2: LLM sees stored notice, peeks at offset 0
            AgentResponse(tool_calls=[ToolCallEvent(
                tool_name="peek_result",
                tool_args={"result_id": "c1", "offset": 0, "limit": 10_000},
                call_id="peek1",
            )]),
            # Turn 3: LLM peeks at offset 10000
            AgentResponse(tool_calls=[ToolCallEvent(
                tool_name="peek_result",
                tool_args={"result_id": "c1", "offset": 10_000, "limit": 10_000},
                call_id="peek2",
            )]),
            # Turn 4: LLM is satisfied and responds
            AgentResponse(text="I've reviewed the data."),
        ]

        agent = AgentCore(backend=backend, tools=registry)
        result = agent.process("get all data")

        assert result == "I've reviewed the data."

        # Verify the result was stored
        assert "c1" in agent._result_store

        # Verify peek_result calls produced correct history entries
        tool_results = [m for m in agent.history if m.get("role") == "tool_result"]
        assert len(tool_results) == 3  # big tool + 2 peeks

        # First tool result is stored
        assert tool_results[0]["result"]["_stored"] is True

        # Second result is peek at offset 0 with has_more=True
        peek1 = tool_results[1]["result"]
        assert peek1["offset"] == 0
        assert peek1["has_more"] is True
        assert len(peek1["chunk"]) == 10_000

        # Third result is peek at offset 10000
        peek2 = tool_results[2]["result"]
        assert peek2["offset"] == 10_000
        assert peek2["has_more"] is True

    def test_peek_result_shows_has_more_false_at_end(self):
        """Peeking at the tail of the stored data should set has_more=False."""
        big_data = self._make_big_data(50_000)

        def big_tool(**kw):
            return {"data": big_data}

        registry = ToolRegistry()
        registry.register(Tool(name="fetch", description="Fetch", input_schema={}, func=big_tool))

        backend = MagicMock()
        # Serialized size will be > 50_000 due to JSON wrapping
        # We need to peek at an offset near the end
        backend.complete.side_effect = [
            # Turn 1: call the big tool
            AgentResponse(tool_calls=[ToolCallEvent(tool_name="fetch", tool_args={}, call_id="c1")]),
            # Turn 2: peek near the end (offset large enough to reach EOF)
            AgentResponse(tool_calls=[ToolCallEvent(
                tool_name="peek_result",
                tool_args={"result_id": "c1", "offset": 45_000, "limit": 20_000},
                call_id="peek_end",
            )]),
            # Turn 3: done
            AgentResponse(text="All read."),
        ]

        agent = AgentCore(backend=backend, tools=registry)
        agent.process("read everything")

        tool_results = [m for m in agent.history if m.get("role") == "tool_result"]
        peek_end = tool_results[1]["result"]
        assert peek_end["has_more"] is False

    def test_paging_in_stream_mode(self):
        """peek_result should work correctly in the streaming TAOR loop."""
        big_data = self._make_big_data(50_000)

        def big_tool(**kw):
            return {"data": big_data}

        registry = ToolRegistry()
        registry.register(Tool(name="fetch", description="Fetch", input_schema={}, func=big_tool))

        backend = MagicMock()
        call_count = [0]

        responses = [
            # Turn 1: call the big tool
            AgentResponse(tool_calls=[ToolCallEvent(tool_name="fetch", tool_args={}, call_id="c1")]),
            # Turn 2: peek at offset 0
            AgentResponse(tool_calls=[ToolCallEvent(
                tool_name="peek_result",
                tool_args={"result_id": "c1", "offset": 0, "limit": 5_000},
                call_id="peek1",
            )]),
            # Turn 3: final answer
            AgentResponse(text="Done paging."),
        ]

        def fake_stream(messages, tools, system_prompt=None):
            idx = call_count[0]
            call_count[0] += 1
            resp = responses[idx]
            backend._last_stream_response = resp
            yield TurnComplete(text=resp.text, has_more=resp.has_tool_calls)

        backend.complete_stream = fake_stream
        agent = AgentCore(backend=backend, tools=registry)
        events = list(agent.process_stream("page through data"))

        # Should have ToolCallResult events for both the big tool and peek_result
        tool_result_events = [e for e in events if isinstance(e, ToolCallResult)]
        assert len(tool_result_events) == 2

        # First is stored result
        assert tool_result_events[0].result["_stored"] is True

        # Second is peek result
        peek = tool_result_events[1].result
        assert "chunk" in peek
        assert peek["offset"] == 0
        assert peek["has_more"] is True

    def test_tool_defs_include_peek_after_store_exclude_before(self):
        """_get_tool_defs dynamically includes peek_result only when store is populated.

        Verify the tool defs passed to the backend change across iterations.
        """
        big_data = self._make_big_data(50_000)

        def big_tool(**kw):
            return {"data": big_data}

        registry = ToolRegistry()
        registry.register(Tool(name="fetch", description="Fetch", input_schema={}, func=big_tool))

        captured_tool_defs = []
        backend = MagicMock()

        def capture_complete(messages, tools, system_prompt=None):
            captured_tool_defs.append([t["name"] for t in tools])
            idx = len(captured_tool_defs) - 1
            if idx == 0:
                return AgentResponse(tool_calls=[ToolCallEvent(tool_name="fetch", tool_args={}, call_id="c1")])
            return AgentResponse(text="Done.")

        backend.complete.side_effect = capture_complete
        agent = AgentCore(backend=backend, tools=registry)
        agent.process("get data")

        # First call: store was empty, no peek_result
        assert "peek_result" not in captured_tool_defs[0]
        # Second call: store has data, peek_result should be present
        assert "peek_result" in captured_tool_defs[1]

    def test_multiple_stored_results_independently_peekable(self):
        """When multiple tool results are stored, each can be peeked independently."""
        def tool_a(**kw):
            return {"data": "A" * 40_000}

        def tool_b(**kw):
            return {"data": "B" * 40_000}

        registry = ToolRegistry()
        registry.register(Tool(name="tool_a", description="A", input_schema={}, func=tool_a))
        registry.register(Tool(name="tool_b", description="B", input_schema={}, func=tool_b))

        backend = MagicMock()
        backend.complete.side_effect = [
            # Turn 1: call both tools
            AgentResponse(tool_calls=[
                ToolCallEvent(tool_name="tool_a", tool_args={}, call_id="a1"),
                ToolCallEvent(tool_name="tool_b", tool_args={}, call_id="b1"),
            ]),
            # Turn 2: peek at tool_a result
            AgentResponse(tool_calls=[ToolCallEvent(
                tool_name="peek_result",
                tool_args={"result_id": "a1", "offset": 0, "limit": 100},
                call_id="peek_a",
            )]),
            # Turn 3: peek at tool_b result
            AgentResponse(tool_calls=[ToolCallEvent(
                tool_name="peek_result",
                tool_args={"result_id": "b1", "offset": 0, "limit": 100},
                call_id="peek_b",
            )]),
            # Turn 4: done
            AgentResponse(text="Compared both."),
        ]

        agent = AgentCore(backend=backend, tools=registry)
        agent.process("compare data")

        assert "a1" in agent._result_store
        assert "b1" in agent._result_store

        tool_results = [m for m in agent.history if m.get("role") == "tool_result"]
        # 2 big tools + 2 peeks = 4 tool results
        assert len(tool_results) == 4

        # Peek results should contain data from the correct stored result
        peek_a = tool_results[2]["result"]
        peek_b = tool_results[3]["result"]
        assert "A" in peek_a["chunk"]  # tool_a data contains "A"s
        assert "B" in peek_b["chunk"]  # tool_b data contains "B"s
