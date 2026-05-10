"""Platform-workflows tools — thin wrappers over MARC27 platform `/workflows`
endpoints.

Closes the GAP-HIGH `/workflows` family from the v2.7.2 endpoint-coverage audit.

Two tools, mirroring the compute / compute_submit and calphad /
calphad_compute split:

  - `platform_workflows`     — list + list_specs + status + cancel.
                               NOT money-spending. No approval gate.
                               Actions: 'list', 'list_specs', 'status',
                               'cancel'.

  - `platform_workflows_run` — start a workflow + register a new spec.
                               BOTH state-changing; `start` actually
                               spends compute budget. requires_approval=True.
                               Actions: 'start', 'register_spec'.

Endpoint coverage:
  - GET  /workflows          → platform_workflows(action='list')
  - GET  /workflows/specs    → platform_workflows(action='list_specs')
  - POST /workflows/specs    → platform_workflows_run(action='register_spec')
  - POST /workflows          → platform_workflows_run(action='start')
  - GET  /workflows/{id}     → platform_workflows(action='status')
  - POST /workflows/{id}/cancel → platform_workflows(action='cancel')
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
        # /workflows/* return 202 Accepted (start), 201 Created (spec),
        # 204 No Content (cancel), or 200 OK depending on action.
        if resp.status_code not in (200, 201, 202, 204):
            return {
                "error": f"platform returned HTTP {resp.status_code}",
                "body": resp.text[:500],
            }
        if resp.status_code == 204 or not resp.content:
            return {"status": "ok", "http_status": resp.status_code}
        try:
            return resp.json()
        except ValueError:
            return {"status": "ok", "http_status": resp.status_code, "body": resp.text[:500]}
    except requests.exceptions.RequestException as e:
        return {"error": f"network error: {e}"}


# ---------------------------------------------------------------------------
# Read / cancel dispatcher
# ---------------------------------------------------------------------------

def _platform_workflows(**kwargs: Any) -> dict:
    """Read/cancel dispatcher. No approval gate.

    Actions:
      • list       — GET /workflows                (instances)
      • list_specs — GET /workflows/specs          (registered spec catalog)
      • status     — GET /workflows/{id}           (single instance)
      • cancel     — POST /workflows/{id}/cancel
    """
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: list, list_specs, status, cancel",
            "hint": (
                "platform_workflows(action='list') / "
                "platform_workflows(action='list_specs') / "
                "platform_workflows(action='status', workflow_id='...') / "
                "platform_workflows(action='cancel', workflow_id='...')"
            ),
        }

    if action == "list":
        return _get("/workflows")
    if action == "list_specs":
        return _get("/workflows/specs")
    if action == "status":
        workflow_id = kwargs.get("workflow_id")
        if not workflow_id:
            return {"error": "Action 'status' requires `workflow_id`"}
        return _get(f"/workflows/{workflow_id}")
    if action == "cancel":
        workflow_id = kwargs.get("workflow_id")
        if not workflow_id:
            return {"error": "Action 'cancel' requires `workflow_id`"}
        return _post(f"/workflows/{workflow_id}/cancel", {})
    return {
        "error": f"Unknown action '{action}'. Valid: list, list_specs, status, cancel"
    }


# ---------------------------------------------------------------------------
# Run / register_spec dispatcher (approval-gated)
# ---------------------------------------------------------------------------

def _platform_workflows_run(**kwargs: Any) -> dict:
    """Approval-gated dispatcher.

    Actions:
      • start         — POST /workflows
      • register_spec — POST /workflows/specs
    """
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: start, register_spec",
            "hint": (
                "platform_workflows_run(action='start', spec='...', inputs={...}) / "
                "platform_workflows_run(action='register_spec', spec_yaml='...')"
            ),
        }

    if action == "start":
        spec = kwargs.get("spec")
        if not spec:
            return {"error": "Action 'start' requires `spec` (slug or UUID)"}
        body: dict = {"spec": spec}
        if kwargs.get("inputs") is not None:
            body["inputs"] = kwargs["inputs"]
        if kwargs.get("project_id"):
            body["project_id"] = kwargs["project_id"]
        return _post("/workflows", body)

    if action == "register_spec":
        spec_yaml = kwargs.get("spec_yaml")
        if not spec_yaml:
            return {"error": "Action 'register_spec' requires `spec_yaml`"}
        body = {"spec_yaml": spec_yaml}
        if kwargs.get("access"):
            body["access"] = kwargs["access"]
        if kwargs.get("org_id"):
            body["org_id"] = kwargs["org_id"]
        return _post("/workflows/specs", body)

    return {"error": f"Unknown action '{action}'. Valid: start, register_spec"}


# ---------------------------------------------------------------------------
# Tool descriptions
# ---------------------------------------------------------------------------

_PLATFORM_WORKFLOWS_DESCRIPTION = (
    "Read/cancel MARC27 platform workflows. Read-only and cancel — NOT "
    "money-spending. ONE tool, four actions:\n"
    "  • action='list' — list the user's workflow instances.\n"
    "  • action='list_specs' — list available workflow specs (org + public + "
    "marketplace) the user can run.\n"
    "  • action='status' — fetch a single workflow instance state. Required: "
    "`workflow_id`.\n"
    "  • action='cancel' — cancel a running or queued workflow. Required: "
    "`workflow_id`. Idempotent.\n"
    "To START a workflow or REGISTER a new spec, use the separate "
    "`platform_workflows_run` tool — that one is approval-gated because "
    "starting a workflow spends compute budget and registering a spec "
    "publishes content."
)


_PLATFORM_WORKFLOWS_RUN_DESCRIPTION = (
    "Start a workflow or register a new workflow spec. BOTH actions are "
    "state-changing; `start` actually spends compute budget. "
    "requires_approval=True; the harness will prompt before each call.\n"
    "  • action='start' — kick off a workflow run. Required: `spec` "
    "(slug or UUID). Optional: `inputs` (dict of input bindings), "
    "`project_id` (UUID, scopes the run + billing).\n"
    "  • action='register_spec' — publish a new YAML workflow spec. "
    "Required: `spec_yaml` (the raw YAML text). Optional: `access` "
    "('public', 'org', 'private'; default 'private'), `org_id` (UUID, "
    "required if access='org').\n"
    "Use `platform_workflows(action='list_specs')` first to discover "
    "what's already published before running or registering."
)


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

def create_platform_workflows_tools(registry: ToolRegistry) -> None:
    """Register the `platform_workflows` (read/cancel) +
    `platform_workflows_run` (start + register_spec) tools."""
    registry.register(Tool(
        name="platform_workflows",
        description=_PLATFORM_WORKFLOWS_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "list_specs", "status", "cancel"],
                    "description": "Which workflow read/cancel operation.",
                },
                "workflow_id": {
                    "type": "string",
                    "description": "Workflow instance UUID. Required for status/cancel.",
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_platform_workflows,
    ))

    registry.register(Tool(
        name="platform_workflows_run",
        description=_PLATFORM_WORKFLOWS_RUN_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "register_spec"],
                    "description": "Start a workflow or register a new spec.",
                },
                "spec": {
                    "type": "string",
                    "description": "Spec slug or UUID. Required for action='start'.",
                },
                "inputs": {
                    "type": "object",
                    "description": "Input bindings for action='start'.",
                    "additionalProperties": True,
                },
                "project_id": {
                    "type": "string",
                    "description": "Project UUID for action='start'.",
                },
                "spec_yaml": {
                    "type": "string",
                    "description": "Raw YAML spec text. Required for action='register_spec'.",
                },
                "access": {
                    "type": "string",
                    "enum": ["public", "org", "private"],
                    "description": "Visibility for action='register_spec'. Default 'private'.",
                },
                "org_id": {
                    "type": "string",
                    "description": "Org UUID for action='register_spec' with access='org'.",
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_platform_workflows_run,
        requires_approval=True,
    ))
