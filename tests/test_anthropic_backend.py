"""Tests for Anthropic backend."""
import pytest
from unittest.mock import patch, MagicMock
from app.agent.backends.anthropic_backend import AnthropicBackend


class TestAnthropicBackend:
    def _make_text_response(self, text):
        mock = MagicMock()
        block = MagicMock()
        block.type = "text"
        block.text = text
        mock.content = [block]
        return mock

    def _make_tool_response(self, tool_name, tool_args, call_id):
        mock = MagicMock()
        block = MagicMock()
        block.type = "tool_use"
        block.name = tool_name
        block.input = tool_args
        block.id = call_id
        mock.content = [block]
        return mock

    @patch("app.agent.backends.anthropic_backend.Anthropic")
    def test_text_response(self, mock_cls):
        client = mock_cls.return_value
        client.messages.create.return_value = self._make_text_response("Hello!")
        backend = AnthropicBackend(api_key="test-key")
        resp = backend.complete(messages=[{"role": "user", "content": "Hi"}], tools=[], system_prompt="You are helpful.")
        assert resp.text == "Hello!"
        assert resp.has_tool_calls is False

    @patch("app.agent.backends.anthropic_backend.Anthropic")
    def test_tool_call_response(self, mock_cls):
        client = mock_cls.return_value
        client.messages.create.return_value = self._make_tool_response("search_materials", {"query": "silicon"}, "call_abc")
        backend = AnthropicBackend(api_key="test-key")
        resp = backend.complete(messages=[{"role": "user", "content": "Find silicon"}], tools=[{"name": "search_materials", "description": "Search", "input_schema": {}}])
        assert resp.has_tool_calls is True
        assert resp.tool_calls[0].tool_name == "search_materials"
        assert resp.tool_calls[0].call_id == "call_abc"

    @patch("app.agent.backends.anthropic_backend.Anthropic")
    def test_system_prompt_passed(self, mock_cls):
        client = mock_cls.return_value
        client.messages.create.return_value = self._make_text_response("ok")
        backend = AnthropicBackend(api_key="test-key")
        backend.complete(messages=[{"role": "user", "content": "hi"}], tools=[], system_prompt="Be helpful")
        call_kwargs = client.messages.create.call_args
        assert call_kwargs.kwargs.get("system") == "Be helpful"

    @patch("app.agent.backends.anthropic_backend.Anthropic")
    def test_complete_stream_text(self, mock_cls):
        from app.agent.events import TextDelta, TurnComplete
        client = mock_cls.return_value

        # Create mock stream events
        ev1 = MagicMock()
        ev1.type = "content_block_delta"
        ev1.delta = MagicMock()
        ev1.delta.text = "Hello "

        ev2 = MagicMock()
        ev2.type = "content_block_delta"
        ev2.delta = MagicMock()
        ev2.delta.text = "world"

        stream_ctx = MagicMock()
        stream_ctx.__enter__ = lambda s: s
        stream_ctx.__exit__ = MagicMock(return_value=False)
        stream_ctx.__iter__ = MagicMock(return_value=iter([ev1, ev2]))
        stream_ctx.get_final_message.return_value = self._make_text_response("Hello world")
        client.messages.stream.return_value = stream_ctx

        backend = AnthropicBackend(api_key="test-key")
        events = list(backend.complete_stream(messages=[{"role": "user", "content": "hi"}], tools=[]))
        text_deltas = [e for e in events if isinstance(e, TextDelta)]
        assert len(text_deltas) == 2
        assert text_deltas[0].text == "Hello "
        assert isinstance(events[-1], TurnComplete)
        assert backend._last_stream_response.text == "Hello world"

    @patch("app.agent.backends.anthropic_backend.Anthropic")
    def test_complete_stream_tool_call(self, mock_cls):
        from app.agent.events import ToolCallStart, TurnComplete
        client = mock_cls.return_value

        ev1 = MagicMock()
        ev1.type = "content_block_start"
        ev1.content_block = MagicMock()
        ev1.content_block.type = "tool_use"
        ev1.content_block.name = "search_materials"
        ev1.content_block.id = "call_1"

        stream_ctx = MagicMock()
        stream_ctx.__enter__ = lambda s: s
        stream_ctx.__exit__ = MagicMock(return_value=False)
        stream_ctx.__iter__ = MagicMock(return_value=iter([ev1]))
        stream_ctx.get_final_message.return_value = self._make_tool_response("search_materials", {"q": "Si"}, "call_1")
        client.messages.stream.return_value = stream_ctx

        backend = AnthropicBackend(api_key="test-key")
        events = list(backend.complete_stream(messages=[{"role": "user", "content": "find Si"}], tools=[{"name": "search_materials"}]))
        tool_starts = [e for e in events if isinstance(e, ToolCallStart)]
        assert len(tool_starts) == 1
        assert tool_starts[0].tool_name == "search_materials"
        assert isinstance(events[-1], TurnComplete)
        assert events[-1].has_more is True
