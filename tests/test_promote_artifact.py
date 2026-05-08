"""Tests for knowledge(action='promote_artifact').

Closes the loop between local stateful memory (the artifact store) and
the shared MARC27 knowledge graph. A recall'able local artifact can be
pushed into the cross-session graph via this action.

These tests use a stub MARC27 client to verify the dispatch + payload
shape without making real HTTP calls. The actual server-side ingest
(entity extraction → graph write → vector embed) is tested separately
on the marc27-core side.
"""
import os
import tempfile
from pathlib import Path
from unittest.mock import patch

import pytest

from app.tools.base import ToolRegistry
from app.tools.knowledge import (
    _build_promotion_text,
    create_knowledge_tools,
)
from app.tools.memory import (
    ArtifactStore,
    HashEmbedder,
    configure as configure_memory,
    reset as reset_memory,
)


@pytest.fixture
def memory_env(tmp_path: Path):
    """Configure stateful memory in a temp DB so promote_artifact can find
    artifacts. Yields (store, artifact_id) — a real artifact pre-recorded
    so tests can promote it."""
    db_path = tmp_path / "artifacts.db"
    store = ArtifactStore(db_path)
    configure_memory(
        store=store,
        embedder=HashEmbedder(dim=64),
        session_id="promote-test",
        embed_async=False,
    )

    # Pre-record an artifact representative of a search result
    artifact_id = store.record(
        tool_name="materials_search",
        args={"elements": ["Ti", "Al"], "limit": 3},
        result={
            "results": [
                {"formula": "Ti6Al4V", "density": 4.43, "tensile_mpa": 950},
                {"formula": "TiAl3", "density": 3.36, "tensile_mpa": 700},
                {"formula": "Ti3Al", "density": 4.20, "tensile_mpa": 850},
            ],
            "count": 3,
            "source": "test",
        },
        session_id="promote-test",
    )
    try:
        yield store, artifact_id
    finally:
        reset_memory()


class _StubBaseAPI:
    """Minimal stand-in for marc27.api.base.BaseAPI."""

    def __init__(self):
        self.posts: list[tuple[str, dict]] = []

    def post(self, path: str, json: dict):
        self.posts.append((path, json))

        class _Resp:
            def __init__(self, body):
                self._body = body

            def json(self):
                return self._body

        return _Resp({
            "job_id": "test-ingest-job-uuid",
            "status": "pending",
        })


class _StubClient:
    """SDK-shaped client with `_base` attribute (so _act_promote_artifact
    takes the SDK path, which uses base.post)."""

    def __init__(self):
        self._base = _StubBaseAPI()


# ---------------------------------------------------------------------------
# Promotion text builder
# ---------------------------------------------------------------------------

class TestBlobBuilder:
    def test_blob_includes_provenance(self, memory_env):
        store, artifact_id = memory_env
        art = store.get(artifact_id)
        blob = _build_promotion_text(art)
        # Provenance metadata must be present so the extractor LLM
        # knows the origin
        assert "ARTIFACT PROMOTED FROM PRISM" in blob
        assert "materials_search" in blob
        assert "promote-test" in blob  # session_id

    def test_blob_includes_summary(self, memory_env):
        store, artifact_id = memory_env
        art = store.get(artifact_id)
        blob = _build_promotion_text(art)
        # Auto-generated summary must be present
        assert art.summary in blob

    def test_blob_includes_records_when_list_shaped(self, memory_env):
        store, artifact_id = memory_env
        art = store.get(artifact_id)
        blob = _build_promotion_text(art)
        # Each formula should appear in the blob
        assert "Ti6Al4V" in blob
        assert "TiAl3" in blob
        assert "Ti3Al" in blob

    def test_blob_includes_args(self, memory_env):
        store, artifact_id = memory_env
        art = store.get(artifact_id)
        blob = _build_promotion_text(art)
        # Args (the tool inputs) should be included for full provenance
        assert "Ti" in blob and "Al" in blob


# ---------------------------------------------------------------------------
# Action dispatch
# ---------------------------------------------------------------------------

