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
        client.messages.create.return_value = self._make_tool_response("search_optimade", {"query": "silicon"}, "call_abc")
        backend = AnthropicBackend(api_key="test-key")
        resp = backend.complete(messages=[{"role": "user", "content": "Find silicon"}], tools=[{"name": "search_optimade", "description": "Search", "input_schema": {}}])
        assert resp.has_tool_calls is True
        assert resp.tool_calls[0].tool_name == "search_optimade"
        assert resp.tool_calls[0].call_id == "call_abc"

    @patch("app.agent.backends.anthropic_backend.Anthropic")
    def test_system_prompt_passed(self, mock_cls):
        client = mock_cls.return_value
        client.messages.create.return_value = self._make_text_response("ok")
        backend = AnthropicBackend(api_key="test-key")
        backend.complete(messages=[{"role": "user", "content": "hi"}], tools=[], system_prompt="Be helpful")
        call_kwargs = client.messages.create.call_args
        assert call_kwargs.kwargs.get("system") == "Be helpful"
