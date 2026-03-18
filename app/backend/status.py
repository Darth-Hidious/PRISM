"""Status checks for the welcome bar.

Four checks:
1. LLM — is an LLM provider configured and reachable?
2. Plugins — placeholder for MARC27 SDK (coming soon)
3. Commands — are key tools registered + providers healthy?
4. Skills — how many skills are loaded?
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Optional


# ── 1. LLM check ────────────────────────────────────────────────────

def detect_llm() -> dict:
    """Check if an LLM provider is configured.

    Returns {"connected": bool, "provider": str | None}.
    """
    providers = [
        ("MARC27_API_KEY", "MARC27"),
        ("MARC27_TOKEN", "MARC27"),
        ("ANTHROPIC_API_KEY", "Claude"),
        ("OPENAI_API_KEY", "OpenAI"),
        ("OPENROUTER_API_KEY", "OpenRouter"),
        ("ZHIPU_API_KEY", "Zhipu"),
    ]
    for env_var, name in providers:
        if os.getenv(env_var):
            return {"connected": True, "provider": name}

    # Check MARC27 token file
    token_path = Path.home() / ".prism" / "marc27_token"
    if token_path.exists() and token_path.read_text().strip():
        return {"connected": True, "provider": "MARC27"}

    # Check MARC27 SDK credentials file
    try:
        from marc27.credentials import CredentialsManager

        creds = CredentialsManager().load()
        if creds and creds.access_token:
            return {"connected": True, "provider": "MARC27"}
    except Exception:
        pass

    return {"connected": False, "provider": None}


# ── 2. Plugins check (placeholder for MARC27 SDK) ───────────────────

def detect_plugins() -> dict:
    """Check installed plugins.

    Returns {"count": int, "available": bool, "names": list[str]}.
    Currently reads local catalog only. Will use MARC27 SDK when ready.
    """
    try:
        from app.plugins.catalog import PluginCatalog
        catalog = PluginCatalog()
        names = [p.name for p in catalog.list_installed()]
        return {"count": len(names), "available": True, "names": names}
    except Exception:
        pass

    # Fallback: count plugin files
    try:
        catalog_path = Path(__file__).parent.parent / "plugins" / "catalog.json"
        if catalog_path.exists():
            data = json.loads(catalog_path.read_text())
            plugins = data.get("plugins", [])
            return {"count": len(plugins), "available": True, "names": []}
    except Exception:
        pass

    return {"count": 0, "available": False, "names": []}


# ── 3. Command / tool health check ──────────────────────────────────

def detect_commands(tool_registry=None) -> dict:
    """Check which key commands/tools are registered and healthy.

    Returns {"tools": [{"name": str, "registered": bool, "healthy": bool | None}], "total": int}.

    Uses cached provider health (no network calls).
    """
    # Key tools users care about
    key_tools = ["search_materials", "query_materials_project", "literature_search",
                 "predict_property", "execute_python", "web_search"]

    registered_names = set()
    if tool_registry:
        registered_names = {t.name for t in tool_registry.list_tools()}

    # Load cached provider health
    health_path = Path.home() / ".prism" / "cache" / "provider_health.json"
    provider_health = {}
    try:
        if health_path.exists():
            provider_health = json.loads(health_path.read_text())
    except Exception:
        pass

    # Count healthy providers (circuit_state == "closed")
    healthy_providers = sum(
        1 for h in provider_health.values()
        if isinstance(h, dict) and h.get("circuit_state") == "closed"
    )
    total_providers = len(provider_health)

    tools = []
    for name in key_tools:
        registered = name in registered_names
        # For search_materials, use provider health
        healthy = None
        if name == "search_materials" and registered:
            healthy = healthy_providers > 0
        elif registered:
            healthy = True  # non-network tools are healthy if registered
        tools.append({"name": name, "registered": registered, "healthy": healthy})

    return {
        "tools": tools,
        "total": len(registered_names),
        "healthy_providers": healthy_providers,
        "total_providers": total_providers,
    }


# ── 4. Skills check ─────────────────────────────────────────────────

def detect_skills() -> dict:
    """Check loaded skills.

    Returns {"count": int, "names": list[str]}.
    """
    try:
        from app.skills.registry import load_builtin_skills
        registry = load_builtin_skills()
        skills = registry.list_skills()
        # list_skills() may return Skill objects — extract names as strings
        names = [str(getattr(s, "name", s)) for s in skills]
        return {"count": len(names), "names": names}
    except Exception:
        return {"count": 0, "names": []}


def detect_account() -> dict:
    """Detect active platform account context propagated by the Rust launcher."""
    user_id = os.getenv("PRISM_ACCOUNT_USER_ID")
    display_name = os.getenv("PRISM_ACCOUNT_DISPLAY_NAME")
    org_id = os.getenv("PRISM_ACCOUNT_ORG_ID")
    org_name = os.getenv("PRISM_ACCOUNT_ORG_NAME")
    project_id = os.getenv("PRISM_ACCOUNT_PROJECT_ID") or os.getenv("MARC27_PROJECT_ID")
    project_name = os.getenv("PRISM_ACCOUNT_PROJECT_NAME")
    platform_url = os.getenv("MARC27_PLATFORM_URL")

    signed_in = bool(os.getenv("MARC27_TOKEN") or user_id or display_name or project_id)
    return {
        "signed_in": signed_in,
        "user_id": user_id,
        "display_name": display_name,
        "org_id": org_id,
        "org_name": org_name,
        "project_id": project_id,
        "project_name": project_name,
        "platform_url": platform_url,
    }


# ── Aggregate ────────────────────────────────────────────────────────

def build_status(tool_registry=None) -> dict:
    """Build the full status payload for ui.welcome."""
    return {
        "llm": detect_llm(),
        "plugins": detect_plugins(),
        "commands": detect_commands(tool_registry),
        "skills": detect_skills(),
        "account": detect_account(),
    }
