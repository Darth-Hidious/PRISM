"""Model configuration registry for LLM backends."""
from dataclasses import dataclass


@dataclass(frozen=True)
class ModelConfig:
    """Immutable configuration for a specific LLM model."""
    id: str
    provider: str
    context_window: int
    max_output_tokens: int
    default_max_tokens: int
    input_price_per_mtok: float
    output_price_per_mtok: float
    supports_caching: bool = False
    supports_thinking: bool = False
    supports_tools: bool = True


MODEL_REGISTRY: dict[str, ModelConfig] = {}


def _reg(
    id: str, provider: str,
    context: int, max_out: int, default: int,
    price_in: float, price_out: float,
    cache: bool = False, think: bool = False,
) -> None:
    MODEL_REGISTRY[id] = ModelConfig(
        id=id, provider=provider,
        context_window=context, max_output_tokens=max_out, default_max_tokens=default,
        input_price_per_mtok=price_in, output_price_per_mtok=price_out,
        supports_caching=cache, supports_thinking=think,
    )


# --- Anthropic ---
_reg("claude-opus-4-6",              "anthropic", 200_000, 128_000, 32_768, 5.00, 25.00, cache=True, think=True)
_reg("claude-sonnet-4-6",            "anthropic", 200_000,  64_000, 16_384, 3.00, 15.00, cache=True, think=True)
_reg("claude-sonnet-4-20250514",     "anthropic", 200_000,  64_000, 16_384, 3.00, 15.00, cache=True, think=True)
_reg("claude-sonnet-4-20250318",     "anthropic", 200_000,  64_000, 16_384, 3.00, 15.00, cache=True, think=True)
_reg("claude-haiku-4-5-20251001",    "anthropic", 200_000,  64_000,  8_192, 1.00,  5.00, cache=True)

# --- OpenAI ---
_reg("gpt-4o",                       "openai",    128_000,  16_384,  8_192, 2.50, 10.00)
_reg("gpt-4o-mini",                  "openai",    128_000,  16_384,  4_096, 0.15,  0.60)
_reg("gpt-4.1",                      "openai",  1_000_000,  32_768, 16_384, 2.00,  8.00)
_reg("gpt-4.1-mini",                 "openai",  1_000_000,  32_768,  8_192, 0.40,  1.60)
_reg("gpt-5",                        "openai",    400_000, 128_000, 16_384, 1.25, 10.00, think=True)
_reg("o3",                           "openai",    200_000, 100_000, 16_384, 2.00,  8.00, think=True)
_reg("o3-mini",                      "openai",    200_000, 100_000,  8_192, 1.10,  4.40, think=True)

# --- Google ---
_reg("gemini-2.5-pro",               "google",  1_000_000,  65_536, 16_384, 1.25, 10.00, think=True)
_reg("gemini-2.5-flash",             "google",  1_000_000,  65_536,  8_192, 0.30,  2.50)
_reg("gemini-3.1-pro",               "google",  1_000_000,  65_536, 16_384, 2.00, 12.00, think=True)

# --- Zhipu ---
_reg("glm-5",                        "zhipu",     200_000, 128_000, 16_384, 1.00,  3.20)
_reg("glm-4.7",                      "zhipu",     128_000,  16_384,  8_192, 0.38,  1.70)
_reg("glm-4.5-air",                  "zhipu",     128_000,  16_384,  4_096, 0.10,  0.50)

del _reg


def get_model_config(model_id: str) -> ModelConfig:
    """Look up model configuration by ID.

    Lookup order:
    1. Exact match in MODEL_REGISTRY
    2. Strip OpenRouter prefix (e.g. "anthropic/claude-opus-4-6")
    3. Prefix match (first entry whose key starts with model_id)
    4. Default: unknown provider with conservative defaults
    """
    # 1. Exact match
    if model_id in MODEL_REGISTRY:
        return MODEL_REGISTRY[model_id]

    # 2. Strip OpenRouter-style prefix ("provider/model-name")
    if "/" in model_id:
        stripped = model_id.split("/", 1)[1]
        if stripped in MODEL_REGISTRY:
            return MODEL_REGISTRY[stripped]

    # 3. Prefix match (first registry key that starts with model_id)
    for key, config in MODEL_REGISTRY.items():
        if key.startswith(model_id):
            return config

    # 4. Default for unknown models
    return ModelConfig(
        id=model_id,
        provider="unknown",
        context_window=128_000,
        max_output_tokens=16_384,
        default_max_tokens=8_192,
        input_price_per_mtok=0.0,
        output_price_per_mtok=0.0,
    )
