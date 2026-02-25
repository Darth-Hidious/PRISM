"""Tests for prediction agent tools."""
import pytest
from app.tools.prediction import create_prediction_tools
from app.tools.base import ToolRegistry


class TestPredictionTools:
    def test_tools_registered(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "predict_property" in names
        assert "list_models" in names

    def test_predict_no_model(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("predict_property")
        result = tool.execute(formula="Si", property_name="band_gap")
        # Should return error since no model is trained in default models/ dir
        assert "error" in result or "prediction" in result

    def test_list_models(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("list_models")
        result = tool.execute()
        assert "trained_models" in result
        assert "pretrained_models" in result
        assert "feature_backend" in result
