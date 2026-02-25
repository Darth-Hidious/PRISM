"""Factory for creating agent backends from configuration."""
import os
from pathlib import Path
from typing import Optional
from app.agent.backends.base import Backend

MARC27_API_URL = "https://api.marc27.com/v1"


def _load_marc27_token() -> Optional[str]:
    """Load MARC27 token from env or ~/.prism/marc27_token."""
    token = os.getenv("MARC27_TOKEN")
    if token:
        return token
    token_path = Path.home() / ".prism" / "marc27_token"
    if token_path.exists():
        token = token_path.read_text().strip()
        if token:
            os.environ["MARC27_TOKEN"] = token
            return token
    return None


def create_backend(provider: Optional[str] = None, model: Optional[str] = None) -> Backend:
    """Create an LLM backend.

    Resolution order for provider/model:
      1. Explicit arguments (from CLI flags)
      2. settings.json (agent.provider / agent.model)
      3. Environment variables (API keys)
      4. Error if nothing configured
    """
    from app.config.settings_schema import get_settings
    settings = get_settings()

    # Resolve model from settings if not explicitly provided
    if model is None and settings.agent.model:
        model = settings.agent.model

    # Resolve provider from settings, then env vars
    if provider is None and settings.agent.provider:
        provider = settings.agent.provider

    if provider is None:
        if _load_marc27_token():
            provider = "marc27"
        elif os.getenv("ANTHROPIC_API_KEY"):
            provider = "anthropic"
        elif os.getenv("OPENAI_API_KEY"):
            provider = "openai"
        elif os.getenv("OPENROUTER_API_KEY"):
            provider = "openrouter"
        else:
            raise ValueError("No LLM provider configured. Set ANTHROPIC_API_KEY, OPENAI_API_KEY, OPENROUTER_API_KEY, or run /login for MARC27.")

    if provider == "marc27":
        from app.agent.backends.openai_backend import OpenAIBackend
        token = _load_marc27_token()
        if not token:
            raise ValueError("MARC27 token not found. Run /login in the REPL or set MARC27_TOKEN.")
        return OpenAIBackend(
            model=model or "claude-3.5-sonnet",
            base_url=MARC27_API_URL,
            api_key=token,
        )
    elif provider == "anthropic":
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
