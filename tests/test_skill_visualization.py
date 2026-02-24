"""Tests for the visualization skill."""

from unittest.mock import patch

import pandas as pd
import pytest

from app.skills.visualization import VISUALIZE_SKILL, _visualize_dataset


@pytest.fixture
def mock_prefs(monkeypatch, tmp_path):
    from app.config.preferences import UserPreferences

    prefs = UserPreferences(output_dir=str(tmp_path / "plots"))
    monkeypatch.setattr(
        "app.skills.visualization.UserPreferences.load", lambda: prefs
    )
    return prefs


@pytest.fixture
def sample_df():
    return pd.DataFrame(
        {
            "formula": ["Fe2O3", "Al2O3", "SiO2"],
            "band_gap": [2.0, 8.8, 9.0],
            "formation_energy_per_atom": [-1.5, -3.4, -3.0],
            "source_id": ["a", "b", "c"],
        }
    )


class TestVisualizeSkill:
    def test_skill_metadata(self):
        assert VISUALIZE_SKILL.name == "visualize_dataset"
        assert len(VISUALIZE_SKILL.steps) == 3
        tool = VISUALIZE_SKILL.to_tool()
        assert tool.name == "visualize_dataset"

    @patch("app.tools.visualization._plot_materials_comparison")
    @patch("app.tools.visualization._plot_property_distribution")
    @patch("app.data.store.DataStore.load")
    def test_visualize_distributions(
        self, mock_load, mock_dist, mock_comp, mock_prefs, sample_df
    ):
        mock_load.return_value = sample_df
        mock_dist.return_value = {"success": True, "path": "test_dist.png"}
        mock_comp.return_value = {"success": True, "path": "test_comp.png"}

        result = _visualize_dataset(dataset_name="test_data")

        assert "plots" in result
        assert len(result["plots"]) > 0
        assert "band_gap" in result["columns_plotted"]

    @patch("app.data.store.DataStore.load")
    def test_dataset_not_found(self, mock_load, mock_prefs):
        mock_load.side_effect = FileNotFoundError()

        result = _visualize_dataset(dataset_name="nonexistent")
        assert "error" in result

    @patch("app.data.store.DataStore.load")
    def test_no_numeric_columns(self, mock_load, mock_prefs):
        df = pd.DataFrame({"formula": ["Fe2O3"], "source_id": ["a"]})
        mock_load.return_value = df

        result = _visualize_dataset(dataset_name="text_only")
        assert "error" in result

    @patch("app.tools.visualization._plot_property_distribution")
    @patch("app.data.store.DataStore.load")
    def test_specific_properties(self, mock_load, mock_dist, mock_prefs, sample_df):
        mock_load.return_value = sample_df
        mock_dist.return_value = {"success": True, "path": "test.png"}

        result = _visualize_dataset(
            dataset_name="test_data",
            properties=["band_gap"],
            chart_types=["distribution"],
        )

        assert result["columns_plotted"] == ["band_gap"]
