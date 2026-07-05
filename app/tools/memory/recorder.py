"""Recorder — called from `Tool.execute` to persist meaningful tool outputs.

This is the integration point between the agent's tool execution and the
artifact memory subsystem. It replaces the earlier middleware.py which
relied on monkey-patching `Tool.execute` at bootstrap; that approach
silently bypassed the MCP server path because `mcp_server.py` captures
bound methods at registration time. By making the recorder a regular
call from `Tool.execute`, every caller — `tool_server.py`, `mcp_server.py`,
and any future invocation site — reaches the same recording logic.

Configuration lives in module-level state. `configure()` is called once
during bootstrap with a configured `ArtifactStore` + `Embedder` + session
id. Until configured (i.e. in tests, or in a CLI mode that doesn't want
memory), `record_if_enabled` is a no-op pass-through.
"""
from __future__ import annotations

import logging
import os
import threading
from typing import Any, Optional

from app.tools.memory.embedder import Embedder, get_default_embedder
from app.tools.memory.store import (
    ArtifactStore,
    _canonical_json,
    _extract_records,
    _summarize,
    _summarize_record,
)

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Heuristics — what to record + how
# ---------------------------------------------------------------------------

# Tools that explicitly opt out of recording — set on the Tool itself
# via `record_artifacts=False`. This module also has a name-based
# safety net for tools that for whatever reason didn't set the flag.
_NEVER_RECORD: frozenset[str] = frozenset({
    "search_artifacts",
    "fetch_artifact",
    "list_artifacts",
    "show_scratchpad",
})

# Minimum result size (in canonical JSON bytes) to bother recording.
_MIN_BYTES = 512

# Fields that indicate a result has content worth keeping.
_CONTENT_KEYS: frozenset[str] = frozenset({
    "results", "data", "papers", "patents", "materials", "entities",
    "services", "subscriptions", "corpora", "providers", "gpus",
    "models", "pretrained_models", "trained_models", "neighbors",
    "paths", "content", "answer", "text", "summary", "hits",
})


def should_record(result: Any, *, tool_name: str) -> bool:
    """Should this tool output be persisted as an artifact?"""
    if tool_name in _NEVER_RECORD:
        return False
    if not isinstance(result, (dict, list)):
        return False

    canon = _canonical_json(result)
    if len(canon.encode("utf-8")) < _MIN_BYTES:
        return False

    if isinstance(result, dict):
        keys = set(result.keys())
        if keys == {"error"} or keys == {"error", "hint"}:
            return False
        if not (keys & _CONTENT_KEYS):
            return False

    return True


def augment_with_artifact_id(
    result: Any,
    artifact_id: str,
    record_count: Optional[int] = None,
) -> Any:
    """Inject `_artifact_id` (+ `_record_count` for list-shaped) into the result."""
    if isinstance(result, dict):
        out = dict(result)
        out["_artifact_id"] = artifact_id
        if record_count is not None:
            out["_record_count"] = record_count
        return out
    if isinstance(result, list):
        return {
            "_artifact_id": artifact_id,
            "_record_count": len(result),
            "results": result,
        }
    return result


# ---------------------------------------------------------------------------
# Module-level configuration set at bootstrap
# ---------------------------------------------------------------------------

_LOCK = threading.Lock()
_CONFIG: dict[str, Any] = {
    "store": None,
    "embedder": None,
    "session_id": None,
    "embed_async": True,
}


_UNSET = object()


def configure(
    *,
    store: Optional[ArtifactStore] = None,
    embedder: Optional[Embedder] = None,
    session_id: Optional[str] = None,
    embed_async: Any = _UNSET,
) -> None:
    """Set the recorder's runtime configuration.

    Each parameter is independently set-or-leave. Passing `None` means
    "leave unchanged" for store/embedder/session_id; for `embed_async`,
    the sentinel `_UNSET` is used instead of None because False is a
    valid value. Called once by bootstrap, may be called again to
    update specific fields (e.g. switch session_id mid-run).
    """
    with _LOCK:
        if store is not None:
            _CONFIG["store"] = store
        if embedder is not None:
            _CONFIG["embedder"] = embedder
        if session_id is not None:
            _CONFIG["session_id"] = session_id
        if embed_async is not _UNSET:
            _CONFIG["embed_async"] = bool(embed_async)


