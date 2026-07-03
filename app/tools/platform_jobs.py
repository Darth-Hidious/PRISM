"""Platform-jobs tools — thin wrappers over MARC27 platform `/jobs` endpoints.

Closes the GAP-HIGH `/jobs` family from the v2.7.2 endpoint-coverage audit.

Two tools, mirroring the compute / compute_submit and calphad /
calphad_compute split:

  - `platform_jobs`        — read + cancel + events. NOT money-spending.
                             No approval gate.
                             Actions: 'status', 'cancel', 'events'.

  - `platform_jobs_submit` — submit a new job. MONEY-SPENDING.
                             requires_approval=True.

Auth path mirrors `app/tools/platform_status.py` exactly: `MARC27_API_KEY`
env var with `~/.prism/credentials.json` access_token fallback.

Endpoint coverage:
  - POST /jobs                  → platform_jobs_submit
  - GET  /jobs/{id}             → platform_jobs(action='status')
  - POST /jobs/{id}/cancel      → platform_jobs(action='cancel')
  - GET  /jobs/{id}/events      → platform_jobs(action='events')  (SSE)
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any, Optional

import requests

from app.tools._platform_client import platform
from app.tools._platform_creds import resolve_platform_auth

from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# Shared auth (mirror of platform_status.py:_resolve_credentials, kept private
# here so each tool module can evolve its own auth handling later if needed).
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
    return platform().get(path, timeout=10)


def _post(path: str, body: dict) -> dict:
    return platform().post(path, json=body, timeout=15)


def _get_sse(path: str, max_events: int = 10, read_timeout: int = 30) -> dict:
    """Read up to `max_events` SSE frames from a streaming endpoint, then return.

    Used for `GET /jobs/{id}/events`. The platform stream stays open for the
    job's lifetime; the agent doesn't want to block forever, so we cap
    at N events or `read_timeout` seconds, whichever comes first.
    """
    api_url, auth_headers = resolve_platform_auth()
    if not auth_headers:
        return {
            "error": "Not authenticated to the MARC27 platform.",
            "hint": "Run `prism login` first, then retry.",
        }
    try:
        resp = requests.get(
            f"{api_url}{path}",
            headers={**auth_headers, "Accept": "text/event-stream"},
            stream=True,
            timeout=(10, read_timeout),
        )
        if resp.status_code != 200:
            return {
                "error": f"platform returned HTTP {resp.status_code}",
                "body": resp.text[:500],
            }

        events: list[dict] = []
        current_event: dict[str, str] = {}
        for raw in resp.iter_lines(decode_unicode=True):
            if raw is None:
                continue
            line = raw.strip("\r")
            if line == "":
                # blank line = dispatch event
                if current_event:
                    events.append(dict(current_event))
                    current_event = {}
                    if len(events) >= max_events:
                        break
                continue
            if line.startswith(":"):
                # comment / keep-alive — ignore
                continue
            if ":" in line:
                field, _, value = line.partition(":")
                value = value.lstrip(" ")
                if field == "data":
                    # try JSON, fall back to raw text
                    try:
                        current_event["data"] = json.loads(value)
                    except (ValueError, TypeError):
                        current_event["data"] = value
                elif field == "event":
                    current_event["event"] = value
                elif field == "id":
                    current_event["id"] = value
        # close the stream cleanly so the connection isn't leaked
        try:
            resp.close()
        except Exception:
            pass
        return {"events": events, "count": len(events)}
    except requests.exceptions.RequestException as e:
        return {"error": f"network error: {e}"}


# ---------------------------------------------------------------------------
# Read-only / cancel dispatcher
# ---------------------------------------------------------------------------

def _platform_jobs(**kwargs: Any) -> dict:
    """Read/cancel dispatcher. No approval gate.

    Actions:
      • status — GET  /jobs/{id}
      • cancel — POST /jobs/{id}/cancel
      • events — GET  /jobs/{id}/events  (SSE; reads up to `max_events`)
    """
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: status, cancel, events",
            "hint": (
                "platform_jobs(action='status', job_id='...') / "
                "platform_jobs(action='cancel', job_id='...') / "
                "platform_jobs(action='events', job_id='...', max_events=10)"
            ),
        }

    job_id = kwargs.get("job_id")
    if not job_id:
        return {"error": f"Action '{action}' requires `job_id`"}

    if action == "status":
        return _get(f"/jobs/{job_id}")
    if action == "cancel":
        return _post(f"/jobs/{job_id}/cancel", {})
    if action == "events":
        max_events = int(kwargs.get("max_events", 10))
        read_timeout = int(kwargs.get("read_timeout", 30))
        return _get_sse(
            f"/jobs/{job_id}/events",
            max_events=max_events,
            read_timeout=read_timeout,
        )
    return {"error": f"Unknown action '{action}'. Valid: status, cancel, events"}


# ---------------------------------------------------------------------------
# Submit (approval-gated)
# ---------------------------------------------------------------------------

def _platform_jobs_submit(**kwargs: Any) -> dict:
    """Submit a new job. Money-spending — approval-gated."""
    job_type = kwargs.get("job_type")
    project_id = kwargs.get("project_id")
    payload = kwargs.get("payload")

    if not job_type:
        return {"error": "Missing required argument `job_type`."}
    if not project_id:
        return {"error": "Missing required argument `project_id`."}
    if payload is None:
        return {"error": "Missing required argument `payload`."}

    body: dict = {
        "job_type": job_type,
        "project_id": project_id,
        "payload": payload,
    }
    if kwargs.get("resource_id"):
        body["resource_id"] = kwargs["resource_id"]
    if "priority" in kwargs:
        body["priority"] = int(kwargs["priority"])
    return _post("/jobs", body)


# ---------------------------------------------------------------------------
# Tool descriptions
# ---------------------------------------------------------------------------

_PLATFORM_JOBS_DESCRIPTION = (
    "Read/cancel MARC27 platform jobs. Read-only and cancel — NOT "
    "money-spending. ONE tool, three actions:\n"
    "  • action='status' — fetch job state. Required: `job_id`.\n"
    "  • action='cancel' — cancel a running or queued job. Required: "
    "`job_id`. Idempotent (cancelling a finished job is a no-op).\n"
    "  • action='events' — read live SSE events for the job. Required: "
    "`job_id`. Optional: `max_events` (default 10), `read_timeout` "
    "seconds (default 30). Returns up to N events then closes the "
    "stream — does NOT block forever.\n"
    "To submit a NEW job, use the separate `platform_jobs_submit` tool — "
    "that one is approval-gated because it spends compute budget."
)


_PLATFORM_JOBS_SUBMIT_DESCRIPTION = (
    "Submit a new job to the MARC27 platform. MONEY/COMPUTE-SPENDING — "
    "requires_approval=True; the harness will prompt before each call.\n"
    "Required:\n"
    "  • job_type — backend job type slug (e.g. 'compute.simulation', "
    "'embedding.batch'; see platform docs).\n"
    "  • project_id — UUID of the project to bill the job against.\n"
    "  • payload — opaque JSON dict the backend job worker consumes.\n"
    "Optional:\n"
    "  • resource_id — UUID of a specific resource (GPU SKU, dataset, "
    "container image) to enforce per-resource quotas against.\n"
    "  • priority — int16, default 0. Higher = sooner.\n"
    "Returns the created `JobRow` (id, status, etc). Use "
    "`platform_jobs(action='status', job_id=...)` to poll progress, "
    "or `platform_jobs(action='events', job_id=...)` for live updates."
)


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

def create_platform_jobs_tools(registry: ToolRegistry) -> None:
    """Register the unified `platform_jobs` (read/cancel) + `platform_jobs_submit`
    tools.

    The split mirrors compute / compute_submit and calphad / calphad_compute:
    money-spending actions stay isolated for per-tool approval gating.
    """
    registry.register(Tool(
        name="platform_jobs",
        description=_PLATFORM_JOBS_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "cancel", "events"],
                    "description": "Which read/cancel operation.",
                },
                "job_id": {
                    "type": "string",
                    "description": "Job UUID. Required for all actions.",
                },
                "max_events": {
                    "type": "integer",
                    "description": "Max SSE events to read for action='events'. Default 10.",
                },
                "read_timeout": {
                    "type": "integer",
                    "description": "SSE read timeout (seconds) for action='events'. Default 30.",
                },
            },
            "required": ["action", "job_id"],
            "additionalProperties": False,
        },
        func=_platform_jobs,
    ))

    registry.register(Tool(
        name="platform_jobs_submit",
        description=_PLATFORM_JOBS_SUBMIT_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "job_type": {
                    "type": "string",
                    "description": "Backend job type slug.",
                },
                "project_id": {
                    "type": "string",
                    "description": "Project UUID to bill against.",
                },
                "payload": {
                    "type": "object",
                    "description": "Job-type-specific JSON payload.",
                    "additionalProperties": True,
                },
                "resource_id": {
                    "type": "string",
                    "description": "Optional resource UUID for quota enforcement.",
                },
                "priority": {
                    "type": "integer",
                    "description": "Priority int16. Default 0.",
                },
            },
            "required": ["job_type", "project_id", "payload"],
            "additionalProperties": False,
        },
        func=_platform_jobs_submit,
        requires_approval=True,
    ))
