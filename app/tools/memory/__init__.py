"""Stateful tool memory.

Local-first artifact store + recall API. Records every meaningful tool
output to a single SQLite database, embeds it (sentence-transformers
when available, deterministic hash fallback otherwise), and exposes
hybrid recall (BM25 + vector + RRF fusion) via the `recall` /
`fetch_artifact` / `list_artifacts` tools.

The integration point is `Tool.execute` in `app/tools/base.py`, which
calls `recorder.record_if_enabled` after the underlying function returns.
There is no monkey-patching — `Tool.execute` always asks the recorder
whether to persist, the recorder's behavior is entirely controlled by
the runtime configuration set via `recorder.configure()`.

See docs/stateful_tools_2026.md for the full architecture.
"""
from app.tools.memory.embedder import (
    Embedder,
    HashEmbedder,
    STEmbedder,
    get_default_embedder,
    reset_default_embedder,
)
from app.tools.memory.recorder import (
    augment_with_artifact_id,
    configure,
    get_embedder,
    get_store,
    is_configured,
    record_if_enabled,
    reset,
    resolve_session_id,
    should_record,
)
from app.tools.memory.store import (
    ArtifactRow,
    ArtifactStore,
    default_db_path,
)
from app.tools.memory.tool import create_memory_tools

__all__ = [
    # Store
    "ArtifactRow",
    "ArtifactStore",
    "default_db_path",
    # Embedder
    "Embedder",
    "HashEmbedder",
    "STEmbedder",
    "get_default_embedder",
    "reset_default_embedder",
    # Recorder
    "configure",
    "is_configured",
    "get_store",
    "get_embedder",
    "resolve_session_id",
    "record_if_enabled",
    "reset",
    "should_record",
    "augment_with_artifact_id",
    # Tools
    "create_memory_tools",
]
