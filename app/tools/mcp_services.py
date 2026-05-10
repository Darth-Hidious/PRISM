"""MCP-services tools — thin wrappers over MARC27 platform
`/projects/{project_id}/mcp-services` endpoints.

Closes the GAP-HIGH `/mcp-services` family from the v2.7.2
endpoint-coverage audit. These endpoints expose platform-hosted MCP server
instances (running in the platform, not locally) so agents can list them,
proxy requests through them, and scale them up/down.

Two tools, mirroring the compute / compute_submit and calphad /
calphad_compute split:

  - `mcp_services`        — list + get instance info. Read-only.
                            No approval gate.
                            Actions: 'list', 'get'.

  - `mcp_services_invoke` — proxy a request through an instance, or scale
                            an instance up/down. BOTH state-changing and
                            potentially money-spending. requires_approval=True.
                            Actions: 'proxy', 'scale'.

`project_id` is required for all four actions. Default resolution order:
  1. explicit `project_id` arg
  2. `MARC27_PROJECT_ID` env var
  3. `~/.prism/credentials.json` `project_id` field

Endpoint coverage:
  - GET  /projects/{project_id}/mcp-services
       → mcp_services(action='list')
  - GET  /projects/{project_id}/mcp-services/{instance_id}
       → mcp_services(action='get')
  - POST /projects/{project_id}/mcp-services/{instance_id}/proxy
       → mcp_services_invoke(action='proxy')
  - POST /projects/{project_id}/mcp-services/{instance_id}/scale
       → mcp_services_invoke(action='scale')
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any, Optional

import requests

from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# Shared auth (mirror of platform_status.py:_resolve_credentials).
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


def _resolve_project_id(explicit: Optional[str]) -> Optional[str]:
    """Resolve project_id from arg → env → credentials.json. Returns None
    if no source produces a value."""
    if explicit:
        return explicit
    env_val = os.environ.get("MARC27_PROJECT_ID")
    if env_val:
        return env_val
    try:
        creds_path = Path.home() / ".prism" / "credentials.json"
        if creds_path.exists():
            creds = json.loads(creds_path.read_text())
            pid = creds.get("project_id")
            if pid:
                return pid
    except Exception:
        pass
    return None


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
            # Proxy requests can ride a 60s upstream timeout; give the
            # platform some headroom over that.
            timeout=75,
        )
        if resp.status_code not in (200, 201):
            return {
                "error": f"platform returned HTTP {resp.status_code}",
                "body": resp.text[:500],
            }
        return resp.json()
    except requests.exceptions.RequestException as e:
        return {"error": f"network error: {e}"}


# ---------------------------------------------------------------------------
# Read-only dispatcher
# ---------------------------------------------------------------------------

def _mcp_services(**kwargs: Any) -> dict:
    """Read-only dispatcher. No approval gate.

    Actions:
      • list — GET /projects/{project_id}/mcp-services
      • get  — GET /projects/{project_id}/mcp-services/{instance_id}
    """
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: list, get",
            "hint": (
                "mcp_services(action='list') / "
                "mcp_services(action='get', instance_id='...')"
            ),
        }

    project_id = _resolve_project_id(kwargs.get("project_id"))
    if not project_id:
        return {
            "error": "No `project_id` resolved.",
            "hint": (
                "Pass project_id=… explicitly, set MARC27_PROJECT_ID env var, "
                "or run `prism login` so ~/.prism/credentials.json carries it."
            ),
        }

    if action == "list":
        return _get(f"/projects/{project_id}/mcp-services")
    if action == "get":
        instance_id = kwargs.get("instance_id")
        if not instance_id:
            return {"error": "Action 'get' requires `instance_id`"}
        return _get(f"/projects/{project_id}/mcp-services/{instance_id}")
    return {"error": f"Unknown action '{action}'. Valid: list, get"}


# ---------------------------------------------------------------------------
# Approval-gated dispatcher
# ---------------------------------------------------------------------------

def _mcp_services_invoke(**kwargs: Any) -> dict:
    """Approval-gated dispatcher.

    Actions:
      • proxy — POST /projects/{project_id}/mcp-services/{instance_id}/proxy
      • scale — POST /projects/{project_id}/mcp-services/{instance_id}/scale
    """
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: proxy, scale",
            "hint": (
                "mcp_services_invoke(action='proxy', instance_id='...', path='/...') / "
                "mcp_services_invoke(action='scale', instance_id='...', replicas=0|1)"
            ),
        }

    project_id = _resolve_project_id(kwargs.get("project_id"))
    if not project_id:
        return {
            "error": "No `project_id` resolved.",
            "hint": (
                "Pass project_id=… explicitly, set MARC27_PROJECT_ID env var, "
                "or run `prism login` so ~/.prism/credentials.json carries it."
            ),
        }

    instance_id = kwargs.get("instance_id")
    if not instance_id:
        return {"error": f"Action '{action}' requires `instance_id`"}

    if action == "proxy":
        path = kwargs.get("path")
        if not path:
            return {"error": "Action 'proxy' requires `path` (upstream URL path)"}
        body: dict = {"path": path}
        if kwargs.get("method"):
            body["method"] = kwargs["method"]
        if kwargs.get("body") is not None:
            body["body"] = kwargs["body"]
        if kwargs.get("headers") is not None:
            body["headers"] = kwargs["headers"]
        return _post(
            f"/projects/{project_id}/mcp-services/{instance_id}/proxy",
            body,
        )

    if action == "scale":
        if "replicas" not in kwargs:
            return {"error": "Action 'scale' requires `replicas` (0 or 1)"}
        try:
            replicas = int(kwargs["replicas"])
        except (TypeError, ValueError):
            return {"error": "`replicas` must be an integer (0 or 1)"}
        if replicas not in (0, 1):
            return {
                "error": f"`replicas` must be 0 or 1 (got {replicas}); v1 does not support >1.",
            }
        return _post(
            f"/projects/{project_id}/mcp-services/{instance_id}/scale",
            {"replicas": replicas},
        )

    return {"error": f"Unknown action '{action}'. Valid: proxy, scale"}


# ---------------------------------------------------------------------------
# Tool descriptions
# ---------------------------------------------------------------------------

_MCP_SERVICES_DESCRIPTION = (
    "Read MARC27 platform-hosted MCP service instances (Model Context "
    "Protocol servers running in the platform, not locally). Read-only "
    "— NO approval gate. ONE tool, two actions:\n"
    "  • action='list' — list all MCP service instances for the active "
    "project. Returns id, status, endpoint_url, health info per instance.\n"
    "  • action='get' — fetch a single instance with current health. "
    "Required: `instance_id`.\n"
    "`project_id` is auto-resolved (env MARC27_PROJECT_ID or "
    "~/.prism/credentials.json) but can be overridden explicitly.\n"
    "To PROXY a request through an instance or SCALE it, use the separate "
    "`mcp_services_invoke` tool — that one is approval-gated because "
    "proxying executes upstream actions and scaling toggles compute usage."
)


_MCP_SERVICES_INVOKE_DESCRIPTION = (
    "Invoke or scale a MARC27 platform-hosted MCP service instance. "
    "BOTH actions are state-changing — proxying executes an upstream "
    "request, scaling toggles compute. requires_approval=True; the "
    "harness will prompt before each call.\n"
    "  • action='proxy' — forward an HTTP request to the instance's "
    "endpoint. Required: `instance_id`, `path` (upstream URL path). "
    "Optional: `method` (default 'POST'), `body` (JSON), `headers` "
    "(dict). Returns `{status_code, body, headers}` from the upstream.\n"
    "  • action='scale' — set instance replica count. Required: "
    "`instance_id`, `replicas` (0 or 1). v1 does NOT support >1 "
    "replicas. Use replicas=0 to stop an instance, replicas=1 to start.\n"
    "Use `mcp_services(action='list')` first to discover instance IDs."
)


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

def create_mcp_services_tools(registry: ToolRegistry) -> None:
    """Register the `mcp_services` (read) + `mcp_services_invoke`
    (proxy + scale) tools."""
    registry.register(Tool(
        name="mcp_services",
        description=_MCP_SERVICES_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get"],
                    "description": "Which read operation.",
                },
                "project_id": {
                    "type": "string",
                    "description": "Optional project UUID. Defaults to env or credentials.json.",
                },
                "instance_id": {
                    "type": "string",
                    "description": "MCP service instance UUID. Required for action='get'.",
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_mcp_services,
    ))

    registry.register(Tool(
        name="mcp_services_invoke",
        description=_MCP_SERVICES_INVOKE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["proxy", "scale"],
                    "description": "Proxy a request or scale the instance.",
                },
                "project_id": {
                    "type": "string",
                    "description": "Optional project UUID. Defaults to env or credentials.json.",
                },
                "instance_id": {
                    "type": "string",
                    "description": "MCP service instance UUID. Required.",
                },
                "path": {
                    "type": "string",
                    "description": "Upstream URL path for action='proxy'.",
                },
                "method": {
                    "type": "string",
                    "description": "HTTP method for action='proxy'. Default 'POST'.",
                },
                "body": {
                    "type": "object",
                    "description": "JSON request body for action='proxy'.",
                    "additionalProperties": True,
                },
                "headers": {
                    "type": "object",
                    "description": "Extra HTTP headers for action='proxy'.",
                    "additionalProperties": {"type": "string"},
                },
                "replicas": {
                    "type": "integer",
                    "description": "Replica count (0 or 1) for action='scale'.",
                },
            },
            "required": ["action", "instance_id"],
            "additionalProperties": False,
        },
        func=_mcp_services_invoke,
        requires_approval=True,
    ))
