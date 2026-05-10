"""agent_capabilities tool — wrapper over MARC27 platform self-discovery.

Closes the `/agent/capabilities` GAP-HIGH item from the v2.7.2
endpoint-coverage audit. One read-only tool, no approval gate.

  - agent_capabilities  → GET /agent/capabilities
                          (returns the platform's self-describing
                           descriptor: route names, models, services,
                           GraphQL schema hints, auth options, etc.)

Auth path mirrors `platform_status.py` — `MARC27_API_KEY` env var or
`~/.prism/credentials.json` access_token. The platform endpoint itself
requires no auth (see marc27-core/.../agent_guide.rs), but we still
attach the bearer token when one is present so the tool behaves
identically to its sibling tools, and we surface a clean "Not
authenticated" hint when no creds are configured at all.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import requests

from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# Shared auth (mirror of platform_status.py:_resolve_credentials, kept
# private here so each tool module can evolve its own auth handling later
# if needed).
# ---------------------------------------------------------------------------

def _resolve_credentials() -> tuple[str, str]:
    """Return (api_url, access_token) from env or `~/.prism/credentials.json`."""
    api_url = os.environ.get(
        "MARC27_API_URL", "https://api.marc27.com/api/v1"
    ).rstrip("/")
    api_key = os.environ.get("MARC27_API_KEY", "")

    if not api_key:
        try:
            creds_path = Path.home() / ".prism" / "credentials.json"
            if creds_path.exists():
                creds = json.loads(creds_path.read_text())
                api_key = creds.get("access_token", "")
                if creds.get("platform_url"):
                    api_url = creds["platform_url"].rstrip("/")
                    if not api_url.endswith("/api/v1"):
                        api_url = api_url + "/api/v1"
        except Exception:
            pass

    return api_url, api_key


def _get(path: str) -> dict:
    """GET helper. Returns dict with 'error' on failure, parsed JSON otherwise."""
    api_url, token = _resolve_credentials()
    if not token:
        return {
            "error": "Not authenticated to the MARC27 platform.",
            "hint": "Run `prism login` first, then retry.",
        }
    try:
        resp = requests.get(
            f"{api_url}{path}",
            headers={"Authorization": f"Bearer {token}"},
            timeout=10,
        )
        if resp.status_code != 200:
            return {
                "error": f"platform returned HTTP {resp.status_code}",
                "body": resp.text[:500],
            }
        return resp.json()
    except requests.exceptions.RequestException as e:
        return {"error": f"network error: {e}"}


# ---------------------------------------------------------------------------
# agent_capabilities
# ---------------------------------------------------------------------------

def _agent_capabilities(**_kwargs: Any) -> dict:
    """Fetch the platform's self-discovery descriptor."""
    return _get("/agent/capabilities")


_AGENT_CAPABILITIES_DESCRIPTION = (
    "Ask the MARC27 platform to describe itself. Returns a structured "
    "self-discovery descriptor: which REST routes exist (grouped by "
    "service), which auth methods are accepted, which GraphQL queries "
    "and mutations are available, and CLI quick-start hints.\n"
    "Use this when you need to discover whether a capability exists "
    "before crafting a request — it's cheaper than guessing and "
    "handling a 404. No arguments. Read-only; no approval gate."
)


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

def create_agent_capabilities_tool(registry: ToolRegistry) -> None:
    """Register the agent_capabilities self-discovery tool."""
    registry.register(Tool(
        name="agent_capabilities",
        description=_AGENT_CAPABILITIES_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        },
        func=_agent_capabilities,
    ))
