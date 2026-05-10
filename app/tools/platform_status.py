"""Platform-status tools — thin wrappers over MARC27 platform read endpoints.

Three high-leverage gaps from the v2.7.2 endpoint-coverage audit get
filled here, all *read-only*:

  - policy_evaluate  → POST /policy/evaluate    (audit gap: agent had no
                                                 way to ask "am I allowed?")
  - usage_status     → GET /usage/me + /usage/projects/{id}
                                                (agent had no way to
                                                 self-budget compute spend)
  - billing_balance  → GET /billing/balance + /billing/usage + /billing/prices
                                                (agent had no way to read
                                                 wallet balance or prices)

All three use the same auth path as `research.py` — `MARC27_API_KEY` env
var or `~/.prism/credentials.json` access_token. None of them mutate
platform state, so none are `requires_approval=True`.

The matrix in the v2.7.2 endpoint-coverage audit lists more GAP-HIGH
endpoints (compute/, jobs/, knowledge/embed, mcp-services, etc.) — those
need their own tools and are tracked separately.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any, Optional

import requests

from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# Shared auth (mirror of research.py:_resolve_credentials, kept private here
# so each tool module can evolve its own auth handling later if needed).
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


def _post(path: str, body: dict) -> dict:
    api_url, token = _resolve_credentials()
    if not token:
        return {
            "error": "Not authenticated to the MARC27 platform.",
            "hint": "Run `prism login` first, then retry.",
        }
    try:
        resp = requests.post(
            f"{api_url}{path}",
            headers={
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/json",
            },
            json=body,
            timeout=15,
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
# policy_evaluate
# ---------------------------------------------------------------------------

def _policy_evaluate(action: Optional[str] = None,
                     resource: Optional[str] = None,
                     context: Optional[dict] = None,
                     **_kwargs: Any) -> dict:
    if not action:
        return {"error": "Missing required argument `action`."}
    body: dict = {"action": action}
    if resource is not None:
        body["resource"] = resource
    if context is not None:
        body["context"] = context
    return _post("/policy/evaluate", body)


_POLICY_EVALUATE_DESCRIPTION = (
    "Ask the MARC27 platform's policy engine whether a given action on a "
    "given resource is permitted for the current user. Use this BEFORE "
    "running anything privileged (compute submit, dataset publish, "
    "cross-org access) so you don't waste a turn on a 403.\n"
    "  • action: required. e.g. 'compute.submit', 'dataset.export', "
    "'mesh.subscribe'.\n"
    "  • resource: optional. e.g. 'gpu://provider/h100', "
    "'dataset://tokyo/cfd-2024'.\n"
    "  • context: optional dict of action-specific extras.\n"
    "Returns `{allowed: bool, reason?: str, ...}`. No state change; "
    "no approval gate."
)


# ---------------------------------------------------------------------------
# usage_status
# ---------------------------------------------------------------------------

def _usage_status(project_id: Optional[str] = None, **_kwargs: Any) -> dict:
    """Return either user-level usage (default) or project-level usage."""
    if project_id:
        return _get(f"/usage/projects/{project_id}")
    return _get("/usage/me")


_USAGE_STATUS_DESCRIPTION = (
    "Read current MARC27 platform usage telemetry for the user or a "
    "specific project. Useful for budgeting decisions BEFORE submitting "
    "expensive compute jobs.\n"
    "  • No args: returns the authenticated user's aggregate usage "
    "(GET /usage/me).\n"
    "  • project_id: returns usage scoped to that project "
    "(GET /usage/projects/{id}).\n"
    "Returns counters for tokens, compute time, dataset IO, etc. "
    "depending on the platform's current schema. No state change."
)


# ---------------------------------------------------------------------------
# billing_balance
# ---------------------------------------------------------------------------

def _billing_balance(action: str = "balance", **_kwargs: Any) -> dict:
    """Read wallet balance, recent usage charges, or current prices."""
    action = (action or "balance").lower()
    if action == "balance":
        return _get("/billing/balance")
    if action == "usage":
        return _get("/billing/usage")
    if action == "prices":
        return _get("/billing/prices")
    return {
        "error": f"Unknown action '{action}'. Valid: balance, usage, prices.",
    }


_BILLING_BALANCE_DESCRIPTION = (
    "Read MARC27 platform billing state. Read-only — never tops up, "
    "never charges. ONE tool, three actions:\n"
    "  • action='balance' (default) — current wallet balance + plan tier "
    "(GET /billing/balance).\n"
    "  • action='usage' — historical usage charges (GET /billing/usage).\n"
    "  • action='prices' — current per-unit prices for compute / inference "
    "(GET /billing/prices). Useful before estimating job cost.\n"
    "No state change; no approval gate. For top-up the human runs "
    "`prism billing topup` interactively."
)


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

def create_platform_status_tools(registry: ToolRegistry) -> None:
    """Register the three platform-read tools (policy / usage / billing)."""
    registry.register(Tool(
        name="policy_evaluate",
        description=_POLICY_EVALUATE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Action verb to check, e.g. 'compute.submit'.",
                },
                "resource": {
                    "type": "string",
                    "description": "Optional resource handle.",
                },
                "context": {
                    "type": "object",
                    "description": "Optional action-specific context.",
                    "additionalProperties": True,
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_policy_evaluate,
    ))

    registry.register(Tool(
        name="usage_status",
        description=_USAGE_STATUS_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "project_id": {
                    "type": "string",
                    "description": "Optional project UUID to scope the query.",
                },
            },
            "additionalProperties": False,
        },
        func=_usage_status,
    ))

    registry.register(Tool(
        name="billing_balance",
        description=_BILLING_BALANCE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["balance", "usage", "prices"],
                    "description": "Which billing read to perform.",
                },
            },
            "additionalProperties": False,
        },
        func=_billing_balance,
    ))
