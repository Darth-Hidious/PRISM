"""Tests for OpenAI backend."""
import json
import pytest
from unittest.mock import patch, MagicMock
from app.agent.backends.openai_backend import OpenAIBackend


class TestOpenAIBackend:
    def _make_text_response(self, text):
        mock = MagicMock()
        choice = MagicMock()
        choice.message.content = text
        choice.message.tool_calls = None
        mock.choices = [choice]
        return mock

    def _make_tool_response(self, tool_name, tool_args, call_id):
        mock = MagicMock()
        choice = MagicMock()
        choice.message.content = None
        tc = MagicMock()
        tc.function.name = tool_name
        tc.function.arguments = json.dumps(tool_args)
        tc.id = call_id
        choice.message.tool_calls = [tc]
        mock.choices = [choice]
        return mock

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_text_response(self, mock_cls):
        client = mock_cls.return_value
        client.chat.completions.create.return_value = self._make_text_response("Hi there")
        backend = OpenAIBackend(api_key="test-key")
        resp = backend.complete(messages=[{"role": "user", "content": "Hello"}], tools=[], system_prompt="Be helpful")
        assert resp.text == "Hi there"
        assert resp.has_tool_calls is False

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_tool_call_response(self, mock_cls):
        client = mock_cls.return_value
        client.chat.completions.create.return_value = self._make_tool_response("search_materials", {"query": "Fe"}, "call_xyz")
        backend = OpenAIBackend(api_key="test-key")
        resp = backend.complete(messages=[{"role": "user", "content": "Find iron"}], tools=[{"name": "search_materials", "description": "Search", "input_schema": {}}])
        assert resp.has_tool_calls is True
        assert resp.tool_calls[0].tool_name == "search_materials"
        assert resp.tool_calls[0].call_id == "call_xyz"

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_system_prompt_injected(self, mock_cls):
        client = mock_cls.return_value
        client.chat.completions.create.return_value = self._make_text_response("ok")
        backend = OpenAIBackend(api_key="test-key")
        backend.complete(messages=[{"role": "user", "content": "hi"}], tools=[], system_prompt="You are PRISM")
        call_args = client.chat.completions.create.call_args
        msgs = call_args.kwargs.get("messages", [])
        assert msgs[0]["role"] == "system"
        assert msgs[0]["content"] == "You are PRISM"

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_openrouter_via_base_url(self, mock_cls):
        backend = OpenAIBackend(api_key="or-key", base_url="https://openrouter.ai/api/v1", model="anthropic/claude-3.5-sonnet")
        assert backend.model == "anthropic/claude-3.5-sonnet"

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_malformed_tool_arguments(self, mock_cls):
        """Gracefully handle invalid JSON in tool arguments."""
        mock = MagicMock()
        choice = MagicMock()
        choice.message.content = None
        tc = MagicMock()
        tc.function.name = "search"
        tc.function.arguments = "{invalid json"
        tc.id = "call_1"
        choice.message.tool_calls = [tc]
        mock.choices = [choice]
        client = mock_cls.return_value
        client.chat.completions.create.return_value = mock

        backend = OpenAIBackend(api_key="test")
        resp = backend.complete(messages=[{"role": "user", "content": "test"}], tools=[])
        assert resp.has_tool_calls
        assert resp.tool_calls[0].tool_args == {}

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_complete_stream_text(self, mock_cls):
        from app.agent.events import TextDelta, TurnComplete
        client = mock_cls.return_value

        chunks = []
        for text in ["Hello ", "world"]:
            chunk = MagicMock()
            delta = MagicMock()
            delta.content = text
            delta.tool_calls = None
            chunk.choices = [MagicMock(delta=delta)]
            chunks.append(chunk)

        client.chat.completions.create.return_value = iter(chunks)
        backend = OpenAIBackend(api_key="test-key")
        events = list(backend.complete_stream(messages=[{"role": "user", "content": "hi"}], tools=[]))
        text_deltas = [e for e in events if isinstance(e, TextDelta)]
        assert len(text_deltas) == 2
        assert text_deltas[0].text == "Hello "
        assert isinstance(events[-1], TurnComplete)
        assert backend._last_stream_response.text == "Hello world"

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_complete_stream_tool_call(self, mock_cls):
        from app.agent.events import ToolCallStart, TurnComplete
        client = mock_cls.return_value

        # First chunk: tool call start with name
        chunk1 = MagicMock()
        tc_delta1 = MagicMock()
        tc_delta1.index = 0
        tc_delta1.id = "call_1"
        tc_delta1.function = MagicMock()
        tc_delta1.function.name = "search"
        tc_delta1.function.arguments = '{"q":'
        delta1 = MagicMock()
        delta1.content = None
        delta1.tool_calls = [tc_delta1]
        chunk1.choices = [MagicMock(delta=delta1)]

        # Second chunk: more arguments
        chunk2 = MagicMock()
        tc_delta2 = MagicMock()
        tc_delta2.index = 0
        tc_delta2.id = None
        tc_delta2.function = MagicMock()
        tc_delta2.function.name = None
        tc_delta2.function.arguments = '"Si"}'
        delta2 = MagicMock()
        delta2.content = None
        delta2.tool_calls = [tc_delta2]
        chunk2.choices = [MagicMock(delta=delta2)]

        client.chat.completions.create.return_value = iter([chunk1, chunk2])
        backend = OpenAIBackend(api_key="test-key")
        events = list(backend.complete_stream(messages=[{"role": "user", "content": "find Si"}], tools=[{"name": "search", "description": "Search", "input_schema": {}}]))
        tool_starts = [e for e in events if isinstance(e, ToolCallStart)]
        assert len(tool_starts) == 1
        assert tool_starts[0].tool_name == "search"
        assert isinstance(events[-1], TurnComplete)
        assert events[-1].has_more is True
        assert backend._last_stream_response.tool_calls[0].tool_args == {"q": "Si"}

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_complete_stream_tool_call_missing_id_gets_synthesized(self, mock_cls):
        client = mock_cls.return_value

        chunk1 = MagicMock()
        tc_delta1 = MagicMock()
        tc_delta1.index = 0
        tc_delta1.id = None
        tc_delta1.function = MagicMock()
        tc_delta1.function.name = "search"
        tc_delta1.function.arguments = '{"q":"Si"}'
        delta1 = MagicMock()
        delta1.content = None
        delta1.tool_calls = [tc_delta1]
        chunk1.choices = [MagicMock(delta=delta1)]

        client.chat.completions.create.return_value = iter([chunk1])
        backend = OpenAIBackend(api_key="test-key")
        list(backend.complete_stream(
            messages=[{"role": "user", "content": "find Si"}],
            tools=[{"name": "search", "description": "Search", "input_schema": {}}],
        ))

        tc = backend._last_stream_response.tool_calls[0]
        assert tc.call_id.startswith("prism_call_")
        assert tc.tool_args == {"q": "Si"}

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_format_messages_backfills_missing_tool_result_id(self, mock_cls):
        backend = OpenAIBackend(api_key="test-key")
        messages = [
            {
                "role": "tool_calls",
                "text": None,
                "calls": [{"id": "", "name": "search", "args": {"q": "Si"}}],
            },
            {
                "role": "tool_result",
                "tool_call_id": "",
                "result": {"count": 1},
            },
        ]

        formatted = backend._format_messages(messages)
        assert formatted[0]["role"] == "assistant"
        assert formatted[0]["tool_calls"][0]["id"].startswith("prism_call_")
        assert formatted[1]["role"] == "tool"
        assert formatted[1]["tool_call_id"] == formatted[0]["tool_calls"][0]["id"]
