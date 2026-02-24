"""Tests for the discovery master skill."""

from unittest.mock import patch

import pandas as pd
import pytest

from app.skills.discovery import DISCOVER_SKILL, _materials_discovery


@pytest.fixture
def mock_prefs(monkeypatch):
    from app.config.preferences import UserPreferences

    prefs = UserPreferences(
        default_providers=["optimade"],
        output_dir="/tmp/prism_test",
    )
    monkeypatch.setattr(
        "app.skills.acquisition.UserPreferences.load", lambda: prefs
    )
    monkeypatch.setattr(
        "app.skills.prediction.UserPreferences.load", lambda: prefs
    )
    monkeypatch.setattr(
        "app.skills.visualization.UserPreferences.load", lambda: prefs
    )
    monkeypatch.setattr(
        "app.skills.reporting.UserPreferences.load", lambda: prefs
    )
    return prefs


class TestDiscoverySkill:
    def test_skill_metadata(self):
        assert DISCOVER_SKILL.name == "materials_discovery"
        assert len(DISCOVER_SKILL.steps) == 4
        tool = DISCOVER_SKILL.to_tool()
        assert tool.name == "materials_discovery"

    @patch("app.skills.reporting._generate_report")
    @patch("app.skills.visualization._visualize_dataset")
    @patch("app.skills.prediction._predict_properties")
    @patch("app.skills.acquisition._acquire_materials")
    def test_full_pipeline(
        self, mock_acquire, mock_predict, mock_viz, mock_report, mock_prefs
    ):
        mock_acquire.return_value = {
            "dataset_name": "w_rh_discovery",
            "total_records": 5,
            "columns": ["formula", "band_gap"],
            "sources_queried": ["optimade"],
        }
        mock_predict.return_value = {
            "dataset_name": "w_rh_discovery",
            "predictions": {"band_gap": "predicted_band_gap"},
        }
        mock_viz.return_value = {
            "dataset_name": "w_rh_discovery",
            "plots": ["dist.png"],
        }
        mock_report.return_value = {
            "report_path": "/tmp/report.md",
            "format": "markdown",
        }

        result = _materials_discovery(elements=["W", "Rh"])

        assert result["dataset_name"] == "w_rh_discovery"
        assert "acquisition" in result["results"]
        assert "prediction" in result["results"]
        assert "visualization" in result["results"]
        assert "report" in result["results"]
        mock_acquire.assert_called_once()
        mock_predict.assert_called_once()

    @patch("app.skills.acquisition._acquire_materials")
    def test_acquisition_failure_stops(self, mock_acquire, mock_prefs):
        mock_acquire.return_value = {"error": "No records found"}

        result = _materials_discovery(elements=["Zz"])
        assert "error" in result

    @patch("app.skills.reporting._generate_report")
    @patch("app.skills.visualization._visualize_dataset")
    @patch("app.skills.prediction._predict_properties")
    @patch("app.skills.acquisition._acquire_materials")
    def test_prediction_failure_continues(
        self, mock_acquire, mock_predict, mock_viz, mock_report, mock_prefs
    ):
        mock_acquire.return_value = {
            "dataset_name": "test",
            "total_records": 3,
            "columns": [],
            "sources_queried": [],
        }
        mock_predict.side_effect = Exception("ML failed")
        mock_viz.return_value = {"plots": []}
        mock_report.return_value = {"report_path": "/tmp/r.md", "format": "markdown"}

        result = _materials_discovery(elements=["Fe"])
        # Should NOT have top-level error
        assert "error" not in result
        assert "error" in result["results"]["prediction"]