class TestPromoteArtifactAction:
    def test_action_in_enum(self, memory_env):
        reg = ToolRegistry()
        create_knowledge_tools(reg)
        tool = reg.get("knowledge")
        actions = tool.input_schema["properties"]["action"]["enum"]
        assert "promote_artifact" in actions

    def test_artifact_id_in_schema(self, memory_env):
        reg = ToolRegistry()
        create_knowledge_tools(reg)
        tool = reg.get("knowledge")
        assert "artifact_id" in tool.input_schema["properties"]

    def test_missing_artifact_id(self, memory_env):
        reg = ToolRegistry()
        create_knowledge_tools(reg)
        tool = reg.get("knowledge")
        # Patch _get_client so the dispatch reaches the handler
        with patch("app.tools.knowledge._get_client", return_value=_StubClient()):
            r = tool.execute(action="promote_artifact")
        assert "error" in r
        assert "artifact_id" in r["error"]

    def test_unknown_artifact(self, memory_env):
        reg = ToolRegistry()
        create_knowledge_tools(reg)
        tool = reg.get("knowledge")
        with patch("app.tools.knowledge._get_client", return_value=_StubClient()):
            r = tool.execute(action="promote_artifact", artifact_id="art_xyz_nope")
        assert "error" in r
        assert "not found in local store" in r["error"]

    def test_happy_path_submits_ingest(self, memory_env):
        store, artifact_id = memory_env
        reg = ToolRegistry()
        create_knowledge_tools(reg)
        tool = reg.get("knowledge")

        stub = _StubClient()
        with patch("app.tools.knowledge._get_client", return_value=stub):
            r = tool.execute(action="promote_artifact", artifact_id=artifact_id)

        assert "error" not in r, f"unexpected error: {r}"
        assert r["status"] == "promoted"
        assert r["artifact_id"] == artifact_id
        assert r["tool"] == "materials_search"
        assert r["ingest_job"]["job_id"] == "test-ingest-job-uuid"
        assert r["blob_size_bytes"] > 0

        # Verify the right endpoint was called
        assert len(stub._base.posts) == 1
        path, body = stub._base.posts[0]
        assert path == "/knowledge/ingest-job"
        # The promotion blob is sent as a query-mode source
        assert body["mode"] == "full"
        assert body["source"]["type"] == "query"
        assert "Ti6Al4V" in body["source"]["query"]

    def test_promoted_flag_persists(self, memory_env):
        """After successful promotion, the local artifact must be flagged
        promoted_to_kg=True so subsequent calls don't double-promote."""
        store, artifact_id = memory_env
        reg = ToolRegistry()
        create_knowledge_tools(reg)
        tool = reg.get("knowledge")

        with patch("app.tools.knowledge._get_client", return_value=_StubClient()):
            r1 = tool.execute(action="promote_artifact", artifact_id=artifact_id)
        assert r1["status"] == "promoted"

        # Verify the flag stuck
        art = store.get(artifact_id)
        assert art.promoted_to_kg is True

    def test_double_promotion_short_circuits(self, memory_env):
        """Already-promoted artifacts return 'already_promoted' without
        re-submitting an ingest job."""
        store, artifact_id = memory_env
        reg = ToolRegistry()
        create_knowledge_tools(reg)
        tool = reg.get("knowledge")

        stub = _StubClient()
        with patch("app.tools.knowledge._get_client", return_value=stub):
            tool.execute(action="promote_artifact", artifact_id=artifact_id)
            r = tool.execute(action="promote_artifact", artifact_id=artifact_id)

        assert r["status"] == "already_promoted"
        # Only ONE ingest call should have happened (the second was short-circuited)
        assert len(stub._base.posts) == 1


# ---------------------------------------------------------------------------
# Memory subsystem unavailability
# ---------------------------------------------------------------------------

def test_memory_disabled_returns_clear_error():
    """When the memory subsystem isn't configured, promote_artifact must
    return a clean error rather than crashing."""
    reset_memory()  # ensure no store configured

    reg = ToolRegistry()
    create_knowledge_tools(reg)
    tool = reg.get("knowledge")

    with patch("app.tools.knowledge._get_client", return_value=_StubClient()):
        r = tool.execute(action="promote_artifact", artifact_id="art_anything")

    assert "error" in r
    assert "Memory subsystem not configured" in r["error"]
