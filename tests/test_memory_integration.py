"""Integration tests for the stateful memory subsystem.

Exercises the FULL flow: Tool.execute → recorder.record_if_enabled →
ArtifactStore.record → embedder backfill → recall returns the right
artifact → fetch_artifact returns verbatim data.

Tests are hermetic — each uses a temp DB + the deterministic HashEmbedder
so embeddings are reproducible and no model is loaded.
"""
import os
from pathlib import Path

import pytest

from app.tools.base import Tool, ToolRegistry
from app.tools.memory import (
    ArtifactStore,
    HashEmbedder,
    configure as configure_memory,
    create_memory_tools,
    is_configured,
    reset as reset_memory,
)


@pytest.fixture
def memory_env(tmp_path: Path):
    """Configure the memory subsystem for a hermetic test.

    Yields a (registry, store) tuple. After the test, recorder state is
    reset so subsequent tests don't see leaked configuration.
    """
    db_path = tmp_path / "artifacts.db"
    store = ArtifactStore(db_path)
    embedder = HashEmbedder(dim=64)
    configure_memory(
        store=store,
        embedder=embedder,
        session_id="test-session",
        embed_async=False,  # synchronous for deterministic test outcome
    )
    registry = ToolRegistry()
    create_memory_tools(registry)
    try:
        yield registry, store
    finally:
        reset_memory()


def _register_dummy_tool(registry: ToolRegistry, name: str, returns):
    """Register a tool that returns a fixed value, recording-eligible."""
    def _impl(**_kwargs):
        return returns
    registry.register(Tool(
        name=name,
        description=f"dummy tool: {name}",
        input_schema={"type": "object", "properties": {}},
        func=_impl,
    ))


# ---------------------------------------------------------------------------
# Scenario 1: end-to-end record → recall → fetch
# ---------------------------------------------------------------------------

def test_e2e_record_recall_fetch(memory_env):
    """A tool returns a list-shaped result; we should be able to find it
    via semantic recall and fetch the verbatim record."""
    registry, store = memory_env

    # Register a fake materials_search-like tool with content worth recording
    # (must exceed _MIN_BYTES=512 in canonical JSON to trip the heuristic)
    materials_payload = {
        "results": [
            {
                "formula": "Ti6Al4V", "density": 4.43, "tensile_mpa": 950,
                "yield_mpa": 880, "elongation_pct": 14, "modulus_gpa": 113.8,
                "elements": ["Ti", "Al", "V"], "use_cases": ["aerospace", "biomedical"],
            },
            {
                "formula": "Inconel718", "density": 8.19, "tensile_mpa": 1240,
                "yield_mpa": 1036, "elongation_pct": 12, "modulus_gpa": 200,
                "elements": ["Ni", "Cr", "Fe", "Nb", "Mo"], "use_cases": ["aerospace", "turbine"],
            },
            {
                "formula": "Al7075", "density": 2.81, "tensile_mpa": 572,
                "yield_mpa": 503, "elongation_pct": 11, "modulus_gpa": 71.7,
                "elements": ["Al", "Zn", "Mg", "Cu"], "use_cases": ["aerospace", "structural"],
            },
            {
                "formula": "Steel316L", "density": 7.99, "tensile_mpa": 580,
                "yield_mpa": 290, "elongation_pct": 50, "modulus_gpa": 193,
                "elements": ["Fe", "Cr", "Ni", "Mo"], "use_cases": ["medical", "marine"],
            },
        ],
        "count": 4,
        "source": "test",
        "query": "high tensile aerospace alloys",
    }
    _register_dummy_tool(registry, "materials_search", materials_payload)

    # Execute the tool — should auto-record because of bootstrap wiring
    tool = registry.get("materials_search")
    result = tool.execute()

    # Result should be augmented with _artifact_id
    assert "_artifact_id" in result, f"missing _artifact_id in {result.keys()}"
    assert "_record_count" in result
    assert result["_record_count"] == 4
    artifact_id = result["_artifact_id"]

    # Original data preserved
    assert result["count"] == 4
    assert len(result["results"]) == 4

    # Now recall — should hit the artifact via BM25 OR vector
    recall = registry.get("recall")
    hits = recall.execute(query="aerospace alloys tensile", limit=5)
    assert hits["count"] >= 1, f"no hits, got {hits}"
    artifact_ids = {h["artifact_id"] for h in hits["hits"]}
    assert artifact_id in artifact_ids, (
        f"recall didn't find recorded artifact. Hits: {hits}"
    )

    # Fetch the full data — should return the verbatim original
    fetch = registry.get("fetch_artifact")
    art = fetch.execute(artifact_id=artifact_id)
    assert art["tool"] == "materials_search"
    assert art["session_id"] == "test-session"
    assert art["record_count"] == 4
    assert art["result"]["query"] == "high tensile aerospace alloys"
    # Verbatim — same number of records as the original
    assert len(art["result"]["results"]) == 4


