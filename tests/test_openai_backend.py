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
        client.chat.completions.create.return_value = self._make_tool_response("search_optimade", {"query": "Fe"}, "call_xyz")
        backend = OpenAIBackend(api_key="test-key")
        resp = backend.complete(messages=[{"role": "user", "content": "Find iron"}], tools=[{"name": "search_optimade", "description": "Search", "input_schema": {}}])
        assert resp.has_tool_calls is True
        assert resp.tool_calls[0].tool_name == "search_optimade"
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
