"""recall / fetch_artifact / list_artifacts — the memory tool surface.

These tools let the LLM query, inspect, and list prior tool outputs
that were auto-recorded by the recorder. They themselves opt out of
recording (`record_artifacts=False`) so calling them doesn't create
yet another recall'able artifact (which would cause infinite recursion
and pointless surface inflation).

See docs/stateful_tools_2026.md for the full architecture.
"""
from __future__ import annotations

import logging
from typing import Any, Optional

from app.tools.base import Tool, ToolRegistry
from app.tools.memory.embedder import Embedder
from app.tools.memory.recorder import (
    get_embedder,
    get_store,
    is_recording_enabled,
    resolve_session_id,
)

logger = logging.getLogger(__name__)


def _empty_note(store, *, session_id: Optional[str], widen: str) -> Optional[str]:
    """Explain a zero-result read, or return None if there is nothing to add.

    A zero-length result list is ambiguous. Two causes look identical to the
    caller and both are live here: the recorder is switched off, so no tool
    output has been written since; and rows exist under other session ids,
    which the session filter excludes. Reporting neither is what made an
    empty Artifacts tab indistinguishable from an empty store.

    `widen` is the argument that drops the session filter for the calling
    tool (they differ: recall takes scope='all', list takes session='*').
    """
    parts: list[str] = []
    if not is_recording_enabled():
        parts.append(
            "Artifact recording is off; new tool outputs are not stored. "
            "Set PRISM_ARTIFACT_RECORDING=1 to store them."
        )
    if session_id is not None:
        try:
            elsewhere = store.count_artifacts() - store.count_artifacts(
                session_id=session_id
            )
        except Exception as e:  # a diagnostic must never break the read
            logger.debug("artifact count for empty-result note failed: %s", e)
            elsewhere = 0
        if elsewhere > 0:
            noun = "artifact is" if elsewhere == 1 else "artifacts are"
            parts.append(
                f"{elsewhere} {noun} stored under other session ids. "
                f"Use {widen} to include them."
            )
    return " ".join(parts) or None


def _recall(**kwargs) -> dict:
    """Hybrid (BM25 + vector + RRF) semantic recall over local artifacts."""
    store = get_store()
    if store is None:
        return {"error": "Artifact store not configured. Memory tools are disabled."}

    query = kwargs.get("query")
    if not query:
        return {"error": "`query` is required"}

    scope = kwargs.get("scope", "session")
    tool_filter = kwargs.get("tool")
    limit = int(kwargs.get("limit", 10))

    if scope not in ("session", "all"):
        return {"error": f"Unknown scope '{scope}'. Valid: 'session', 'all'."}

    session_id: Optional[str] = (
        resolve_session_id() if scope == "session" else None
    )

    embedder: Embedder = get_embedder()
    try:
        query_emb = embedder.embed(query)
    except Exception as e:
        logger.warning("recall embedding failed (%s); falling back to BM25-only", e)
        query_emb = None

    hits = store.recall(
        query_text=query,
        query_embedding=query_emb,
        session_id=session_id,
        tool_name=tool_filter,
        limit=limit,
    )
    out = {
        "query": query,
        "scope": scope,
        "hits": hits,
        "count": len(hits),
    }
    if not hits:
        note = _empty_note(store, session_id=session_id, widen="scope='all'")
        if note:
            out["note"] = note
    return out


def _fetch_artifact(**kwargs) -> dict:
    """Get the full verbatim data for one artifact (or one record of it)."""
    store = get_store()
    if store is None:
        return {"error": "Artifact store not configured."}

    artifact_id = kwargs.get("artifact_id")
    if not artifact_id:
        return {"error": "`artifact_id` is required"}

    record_idx = kwargs.get("record_idx")

    art = store.get(artifact_id)
    if art is None:
        return {"error": f"Artifact '{artifact_id}' not found"}

    if record_idx is not None:
        rec = store.get_record(artifact_id, int(record_idx))
        if rec is None:
            return {
                "error": (
                    f"Record {record_idx} not found in {artifact_id} "
                    f"(record_count={art.record_count})"
                )
            }
        return {
            "artifact_id": artifact_id,
            "record_idx": int(record_idx),
            "record": rec,
            "tool": art.tool_name,
            "session_id": art.session_id,
            "created_at": art.created_at,
        }

    return {
        "artifact_id": artifact_id,
        "tool": art.tool_name,
        "session_id": art.session_id,
        "args": art.args,
        "result": art.result,
        "summary": art.summary,
        "record_count": art.record_count,
        "bytes_size": art.bytes_size,
        "created_at": art.created_at,
        "promoted_to_kg": art.promoted_to_kg,
    }


def _list_artifacts(**kwargs) -> dict:
    """Non-semantic listing — by tool, by time, by session."""
    store = get_store()
    if store is None:
        return {"error": "Artifact store not configured."}

    session_filter = kwargs.get("session")
    if session_filter is None and kwargs.get("scope", "session") == "session":
        session_filter = resolve_session_id()

    scoped = session_filter if session_filter != "*" else None
    rows = store.list_artifacts(
        session_id=scoped,
        tool_name=kwargs.get("tool"),
        since=kwargs.get("since"),
        limit=int(kwargs.get("limit", 20)),
    )
    out = {
        "artifacts": rows,
        "count": len(rows),
        "session_filter": session_filter,
    }
    # `note` is additive: the Rust workspace reader (crates/agent/src/protocol.rs,
    # parse_artifact_list_response) reads session_filter/artifacts/count and
    # treats a top-level `error` key as a failure, so the key must not be named
    # `error` and must not replace any of those three.
    if not rows:
        note = _empty_note(store, session_id=scoped, widen="session='*'")
        if note:
            out["note"] = note
    return out