# ---------------------------------------------------------------------------
# Scenario 2: per-record recall + verbatim fetch by record_idx
# ---------------------------------------------------------------------------

def test_per_record_recall_and_fetch(memory_env):
    """recall should be able to surface a specific record from a list-shaped
    artifact, and fetch_artifact should return that record."""
    registry, store = memory_env

    payload = {
        "results": [
            {
                "name": "Ti-6Al-4V titanium aerospace alloy fatigue resistance",
                "details": "alpha-beta titanium alloy widely used in aerospace structural components, "
                           "biomedical implants, and high-performance applications. "
                           "Known for strength-to-weight ratio and fatigue endurance.",
            },
            {
                "name": "stainless steel 316L corrosion resistance",
                "details": "austenitic chromium-nickel stainless steel with molybdenum addition. "
                           "Excellent corrosion resistance in marine and chloride environments. "
                           "Used in medical implants, food processing, and chemical equipment.",
            },
            {
                "name": "Inconel 718 nickel superalloy creep",
                "details": "precipitation-hardenable nickel-chromium superalloy with high strength "
                           "from cryogenic to 700°C. Heavy use in gas turbine engines, rocket motors, "
                           "and high-temperature aerospace components. Excellent creep resistance.",
            },
        ],
        "count": 3,
        "source": "test_per_record",
    }
    _register_dummy_tool(registry, "search", payload)
    result = registry.get("search").execute()
    artifact_id = result["_artifact_id"]

    # Recall a record-level match
    hits = registry.get("recall").execute(query="titanium aerospace", limit=10)
    # At least one hit should be record-level (has record_idx) and point at our artifact
    record_hits = [h for h in hits["hits"] if h.get("record_idx") is not None]
    assert any(h["artifact_id"] == artifact_id for h in record_hits), (
        f"no record-level hit for the titanium query. hits={hits}"
    )

    # Fetch a specific record
    fetched = registry.get("fetch_artifact").execute(
        artifact_id=artifact_id, record_idx=0,
    )
    assert "Ti-6Al-4V" in fetched["record"]["name"]


# ---------------------------------------------------------------------------
# Scenario 3: small / error-shaped results are NOT recorded
# ---------------------------------------------------------------------------

def test_small_results_not_recorded(memory_env):
    """The recorder's heuristic skips small/error results."""
    registry, store = memory_env

    # Tiny result — well under 512 bytes
    _register_dummy_tool(registry, "tiny", {"results": [{"x": 1}, {"x": 2}]})
    r = registry.get("tiny").execute()
    # No artifact_id should be added
    assert "_artifact_id" not in r, f"small result should NOT be recorded, got {r}"

    # Pure error — should NOT be recorded
    _register_dummy_tool(registry, "errors_only",
                         {"error": "X" * 1000})  # large but error-only
    r2 = registry.get("errors_only").execute()
    assert "_artifact_id" not in r2

    # Verify no artifacts were created
    assert store.list_artifacts(session_id="test-session") == []


# ---------------------------------------------------------------------------
# Scenario 4: the memory tools themselves don't get recorded
# ---------------------------------------------------------------------------

def test_memory_tools_opt_out(memory_env):
    """recall / fetch / list don't create their own artifacts — would cause
    pointless self-indexing and risk infinite recursion."""
    registry, store = memory_env

    # Seed an artifact (large enough to trip the recording threshold)
    _register_dummy_tool(registry, "seed",
                         {"results": [{"a": i, "padding": "x" * 30} for i in range(20)]})
    registry.get("seed").execute()
    initial_count = len(store.list_artifacts(session_id="test-session"))

    # Call recall + list multiple times
    registry.get("recall").execute(query="anything", limit=5)
    registry.get("recall").execute(query="more", limit=5)
    registry.get("list_artifacts").execute()
    artifacts = store.list_artifacts(session_id="test-session")
    art_id = artifacts[0]["artifact_id"]
    registry.get("fetch_artifact").execute(artifact_id=art_id)

    # Count should be unchanged
    final_count = len(store.list_artifacts(session_id="test-session"))
    assert final_count == initial_count, (
        f"memory tools created artifacts: {initial_count} → {final_count}"
    )


