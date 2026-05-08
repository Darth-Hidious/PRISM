"""Tests for prediction agent tools.

After Round 4 batch 2: `predict_property` and `predict_structure` were
collapsed into a single `predict(target=…)` tool. `list_models` stays
separate (different abstraction — registry inspection, not prediction).
"""
import pytest
from app.tools.prediction import create_prediction_tools
from app.tools.base import ToolRegistry


class TestPredictionTools:
    def test_tools_registered(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        names = [t.name for t in registry.list_tools()]
        # Unified predict tool replaces predict_property + predict_structure
        assert "predict" in names
        assert "list_models" in names
        # Old names must be gone — collapse, no aliases
        assert "predict_property" not in names
        assert "predict_structure" not in names

    def test_predict_target_formula(self):
        """target='formula' calls into the composition-based predictor."""
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("predict")
        result = tool.execute(target="formula", formula="Si", property_name="band_gap")
        # No trained models in default models/ dir → error path; but the
        # dispatch must have routed correctly (no "Missing 'target'" or
        # "Unknown target" errors).
        if "error" in result:
            err = result["error"]
            assert "Missing 'target'" not in err
            assert "Unknown target" not in err
            assert "requires `formula`" not in err  # we DID supply formula
        else:
            assert "prediction" in result or "predicted_value" in result

    def test_predict_target_structure_requires_dict(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("predict")
        # No structure → clear validation error
        result = tool.execute(target="structure")
        assert "error" in result
        assert "requires `structure` dict" in result["error"]

    def test_predict_missing_target(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("predict")
        result = tool.execute()
        assert "error" in result
        assert "Missing 'target'" in result["error"]

    def test_predict_unknown_target(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("predict")
        result = tool.execute(target="bogus_target")
        assert "error" in result
        assert "Unknown target" in result["error"]

    def test_predict_target_formula_requires_formula(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("predict")
        result = tool.execute(target="formula")
        assert "error" in result
        assert "requires `formula`" in result["error"]

    def test_list_models(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("list_models")
        result = tool.execute()
        assert "trained_models" in result
        assert "pretrained_models" in result
        assert "feature_backend" in result

    def test_predict_schema_advertises_two_targets(self):
        """The action enum must be 'formula' and 'structure'."""
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("predict")
        targets = tool.input_schema["properties"]["target"]["enum"]
        assert set(targets) == {"formula", "structure"}
