"""A zero-result artifact read must say why it was zero.

Two live causes produce an empty result that looks exactly like an empty
store, and both were observed on ~/.prism/artifacts.db (5 artifacts, all
under session_id 'default', none written since 2026-07-03):

  1. `build_full_registry()` configures the recorder with record_enabled=False
     unless PRISM_ARTIFACT_RECORDING is set, so nothing new is written.
  2. list/search filter on session_id, so rows written under an earlier
     session id are excluded from every default read.

Reads stay session-scoped on purpose: crates/agent/src/protocol.rs
(parse_artifact_list_response) rejects the whole response if any returned
artifact carries a different session_id, so widening the default would turn
an empty Artifacts tab into an error banner. These tests pin the diagnosis
instead: the reads must name the cause and the argument that widens them.
"""
from pathlib import Path

import pytest

from app.tools.base import Tool, ToolRegistry
from app.tools.memory import (
    ArtifactStore,
    HashEmbedder,
    configure as configure_memory,
    create_memory_tools,
    reset as reset_memory,
)

# Recording-eligible payload: >512 bytes of canonical JSON under a content key.
_PAYLOAD = {"results": [{"formula": f"Ti{i}Al", "padding": "z" * 40} for i in range(20)]}


@pytest.fixture
def env(tmp_path: Path):
    """Registry + store wired to a temp DB, recording ON so we can seed."""
    store = ArtifactStore(tmp_path / "artifacts.db")
    configure_memory(
        store=store,
        embedder=HashEmbedder(dim=64),
        session_id="session-a",
        embed_async=False,
        record_enabled=True,
    )
    registry = ToolRegistry()
    create_memory_tools(registry)
    registry.register(Tool(
        name="seed",
        description="dummy tool: seed",
        input_schema={"type": "object", "properties": {}},
        func=lambda **_k: _PAYLOAD,
    ))
    try:
        yield registry, store
    finally:
        reset_memory()


def test_count_artifacts_totals_and_scopes(env):
    """The store can count in-session and overall without pulling rows."""
    registry, store = env
    registry.get("seed").execute()
    configure_memory(session_id="session-b")
    registry.get("seed").execute()

    assert store.count_artifacts() == 2
    assert store.count_artifacts(session_id="session-a") == 1
    assert store.count_artifacts(session_id="session-b") == 1
    assert store.count_artifacts(session_id="nobody") == 0


def test_search_names_the_recorder_when_it_is_off(env):
    """Recording off + nothing stored: the note must name the switch."""
    registry, store = env
    configure_memory(record_enabled=False)

    out = registry.get("search_artifacts").execute(query="titanium")

    assert out["count"] == 0
    assert "PRISM_ARTIFACT_RECORDING" in out.get("note", ""), out


def test_search_reports_artifacts_held_in_other_sessions(env):
    """Rows exist, the session filter hid them: say how many and how to widen."""
    registry, store = env
    registry.get("seed").execute()          # written under session-a
    configure_memory(session_id="session-b")

    out = registry.get("search_artifacts").execute(query="titanium")

    assert out["count"] == 0
    note = out.get("note", "")
    assert "1 artifact is stored under other session ids" in note, note
    assert "scope='all'" in note, note


def test_list_reports_artifacts_held_in_other_sessions(env):
    """Same diagnosis on list_artifacts, naming ITS widening argument."""
    registry, store = env
    registry.get("seed").execute()
    configure_memory(session_id="session-b")

    out = registry.get("list_artifacts").execute()

    assert out["count"] == 0
    note = out.get("note", "")
    assert "1 artifact is stored under other session ids" in note, note
    assert "session='*'" in note, note


def test_widened_read_returns_the_hidden_artifacts(env):
    """The widening arguments the note advertises must actually work."""
    registry, store = env
    registry.get("seed").execute()
    configure_memory(session_id="session-b")

    assert registry.get("search_artifacts").execute(
        query="titanium", scope="all")["count"] > 0
    assert registry.get("list_artifacts").execute(session="*")["count"] == 1


def test_no_note_when_the_read_found_something(env):
    """The note is a diagnosis for zero results, not a permanent banner."""
    registry, store = env
    registry.get("seed").execute()

    hit = registry.get("search_artifacts").execute(query="titanium")
    listed = registry.get("list_artifacts").execute()

    assert hit["count"] > 0 and "note" not in hit, hit
    assert listed["count"] == 1 and "note" not in listed, listed


def test_list_note_does_not_break_the_rust_workspace_contract(env):
    """crates/agent/src/protocol.rs reads session_filter/artifacts/count and
    treats a top-level `error` key as a failed list. The note must stay
    additive: if it were ever renamed to `error`, the Artifacts tab would go
    from empty to an error banner."""
    registry, store = env
    registry.get("seed").execute()
    configure_memory(session_id="session-b")

    out = registry.get("list_artifacts").execute(session="session-b")

    assert "error" not in out, out
    assert out["session_filter"] == "session-b"
    assert out["artifacts"] == []
    assert out["count"] == len(out["artifacts"])
    assert out.get("note")
