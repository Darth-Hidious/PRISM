"""Tests for LLM service factory."""
import os
import pytest
from unittest.mock import patch, MagicMock
from app.llm import get_llm_service, OpenAIService, AnthropicService, OpenRouterService


class TestGetLLMService:
    def test_explicit_provider_openai(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"}):
            svc = get_llm_service(provider="openai")
            assert isinstance(svc, OpenAIService)

    def test_explicit_provider_anthropic(self):
        with patch.dict(os.environ, {"ANTHROPIC_API_KEY": "test-key"}):
            svc = get_llm_service(provider="anthropic")
            assert isinstance(svc, AnthropicService)

    def test_explicit_provider_openrouter(self):
        with patch.dict(os.environ, {"OPENROUTER_API_KEY": "test-key"}):
            svc = get_llm_service(provider="openrouter")
            assert isinstance(svc, OpenRouterService)

    def test_auto_detect_openai(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"}, clear=True):
            svc = get_llm_service()
            assert isinstance(svc, OpenAIService)

    def test_no_provider_raises(self):
        with patch.dict(os.environ, {}, clear=True):
            for key in ["OPENROUTER_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GOOGLE_CLOUD_PROJECT"]:
                os.environ.pop(key, None)
            with pytest.raises(ValueError, match="No LLM provider configured"):
                get_llm_service()

    def test_unsupported_provider_raises(self):
        with pytest.raises(ValueError, match="Unsupported LLM provider"):
            get_llm_service(provider="nonexistent")