# ---------------------------------------------------------------------------
# Scenario 5: graceful degradation when memory is not configured
# ---------------------------------------------------------------------------

def test_graceful_when_memory_disabled(tmp_path: Path):
    """If the recorder isn't configured, Tool.execute MUST still return the
    original tool result. This is the contract: storage failures never
    break tool execution."""
    # Don't configure memory — fresh state
    reset_memory()
    assert not is_configured()

    registry = ToolRegistry()
    payload = {
        "results": [{"x": i, "padding": "y" * 30} for i in range(10)],
        "count": 10,
    }
    _register_dummy_tool(registry, "graceful", payload)

    result = registry.get("graceful").execute()

    # Result should be identical to the payload — no _artifact_id, no augmentation
    assert "_artifact_id" not in result
    assert result["count"] == 10
    assert len(result["results"]) == 10


# ---------------------------------------------------------------------------
# Scenario 6: provenance — every recorded artifact has reproducible metadata
# ---------------------------------------------------------------------------

def test_provenance_preserved(memory_env):
    """Every artifact tracks tool_name, args, session_id, created_at."""
    registry, store = memory_env

    payload = {
        "results": [{"a": i, "padding": "z" * 200} for i in range(5)],
        "count": 5,
        "source": "provenance_test",
    }
    _register_dummy_tool(registry, "provenance_tool", payload)
    result = registry.get("provenance_tool").execute(some_arg="value123")
    art_id = result["_artifact_id"]

    art = store.get(art_id)
    assert art is not None
    assert art.tool_name == "provenance_tool"
    assert art.args == {"some_arg": "value123"}
    assert art.session_id == "test-session"
    assert art.created_at  # populated
    assert art.bytes_size > 0


# ---------------------------------------------------------------------------
# Scenario 7: cross-session recall via scope='all'
# ---------------------------------------------------------------------------

def test_cross_session_recall(memory_env):
    """scope='all' should reach across sessions; scope='session' should not."""
    registry, store = memory_env

    # Record in session A (must exceed _MIN_BYTES threshold)
    _register_dummy_tool(registry, "tool_a", {
        "results": [
            {"unique_alpha_token": 1, "padding": "p" * 250},
            {"unique_alpha_token": 2, "padding": "p" * 250},
            {"unique_alpha_token": 3, "padding": "p" * 250},
        ],
        "count": 3,
        "source": "session_test",
    })

    # Reconfigure to session A
    configure_memory(session_id="alpha")
    registry.get("tool_a").execute()

    # Reconfigure to session B
    configure_memory(session_id="beta")
    registry.get("tool_a").execute()  # still records — different session

    # From session B, scope='session' should not see alpha's content
    hits_session = registry.get("recall").execute(
        query="unique_alpha_token", scope="session", limit=10,
    )
    session_artifact_ids = {h["artifact_id"] for h in hits_session["hits"]}
    # Both records here are essentially identical content; what matters is
    # that the artifact in BETA's session is the only one returned.
    beta_artifacts = store.list_artifacts(session_id="beta")
    alpha_artifacts = store.list_artifacts(session_id="alpha")
    assert len(beta_artifacts) >= 1
    assert len(alpha_artifacts) >= 1
    for a_id in (a["artifact_id"] for a in alpha_artifacts):
        assert a_id not in session_artifact_ids, (
            f"session scope leaked alpha artifact {a_id}"
        )

    # scope='all' should include both
    hits_all = registry.get("recall").execute(
        query="unique_alpha_token", scope="all", limit=10,
    )
    all_artifact_ids = {h["artifact_id"] for h in hits_all["hits"]}
    # At least one alpha artifact should appear
    alpha_ids = {a["artifact_id"] for a in alpha_artifacts}
    assert all_artifact_ids & alpha_ids, (
        f"scope='all' missed alpha artifacts. all={all_artifact_ids}, alpha={alpha_ids}"
    )
