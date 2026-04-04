"""Integration test: materials discovery workflow.

Tests the full search → identify gaps → fill → ingest flow
that the agent follows when executing the materials_discovery workflow.
Each step is tested independently to verify the tools work together.
"""
import json
import os
import pytest


class TestMaterialsDiscoveryFlow:
    """End-to-end flow: search → gaps → fill → present → ingest."""

    # ── Step 1: OPTIMADE search returns results ────────────────────

    def test_step1_search_returns_materials(self):
        """OPTIMADE federated search should find materials."""
        from app.tools.data import _search_materials

        result = _search_materials(elements=["Ti", "Al"], limit=5)

        assert "error" not in result, f"Search failed: {result.get('error')}"
        assert result["count"] > 0, "No materials found"
        assert len(result["results"]) > 0
        # Each result should have at minimum: id, formula, elements
        for mat in result["results"]:
            assert "id" in mat
            assert "formula" in mat
            assert "elements" in mat

    # ── Step 2: Identify property gaps ─────────────────────────────

    def test_step2_identify_gaps_in_results(self):
        """Search results should have property gaps we can identify."""
        from app.tools.data import _search_materials

        result = _search_materials(elements=["Si"], limit=3)
        assert "error" not in result

        target_props = ["band_gap", "formation_energy"]
        gaps = []
        for mat in result["results"]:
            for prop in target_props:
                if prop not in mat:
                    gaps.append({
                        "material_id": mat["id"],
                        "formula": mat["formula"],
                        "missing_property": prop,
                    })

        # We expect gaps — most OPTIMADE results don't have all properties
        assert len(gaps) > 0, "Expected property gaps but found none"

    # ── Step 3: ML models are available for gap-filling ────────────

    def test_step3_ml_models_available(self):
        """Pre-trained GNN models should be listed (even if not installed)."""
        from app.tools.prediction import _list_models

        result = _list_models()
        assert "pretrained_models" in result
        models = result["pretrained_models"]
        assert len(models) >= 3, f"Expected 3+ pretrained models, got {len(models)}"

        # Check we have band_gap and formation_energy models
        props = {m["property"] for m in models}
        assert "band_gap" in props
        assert "formation_energy" in props

    # ── Step 4: Prediction tool handles missing model gracefully ───

    def test_step4_predict_graceful_when_no_model(self):
        """predict_property should return an error, not crash, when no model."""
        from app.tools.prediction import _predict_property

        result = _predict_property(formula="TiAl", target_property="band_gap")
        # Should return an error dict, not raise
        assert isinstance(result, dict)
        assert "error" in result

    # ── Step 5: CSV export works ───────────────────────────────────

    def test_step5_export_results_csv(self):
        """Export tool should write valid CSV from search results."""
        from app.tools.data import _export_results_csv
        import tempfile

        data = [
            {"formula": "TiAl", "band_gap": 1.5, "source": "predicted"},
            {"formula": "Ti3Al", "band_gap": None, "source": "missing"},
        ]
        with tempfile.NamedTemporaryFile(suffix=".csv", delete=False) as f:
            path = f.name

        try:
            result = _export_results_csv(results=data, filename=path)
            assert "error" not in result, f"Export failed: {result}"
            assert os.path.exists(path)
            with open(path) as f:
                content = f.read()
            assert "TiAl" in content
            assert "formula" in content
        finally:
            os.unlink(path)

    # ── Step 6: Dataset import works ───────────────────────────────

    def test_step6_import_dataset(self):
        """Import tool should accept data as a named dataset."""
        from app.tools.data import _import_dataset
        import tempfile

        # Create a small CSV
        csv_content = "formula,band_gap\nTiAl,1.5\nTi3Al,0.8\n"
        with tempfile.NamedTemporaryFile(
            suffix=".csv", mode="w", delete=False
        ) as f:
            f.write(csv_content)
            path = f.name

        try:
            result = _import_dataset(file_path=path, name="test-discovery")
            assert "error" not in result, f"Import failed: {result}"
        finally:
            os.unlink(path)

    # ── Step 7: Tool registry has all needed tools ─────────────────

    def test_step7_all_workflow_tools_available(self):
        """All tools referenced by materials_discovery workflow must exist."""
        from app.plugins.bootstrap import build_full_registry

        reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        tool_names = {t.name for t in reg.list_tools()}

        required = [
            "search_materials",
            "query_materials_project",
            "predict_property",
            "predict_structure",
            "list_models",
            "export_results_csv",
            "import_dataset",
            "execute_python",
            "discover_capabilities",
        ]
        missing = [t for t in required if t not in tool_names]
        assert not missing, f"Missing tools for workflow: {missing}"

    # ── Step 8: Workflow YAML is valid ─────────────────────────────

    def test_step8_workflow_yaml_is_valid(self):
        """materials_discovery.yaml should parse correctly."""
        import yaml

        path = os.path.join(
            os.path.dirname(__file__),
            "..",
            "app",
            "workflows",
            "builtin",
            "materials_discovery.yaml",
        )
        with open(path) as f:
            spec = yaml.safe_load(f)

        assert spec["name"] == "materials_discovery"
        assert spec["kind"] == "skill_workflow"
        assert len(spec["steps"]) >= 7
        assert len(spec["inputs"]) >= 3

        # Each step should have name, skill, description
        for step in spec["steps"]:
            assert "name" in step, f"Step missing name: {step}"
            assert "skill" in step, f"Step missing skill: {step}"
            assert "description" in step, f"Step missing description: {step}"

    # ── Step 9: Search with band_gap filter ────────────────────────

    def test_step9_search_with_property_filter(self):
        """Search should accept band_gap range filter."""
        from app.tools.data import _search_materials

        result = _search_materials(
            elements=["Si"],
            band_gap_min=0.5,
            band_gap_max=2.0,
            limit=3,
        )
        # May return 0 results if no provider supports band_gap filter,
        # but should not error
        assert "error" not in result, f"Search with filter failed: {result.get('error')}"

    # ── Step 10: Capabilities discovery works ──────────────────────

    def test_step10_discover_capabilities(self):
        """discover_capabilities tool should report what's available."""
        from app.tools.capabilities import discover_capabilities

        result = discover_capabilities()
        assert "error" not in result
        # Should report on search providers, models, etc.
        assert "search_providers" in result or "tools" in result or "capabilities" in result
