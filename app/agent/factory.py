"""Factory for creating agent backends from configuration."""
import os
from typing import Optional
from app.agent.backends.base import Backend


def create_backend(provider: Optional[str] = None, model: Optional[str] = None) -> Backend:
    if provider is None:
        if os.getenv("ANTHROPIC_API_KEY"):
            provider = "anthropic"
        elif os.getenv("OPENAI_API_KEY"):
            provider = "openai"
        elif os.getenv("OPENROUTER_API_KEY"):
            provider = "openrouter"
        else:
            raise ValueError("No LLM provider configured for agent mode. Set ANTHROPIC_API_KEY, OPENAI_API_KEY, or OPENROUTER_API_KEY.")
    if provider == "anthropic":
        from app.agent.backends.anthropic_backend import AnthropicBackend
        return AnthropicBackend(model=model)
    elif provider == "openai":
        from app.agent.backends.openai_backend import OpenAIBackend
        return OpenAIBackend(model=model)
    elif provider == "openrouter":
        from app.agent.backends.openai_backend import OpenAIBackend
        return OpenAIBackend(model=model or os.getenv("PRISM_MODEL", "anthropic/claude-3.5-sonnet"), base_url="https://openrouter.ai/api/v1", api_key=os.getenv("OPENROUTER_API_KEY"))
    else:
        raise ValueError(f"Unknown provider: {provider}")