# ---------------------------------------------------------------------------
# Tool descriptions — written for the LLM to understand the WORKFLOW.
# ---------------------------------------------------------------------------

_RECALL_DESCRIPTION = (
    "Hybrid recall (BM25 keyword + semantic vector, RRF-fused) over the "
    # The previous wording promised "every meaningful tool output from this and
    # prior sessions has been auto-indexed". Measured false: the recorder is off
    # unless PRISM_ARTIFACT_RECORDING is set, so nothing has been indexed since
    # 2026-07-03. Say what the tool searches, not what the store is assumed to hold.
    "agent's local artifact store. Covers tool outputs that were recorded "
    "as artifacts. Use this when the user "
    "refers to earlier results: 'which of those Ti alloys had the highest "
    "density', 'show me the DFT result from yesterday', 'what did we find "
    "about Inconel 718'. The hit list contains stable artifact_ids — pass "
    "one to `fetch_artifact` for the full verbatim data. By default "
    "`scope='session'` (this conversation only) is much faster and usually "
    "what you want; use `scope='all'` to reach across all sessions on this "
    "machine. Optional `tool` filter narrows to a specific source "
    "(e.g. tool='materials_search'). NOT for searching the open web (use "
    "web_search) and NOT for searching the platform knowledge graph "
    "(use knowledge with action='search' or action='semantic')."
)

_RECALL_SCHEMA = {
    "type": "object",
    "properties": {
        "query": {
            "type": "string",
            "description": (
                "Natural-language query. Concrete phrasing wins "
                "('Ti alloys with band gap above 2 eV') over abstract."
            ),
        },
        "scope": {
            "type": "string",
            "enum": ["session", "all"],
            "default": "session",
            "description": (
                "'session' searches only this conversation's artifacts "
                "(fast, default). 'all' includes every prior session."
            ),
        },
        "tool": {
            "type": "string",
            "description": (
                "Optional: only return artifacts produced by this exact "
                "tool name (e.g. 'materials_search', 'predict', 'research')."
            ),
        },
        "limit": {
            "type": "integer",
            "default": 10,
            "minimum": 1,
            "maximum": 50,
            "description": "Max hits to return.",
        },
    },
    "required": ["query"],
    "additionalProperties": False,
}


_FETCH_DESCRIPTION = (
    "Retrieve the FULL verbatim data of one artifact by its artifact_id. "
    "Use this after `recall` returns hits and you need the complete content "
    "rather than just the summary. For list-shaped artifacts (e.g. a "
    "materials_search result with 50 entries), pass `record_idx=N` to get "
    "just one record. No semantic processing; deterministic lookup. "
    "artifact_id format is 'art_<short>'."
)

_FETCH_SCHEMA = {
    "type": "object",
    "properties": {
        "artifact_id": {
            "type": "string",
            "description": (
                "Stable artifact ID (returned by recall or auto-injected "
                "as _artifact_id on tool outputs)."
            ),
        },
        "record_idx": {
            "type": "integer",
            "description": (
                "For list-shaped artifacts: zero-based index of the single "
                "record to fetch. Omit to get the full artifact."
            ),
        },
    },
    "required": ["artifact_id"],
    "additionalProperties": False,
}


_LIST_DESCRIPTION = (
    "List artifacts by metadata (tool, session, time) — non-semantic, "
    "ordered by creation time descending. Use when the user asks 'what "
    "have we done so far', 'show recent results', or to enumerate all "
    "outputs from a specific tool. For semantic search use `search_artifacts` "
    "instead. Default: current session, 20 most recent."
)

_LIST_SCHEMA = {
    "type": "object",
    "properties": {
        "session": {
            "type": "string",
            "description": (
                "Filter by session ID. Default: current session. Pass "
                "'*' to list across all sessions."
            ),
        },
        "tool": {
            "type": "string",
            "description": (
                "Filter by tool name (e.g. 'research', 'predict', "
                "'materials_search')."
            ),
        },
        "since": {
            "type": "string",
            "description": (
                "ISO8601 timestamp; only return artifacts created at or "
                "after this time."
            ),
        },
        "limit": {
            "type": "integer",
            "default": 20,
            "minimum": 1,
            "maximum": 200,
            "description": "Max rows to return.",
        },
    },
    "additionalProperties": False,
}


def create_memory_tools(registry: ToolRegistry) -> None:
    """Register recall / fetch_artifact / list_artifacts.

    All three opt out of recording (`record_artifacts=False`) — they're
    the recall surface, not the create surface. Recording their outputs
    would inflate the artifact store with self-references and risk
    infinite recursion.
    """
    registry.register(Tool(
        name="search_artifacts",
        description=_RECALL_DESCRIPTION,
        input_schema=_RECALL_SCHEMA,
        func=_recall,
        record_artifacts=False,
    ))
    registry.register(Tool(
        name="fetch_artifact",
        description=_FETCH_DESCRIPTION,
        input_schema=_FETCH_SCHEMA,
        func=_fetch_artifact,
        record_artifacts=False,
    ))
    registry.register(Tool(
        name="list_artifacts",
        description=_LIST_DESCRIPTION,
        input_schema=_LIST_SCHEMA,
        func=_list_artifacts,
        record_artifacts=False,
    ))