def is_configured() -> bool:
    return _CONFIG["store"] is not None


def get_store() -> Optional[ArtifactStore]:
    return _CONFIG["store"]


def get_embedder() -> Embedder:
    """Return the configured embedder, falling back to default."""
    e = _CONFIG["embedder"]
    if e is not None:
        return e
    return get_default_embedder()


def resolve_session_id() -> str:
    """Use configured session_id, else PRISM_SESSION_ID env, else 'default'."""
    if _CONFIG["session_id"]:
        return str(_CONFIG["session_id"])
    return os.environ.get("PRISM_SESSION_ID", "default")


def reset() -> None:
    """For tests — clear all configuration so the next configure() starts clean."""
    with _LOCK:
        _CONFIG["store"] = None
        _CONFIG["embedder"] = None
        _CONFIG["session_id"] = None
        _CONFIG["embed_async"] = True


# ---------------------------------------------------------------------------
# Background embedding
# ---------------------------------------------------------------------------

def _embed_in_background(
    *,
    artifact_id: str,
    summary: str,
    record_summaries: list[str],
    store: ArtifactStore,
    embedder: Embedder,
) -> None:
    """Compute embeddings + backfill them into the store. Run on a daemon thread."""
    try:
        artifact_emb = embedder.embed(summary)
        rec_embs: list[Optional[list[float]]] = []
        if record_summaries:
            try:
                batched = embedder.embed_batch(record_summaries)
                rec_embs = list(batched)
                while len(rec_embs) < len(record_summaries):
                    rec_embs.append(None)
            except Exception as e:
                logger.debug("batch embed failed (%s); per-item fallback", e)
                for s in record_summaries:
                    try:
                        rec_embs.append(embedder.embed(s))
                    except Exception:
                        rec_embs.append(None)
        store.update_embedding(
            artifact_id=artifact_id,
            embedding=artifact_emb,
            record_embeddings=rec_embs if record_summaries else None,
        )
    except Exception as e:
        logger.warning("background embed failed for %s: %s", artifact_id, e)


# ---------------------------------------------------------------------------
# Entry point — called from Tool.execute
# ---------------------------------------------------------------------------

def record_if_enabled(
    *,
    tool_name: str,
    args: dict,
    result: Any,
) -> Any:
    """Record the result if recording is enabled and it's worth keeping.

    Returns the augmented result (with `_artifact_id`) on success, or the
    original result on any opt-out / disabled / error condition. This is
    the contract `Tool.execute` relies on — non-fatal storage failures
    must NEVER raise.
    """
    store = _CONFIG["store"]
    if store is None:
        return result
    if not should_record(result, tool_name=tool_name):
        return result

    embedder = get_embedder()
    session_id = resolve_session_id()

    try:
        artifact_id = store.record(
            tool_name=tool_name,
            args=args,
            result=result,
            session_id=session_id,
        )
    except Exception as e:
        logger.warning("artifact insert failed for %s: %s", tool_name, e)
        return result

    summary = _summarize(result, tool_name)
    records = _extract_records(result)
    rec_summaries = [_summarize_record(r, i) for i, r in enumerate(records or [])]
    record_count = len(records) if records else None

    if _CONFIG["embed_async"]:
        threading.Thread(
            target=_embed_in_background,
            kwargs=dict(
                artifact_id=artifact_id,
                summary=summary,
                record_summaries=rec_summaries,
                store=store,
                embedder=embedder,
            ),
            daemon=True,
            name=f"embed-{artifact_id}",
        ).start()
    else:
        _embed_in_background(
            artifact_id=artifact_id,
            summary=summary,
            record_summaries=rec_summaries,
            store=store,
            embedder=embedder,
        )

    return augment_with_artifact_id(result, artifact_id, record_count)
