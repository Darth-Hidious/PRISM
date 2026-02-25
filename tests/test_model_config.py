"""Tests for ModelConfig registry and lookup."""
import pytest
from app.agent.models import ModelConfig, MODEL_REGISTRY, get_model_config


class TestModelConfigDataclass:
    def test_frozen_immutable(self):
        config = get_model_config("claude-opus-4-6")
        with pytest.raises(AttributeError):
            config.id = "something-else"

    def test_fields_present(self):
        config = get_model_config("claude-opus-4-6")
        assert config.id == "claude-opus-4-6"
        assert config.provider == "anthropic"
        assert isinstance(config.context_window, int)
        assert isinstance(config.max_output_tokens, int)
        assert isinstance(config.default_max_tokens, int)
        assert isinstance(config.input_price_per_mtok, float)
        assert isinstance(config.output_price_per_mtok, float)
        assert isinstance(config.supports_caching, bool)
        assert isinstance(config.supports_thinking, bool)
        assert isinstance(config.supports_tools, bool)


class TestExactMatch:
    def test_opus(self):
        config = get_model_config("claude-opus-4-6")
        assert config.provider == "anthropic"
        assert config.context_window == 200_000
        assert config.max_output_tokens == 128_000
        assert config.default_max_tokens == 32_768
        assert config.supports_caching is True
        assert config.supports_thinking is True

    def test_sonnet(self):
        config = get_model_config("claude-sonnet-4-20250514")
        assert config.provider == "anthropic"
        assert config.context_window == 200_000
        assert config.max_output_tokens == 64_000
        assert config.default_max_tokens == 16_384

    def test_haiku(self):
        config = get_model_config("claude-haiku-4-5-20251001")
        assert config.provider == "anthropic"
        assert config.default_max_tokens == 8_192
        assert config.supports_caching is True
        assert config.supports_thinking is False

    def test_gpt4o(self):
        config = get_model_config("gpt-4o")
        assert config.provider == "openai"
        assert config.context_window == 128_000
        assert config.max_output_tokens == 16_384
        assert config.supports_caching is False

    def test_glm5(self):
        config = get_model_config("glm-5")
        assert config.provider == "zhipu"
        assert config.context_window == 200_000
        assert config.max_output_tokens == 128_000
        assert config.default_max_tokens == 16_384

    def test_gemini_pro(self):
        config = get_model_config("gemini-2.5-pro")
        assert config.provider == "google"
        assert config.context_window == 1_000_000
        assert config.supports_thinking is True

    def test_o3(self):
        config = get_model_config("o3")
        assert config.provider == "openai"
        assert config.max_output_tokens == 100_000
        assert config.supports_thinking is True


class TestPrefixMatch:
    def test_claude_sonnet_prefix(self):
        config = get_model_config("claude-sonnet")
        assert config.provider == "anthropic"
        assert "claude-sonnet" in config.id

    def test_gpt_4o_prefix(self):
        config = get_model_config("gpt-4o")
        assert config.provider == "openai"

    def test_glm_prefix(self):
        config = get_model_config("glm-4")
        assert config.provider == "zhipu"
        assert "glm-4" in config.id


class TestOpenRouterPrefix:
    def test_anthropic_prefix_stripped(self):
        config = get_model_config("anthropic/claude-opus-4-6")
        assert config.id == "claude-opus-4-6"
        assert config.provider == "anthropic"
        assert config.context_window == 200_000

    def test_openai_prefix_stripped(self):
        config = get_model_config("openai/gpt-4o")
        assert config.id == "gpt-4o"
        assert config.provider == "openai"

    def test_google_prefix_stripped(self):
        config = get_model_config("google/gemini-2.5-pro")
        assert config.id == "gemini-2.5-pro"
        assert config.provider == "google"


class TestUnknownModel:
    def test_returns_default(self):
        config = get_model_config("totally-unknown-model-xyz")
        assert config.id == "totally-unknown-model-xyz"
        assert config.provider == "unknown"
        assert config.context_window == 128_000
        assert config.max_output_tokens == 16_384
        assert config.default_max_tokens == 8_192
        assert config.input_price_per_mtok == 0.0
        assert config.output_price_per_mtok == 0.0
        assert config.supports_tools is True


class TestRegistryInvariants:
    def test_minimum_model_count(self):
        assert len(MODEL_REGISTRY) >= 15

    def test_all_prices_non_negative(self):
        for model_id, config in MODEL_REGISTRY.items():
            assert config.input_price_per_mtok >= 0.0, f"{model_id} has negative input price"
            assert config.output_price_per_mtok >= 0.0, f"{model_id} has negative output price"

    def test_default_within_max_output(self):
        for model_id, config in MODEL_REGISTRY.items():
            assert config.default_max_tokens <= config.max_output_tokens, (
                f"{model_id}: default_max_tokens ({config.default_max_tokens}) "
                f"> max_output_tokens ({config.max_output_tokens})"
            )

    def test_all_ids_match_keys(self):
        for model_id, config in MODEL_REGISTRY.items():
            assert config.id == model_id, f"Key {model_id} != config.id {config.id}"

    def test_all_providers_valid(self):
        valid_providers = {"anthropic", "openai", "google", "zhipu"}
        for model_id, config in MODEL_REGISTRY.items():
            assert config.provider in valid_providers, f"{model_id} has unknown provider {config.provider}"
