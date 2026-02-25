"""Tests for model config integration and prompt caching in backends."""
import os
import pytest
from unittest.mock import patch, MagicMock


class TestAnthropicBackendUsesModelConfig:
    def test_init_stores_model_config(self):
        with patch.dict(os.environ, {"ANTHROPIC_API_KEY": "test-key"}):
            with patch("app.agent.backends.anthropic_backend.Anthropic"):
                from app.agent.backends.anthropic_backend import AnthropicBackend
                backend = AnthropicBackend(model="claude-opus-4-6")
                assert backend.model_config.id == "claude-opus-4-6"
                assert backend.model_config.default_max_tokens == 32_768

    def test_complete_uses_model_config_max_tokens(self):
        with patch.dict(os.environ, {"ANTHROPIC_API_KEY": "test-key"}):
            with patch("app.agent.backends.anthropic_backend.Anthropic") as MockClient:
                mock_client = MockClient.return_value
                mock_response = MagicMock()
                mock_response.content = [MagicMock(type="text", text="hi")]
                mock_response.usage = None
                mock_client.messages.create.return_value = mock_response
                from app.agent.backends.anthropic_backend import AnthropicBackend
                backend = AnthropicBackend(model="claude-opus-4-6")
                backend.complete([], [])
                call_kwargs = mock_client.messages.create.call_args.kwargs
                assert call_kwargs["max_tokens"] == 32_768


class TestOpenAIBackendUsesModelConfig:
    def test_init_stores_model_config(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"}):
            with patch("app.agent.backends.openai_backend.OpenAI"):
                from app.agent.backends.openai_backend import OpenAIBackend
                backend = OpenAIBackend(model="gpt-4.1")
                assert backend.model_config.id == "gpt-4.1"
                assert backend.model_config.default_max_tokens == 16_384

    def test_complete_uses_model_config_max_tokens(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"}):
            with patch("app.agent.backends.openai_backend.OpenAI") as MockClient:
                mock_client = MockClient.return_value
                mock_msg = MagicMock()
                mock_msg.content = "hi"
                mock_msg.tool_calls = None
                mock_response = MagicMock()
                mock_response.choices = [MagicMock(message=mock_msg)]
                mock_response.usage = None
                mock_client.chat.completions.create.return_value = mock_response
                from app.agent.backends.openai_backend import OpenAIBackend
                backend = OpenAIBackend(model="gpt-4.1")
                backend.complete([], [])
                call_kwargs = mock_client.chat.completions.create.call_args.kwargs
                assert call_kwargs["max_tokens"] == 16_384


class TestAnthropicPromptCaching:
    def test_system_prompt_has_cache_control(self):
        with patch.dict(os.environ, {"ANTHROPIC_API_KEY": "test-key"}):
            with patch("app.agent.backends.anthropic_backend.Anthropic") as MockClient:
                mock_client = MockClient.return_value
                mock_response = MagicMock()
                mock_response.content = [MagicMock(type="text", text="hi")]
                mock_response.usage = None
                mock_client.messages.create.return_value = mock_response
                from app.agent.backends.anthropic_backend import AnthropicBackend
                backend = AnthropicBackend(model="claude-sonnet-4-20250514")
                backend.complete([], [], system_prompt="You are PRISM.")
                call_kwargs = mock_client.messages.create.call_args.kwargs
                system = call_kwargs["system"]
                assert isinstance(system, list)
                assert system[0]["type"] == "text"
                assert system[0]["text"] == "You are PRISM."
                assert system[0]["cache_control"] == {"type": "ephemeral"}

    def test_no_system_prompt_no_system_key(self):
        with patch.dict(os.environ, {"ANTHROPIC_API_KEY": "test-key"}):
            with patch("app.agent.backends.anthropic_backend.Anthropic") as MockClient:
                mock_client = MockClient.return_value
                mock_response = MagicMock()
                mock_response.content = [MagicMock(type="text", text="hi")]
                mock_response.usage = None
                mock_client.messages.create.return_value = mock_response
                from app.agent.backends.anthropic_backend import AnthropicBackend
                backend = AnthropicBackend()
                backend.complete([], [])
                call_kwargs = mock_client.messages.create.call_args.kwargs
                assert "system" not in call_kwargs

    def test_openai_no_cache_control(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"}):
            with patch("app.agent.backends.openai_backend.OpenAI") as MockClient:
                mock_client = MockClient.return_value
                mock_msg = MagicMock()
                mock_msg.content = "hi"
                mock_msg.tool_calls = None
                mock_response = MagicMock()
                mock_response.choices = [MagicMock(message=mock_msg)]
                mock_response.usage = None
                mock_client.chat.completions.create.return_value = mock_response
                from app.agent.backends.openai_backend import OpenAIBackend
                backend = OpenAIBackend(model="gpt-4o")
                backend.complete([], [], system_prompt="You are PRISM.")
                call_kwargs = mock_client.chat.completions.create.call_args.kwargs
                msgs = call_kwargs["messages"]
                assert msgs[0]["role"] == "system"
                assert isinstance(msgs[0]["content"], str)
