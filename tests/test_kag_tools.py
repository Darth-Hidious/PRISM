"""Tests for KAG-style tool reasoning and session context builder."""
import json
import os
import pytest

from app.tools.tool_reasoning import (
    _tool_reasoning, _classify_intent, LOGICAL_FORMS,
    KNOWLEDGE_BOUNDARY_PATTERNS, TOOL_GRAPH,
)
from app.tools.session_context import (
    _session_context, _fresh_session, _save_session, _load_session,
)


# ── tool_reasoning tests ────────────────────────────────────────────


class TestIntentClassification:
    """Verify the KAG logical-form intent classifier."""

    @pytest.mark.parametrize("query,expected_intent,expected_tools", [
        ("Find me a stable TiAl alloy for aerospace", "discover_materials", True),
        ("Evaluate W0.25 Mo0.25 Ta0.25 Nb0.25", "evaluate_composition", True),
        ("What is the formation energy of CuNi?", "evaluate_composition", True),
        ("Search Materials Project for nickel superalloys", "search_existing", True),
        ("Submit a DFT job on the GPU cluster", "run_compute", True),
        ("Show me the mesh peers", "mesh_operations", True),
        ("What did we discover yesterday?", "recall_memory", True),
        ("Hello, how are you?", "direct_answer", False),
    ])
    def test_intent_classification(self, query, expected_intent, expected_tools):
        result = _classify_intent(query)
        assert result["intent"] == expected_intent, \
            f"Expected {expected_intent}, got {result['intent']} for: {query}"
        assert result["needs_tools"] == expected_tools

    def test_composition_pattern_detected(self):
        """Fe0.3Ni0.7 style patterns should trigger evaluate intent."""
        result = _classify_intent("Fe0.3 Ni0.7")
        assert result["needs_tools"] is True

    def test_unknown_query_falls_back(self):
        """Unrecognized queries return unknown with keyword suggestions."""
        result = _classify_intent("xyzzy frobnicate")
        assert result["intent"] == "unknown"

    def test_greeting_no_tools(self):
        """Greetings should not trigger tool usage."""
        for greeting in ["hi", "hello", "hey", "thanks", "bye"]:
            result = _classify_intent(greeting)
            assert result["intent"] == "direct_answer"
            assert result["needs_tools"] is False


class TestToolReasoningOutput:
    """Verify the full tool_reasoning output structure."""

    def test_returns_explanation(self):
        result = _tool_reasoning(query="Find a refractory HEA")
        assert "explanation" in result
        assert len(result["explanation"]) > 10

    def test_returns_tool_graph(self):
        result = _tool_reasoning(query="Discover new W-Mo-Ta alloys")
        assert "tool_graph" in result
        assert "nodes" in result["tool_graph"]
        assert "edges" in result["tool_graph"]

    def test_returns_recommended_tools(self):
        result = _tool_reasoning(query="Search for Ti alloys")
        tools = result["classification"]["recommended_tools"]
        assert len(tools) > 0
        assert all("tool" in t for t in tools)

    def test_empty_query_errors(self):
        result = _tool_reasoning(query="")
        assert "error" in result

    def test_data_flow_present(self):
        result = _tool_reasoning(query="Find stable HEA compositions")
        assert result["classification"]["data_flow"] != ""


class TestToolGraph:
    """Verify the tool relationship graph."""

    def test_search_materials_feeds_alpha(self):
        assert "search_materials" in TOOL_GRAPH
        targets = [t["tool"] for t in TOOL_GRAPH["search_materials"]["feeds_into"]]
        assert "alpha_predict" in targets


# ── session_context tests ───────────────────────────────────────────


