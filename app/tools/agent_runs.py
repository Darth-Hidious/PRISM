# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""Background research runs — a SECOND agent, running server-side.

This is the client for the platform's `/agent-runs` orchestrator (a
long-running research agent on a frontier model — Nemotron via NVIDIA
Build — with knowledge-graph + web access). It exists so the LOCAL chat
agent never blocks on deep research:

    local agent (this chat)          platform agent (run)
    ----------------------          --------------------
    start_background_research  -->  run created, starts working
    ...user keeps chatting...       ...researches for minutes...
    check_background_research  -->  status; when done: the ANSWER

Context discipline (deliberate): check_* returns the run's answer and
citations ONLY — never the raw event stream. The local model's context
stays small; the heavy lifting stays server-side. That is the whole
point of running two agents with different contexts.
"""
from __future__ import annotations

import logging

from app.tools._platform_client import platform
from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


def _start(**kwargs) -> dict:
    question = (kwargs.get("question") or "").strip()
    if not question:
        return {"error": "`question` is required"}
    depth = int(kwargs.get("depth", 1))
    body = {"question": question, "depth": depth}
    result = platform().post("/agent-runs", json=body)
    if "error" in result:
        return result
    return {
        "run_id": result.get("id"),
        # Live API field is `state` (verified 2026-07-02); keep `status`
        # as a fallback in case the wire shape evolves.
        "status": result.get("state") or result.get("status") or "queued",
        "hint": (
            "The platform agent is researching in the background. Keep "
            "working; call check_background_research(run_id=...) later — "
            "typical runs take one to several minutes."
        ),
    }


def _check(**kwargs) -> dict:
    run_id = (kwargs.get("run_id") or "").strip()
    if not run_id:
        return {"error": "`run_id` is required"}
    result = platform().get(f"/agent-runs/{run_id}")
    if "error" in result:
        return result
    status = result.get("state") or result.get("status") or "unknown"
    out = {"run_id": run_id, "status": status}
    # Context discipline: forward the answer only when the run is done —
    # never the event stream.
    if status in ("succeeded", "completed", "done"):
        out["answer"] = result.get("answer") or "(run finished with no answer text)"
    elif status in ("failed", "cancelled"):
        out["error"] = result.get("error") or f"run {status}"
    else:
        out["hint"] = "still running — check again later, do not busy-poll"
    return out


def _list(**kwargs) -> dict:
    result = platform().get("/agent-runs")
    if "error" in result:
        return result
    runs = result.get("runs", result if isinstance(result, list) else [])
    return {
        "runs": [
            {
                "run_id": r.get("id"),
                "status": r.get("state") or r.get("status"),
                "question": (r.get("question") or "")[:120],
            }
            for r in runs[:20]
        ]
    }


def _cancel(**kwargs) -> dict:
    run_id = (kwargs.get("run_id") or "").strip()
    if not run_id:
        return {"error": "`run_id` is required"}
    return platform().post(f"/agent-runs/{run_id}/cancel", json={})


def create_agent_run_tools(registry: ToolRegistry) -> None:
    """Register the background-research (platform agent-run) tools."""
    registry.register(Tool(
        name="start_background_research",
        description=(
            "Launch a SEPARATE long-running research agent on the MARC27 "
            "platform (frontier model with knowledge-graph + web access). "
            "Use this instead of the blocking `research` tool whenever the "
            "question needs deep multi-step work — you get a run_id back "
            "immediately and can keep helping the user while it works. "
            "Costs server LLM budget."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The research question or instruction.",
                },
                "depth": {
                    "type": "integer",
                    "description": "0 = knowledge graph only; 1+ allows web (default 1).",
                },
            },
            "required": ["question"],
            "additionalProperties": False,
        },
        func=_start,
        requires_approval=True,
    ))
    registry.register(Tool(
        name="check_background_research",
        description=(
            "Check a background research run. Returns status while running; "
            "returns the ANSWER once finished. Free / read-only."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "run_id": {"type": "string", "description": "Run id from start_background_research."},
            },
            "required": ["run_id"],
            "additionalProperties": False,
        },
        func=_check,
        requires_approval=False,
    ))
    registry.register(Tool(
        name="list_background_research",
        description=(
            "List recent background research runs with status. Use this to "
            "discover run ids for check_background_research or "
            "cancel_background_research. Takes no arguments. Free / read-only."
        ),
        input_schema={"type": "object", "properties": {}, "additionalProperties": False},
        func=_list,
        requires_approval=False,
    ))
    registry.register(Tool(
        name="cancel_background_research",
        description=(
            "Cancel a running background research run by id. Use this when a "
            "detached research agent (started by start_background_research) "
            "is spending budget you want to stop. Returns the cancelled run's "
            "final status."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "run_id": {"type": "string", "description": "Run id to cancel."},
            },
            "required": ["run_id"],
            "additionalProperties": False,
        },
        func=_cancel,
        requires_approval=True,
    ))
