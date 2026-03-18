"""Factory for creating agent backends from configuration."""
import os
from pathlib import Path
from typing import Optional

from app.agent.backends.base import Backend
from app.agent.models import get_default_model

MARC27_PLATFORM_URL = "https://api.marc27.com"
MARC27_OPENAI_FALLBACK_URL = "https://api.marc27.com/api/v1"


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


def _has_marc27_sdk_auth() -> bool:
    """Check whether marc27-sdk has usable auth configured."""
    env_token = os.getenv("MARC27_API_KEY") or os.getenv("MARC27_TOKEN")
    env_project = os.getenv("MARC27_PROJECT_ID")
    if env_token and env_project:
        return True
    try:
        from marc27.credentials import CredentialsManager

        creds = CredentialsManager().load()
        return bool(creds and creds.access_token and getattr(creds, "project_id", None))
    except Exception:
        return False


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
        if _has_marc27_sdk_auth() or _load_marc27_token():
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
        token = os.getenv("MARC27_API_KEY") or _load_marc27_token()
        if token and not os.getenv("MARC27_API_KEY"):
            os.environ["MARC27_API_KEY"] = token
        platform_url = os.getenv("MARC27_PLATFORM_URL", MARC27_PLATFORM_URL)
        os.environ.setdefault("MARC27_PLATFORM_URL", platform_url)
        try:
            from app.agent.backends.marc27_backend import Marc27Backend

            return Marc27Backend(
                model=model or get_default_model("marc27"),
                api_key=token,
                project_id=os.getenv("MARC27_PROJECT_ID"),
                platform_url=platform_url,
            )
        except ImportError:
            from app.agent.backends.openai_backend import OpenAIBackend

            if not token:
                raise ValueError(
                    "MARC27 auth missing. Set MARC27_API_KEY (preferred) or "
                    "MARC27_TOKEN. For native integration install marc27-sdk."
                )
            return OpenAIBackend(
                model=model or get_default_model("marc27"),
                base_url=os.getenv("MARC27_OPENAI_BASE_URL", MARC27_OPENAI_FALLBACK_URL),
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
        return OpenAIBackend(
            model=model or os.getenv("PRISM_MODEL") or get_default_model("openrouter"),
            base_url="https://openrouter.ai/api/v1",
            api_key=os.getenv("OPENROUTER_API_KEY"),
        )
    else:
        raise ValueError(f"Unknown provider: {provider}")