class TestSessionContext:
    """Verify the session context builder."""

    def setup_method(self):
        """Use a test session ID and start fresh."""
        os.environ["PRISM_SESSION_ID"] = "test_pytest"
        global _CURRENT_SESSION
        import app.tools.session_context as sc
        sc._CURRENT_SESSION = None
        self._cleanup()

    def teardown_method(self):
        self._cleanup()

    def _cleanup(self):
        from pathlib import Path
        p = Path.home() / ".prism" / "sessions" / "test_pytest.json"
        if p.exists():
            p.unlink()

    def test_empty_session_status(self):
        result = _session_context(action="status")
        assert result["n_compositions"] == 0
        assert result["n_discoveries"] == 0

    def test_record_evaluation(self):
        result = _session_context(
            action="record",
            tool="alpha_predict",
            args=json.dumps({"formula": "W0.25 Mo0.25 Ta0.25 Nb0.25"}),
            result=json.dumps({
                "verifiers": {
                    "physics": {"delta": 2.46, "entropy_j_mol_k": 11.5,
                                "density_g_cm3": 13.2}
                }
            }),
            elapsed_s=0.5,
        )
        assert result["status"] == "recorded"
        assert result["n_total_evaluated"] == 1

    def test_best_values_tracked(self):
        _session_context(
            action="record", tool="alpha_predict",
            args=json.dumps({"formula": "W0.25 Mo0.25 Ta0.25 Nb0.25"}),
            result=json.dumps({"verifiers": {"physics": {"delta": 2.46}}}),
            elapsed_s=0.1,
        )
        _session_context(
            action="record", tool="alpha_predict",
            args=json.dumps({"formula": "Fe0.5 Ni0.5"}),
            result=json.dumps({"verifiers": {"physics": {"delta": 1.59}}}),
            elapsed_s=0.1,
        )
        result = _session_context(action="query", key="best")
        # delta is a "min" objective conceptually, but we track raw best
        assert "delta" in result["best_per_objective"]

    def test_element_systems_tracked(self):
        _session_context(
            action="record", tool="alpha_predict",
            args=json.dumps({"formula": "W0.25 Mo0.25 Ta0.25 Nb0.25"}),
            result=json.dumps({"verifiers": {"physics": {"delta": 2.46}}}),
            elapsed_s=0.1,
        )
        result = _session_context(action="query", key="element_systems")
        assert "Mo-Nb-Ta-W" in result["element_systems"]

    def test_compact_builds_summary(self):
        _session_context(
            action="record", tool="alpha_predict",
            args=json.dumps({"formula": "Cu0.5 Ni0.5"}),
            result=json.dumps({"verifiers": {"physics": {"delta": 1.59}}}),
            elapsed_s=0.1,
        )
        result = _session_context(action="compact")
        assert "summary" in result
        assert "Cu" in result["summary"]
        assert result["token_estimate"] < 500

    def test_reset_clears_session(self):
        _session_context(
            action="record", tool="alpha_predict",
            args=json.dumps({"formula": "Cu0.5 Ni0.5"}),
            result=json.dumps({"verifiers": {"physics": {"delta": 1.59}}}),
            elapsed_s=0.1,
        )
        _session_context(action="reset")
        result = _session_context(action="status")
        assert result["n_compositions"] == 0

    def test_query_unknown_key_returns_hint(self):
        result = _session_context(action="query", key="nonexistent")
        assert "available_keys" in result

    def test_persists_across_calls(self):
        """Session context should persist between calls (to disk)."""
        _session_context(
            action="record", tool="alpha_predict",
            args=json.dumps({"formula": "Fe0.3 Ni0.3 Cr0.4"}),
            result=json.dumps({"verifiers": {"physics": {"delta": 0.62}}}),
            elapsed_s=0.1,
        )
        # Force reload from disk
        import app.tools.session_context as sc
        sc._CURRENT_SESSION = None
        result = _session_context(action="status")
        assert result["n_compositions"] == 1


# ── Integration: both tools together ─────────────────────────────────


class TestIntegration:
    """Verify tool_reasoning + session_context work together."""

    def test_reason_then_record(self):
        """Agent calls tool_reasoning, then records the result."""
        # 1. Reason about intent
        reasoning = _tool_reasoning(query="Evaluate W0.25 Mo0.25 Ta0.25 Nb0.25")
        assert reasoning["classification"]["intent"] == "evaluate_composition"

        # 2. Agent would call alpha_predict, then record
        os.environ["PRISM_SESSION_ID"] = "test_integration"
        import app.tools.session_context as sc
        sc._CURRENT_SESSION = None

        _session_context(
            action="record", tool="alpha_predict",
            args=json.dumps({"formula": "W0.25 Mo0.25 Ta0.25 Nb0.25"}),
            result=json.dumps({"verifiers": {"physics": {"delta": 2.46}}}),
            elapsed_s=0.1,
        )

        # 3. Query accumulated knowledge
        compact = _session_context(action="compact")
        assert "W0.25" in compact["summary"] or "Mo-Nb-Ta-W" in compact["summary"]

        # Cleanup
        from pathlib import Path
        p = Path.home() / ".prism" / "sessions" / "test_integration.json"
        if p.exists():
            p.unlink()