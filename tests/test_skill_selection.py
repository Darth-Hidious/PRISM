"""Tests for the selection skill."""

from unittest.mock import patch

import pandas as pd
import pytest

from app.skills.selection import SELECT_SKILL, _select_materials


@pytest.fixture
def sample_df():
    return pd.DataFrame(
        {
            "formula": ["Fe2O3", "Al2O3", "SiO2", "TiO2", "MgO"],
            "band_gap": [2.0, 8.8, 9.0, 3.0, 7.8],
            "formation_energy_per_atom": [-1.5, -3.4, -3.0, -3.2, -3.0],
            "source_id": ["a", "b", "c", "d", "e"],
        }
    )


class TestSelectSkill:
    def test_skill_metadata(self):
        assert SELECT_SKILL.name == "select_materials"
        assert len(SELECT_SKILL.steps) == 5
        tool = SELECT_SKILL.to_tool()
        assert tool.name == "select_materials"

    @patch("app.data.store.DataStore.save")
    @patch("app.data.store.DataStore.load")
    def test_select_with_criteria(self, mock_load, mock_save, sample_df):
        mock_load.return_value = sample_df
        mock_save.return_value = "/tmp/test.parquet"

        result = _select_materials(
            dataset_name="test_data",
            criteria={"band_gap_min": 3.0, "band_gap_max": 9.0},
        )

        assert result["selected_count"] <= 5
        assert result["dataset_name"] == "test_data_selected"

    @patch("app.data.store.DataStore.save")
    @patch("app.data.store.DataStore.load")
    def test_select_sort_and_top_n(self, mock_load, mock_save, sample_df):
        mock_load.return_value = sample_df
        mock_save.return_value = "/tmp/test.parquet"

        result = _select_materials(
            dataset_name="test_data",
            sort_by="band_gap",
            top_n=2,
        )

        assert result["selected_count"] == 2

    @patch("app.data.store.DataStore.load")
    def test_dataset_not_found(self, mock_load):
        mock_load.side_effect = FileNotFoundError()

        result = _select_materials(dataset_name="nonexistent")
        assert "error" in result

    @patch("app.data.store.DataStore.load")
    def test_no_matches(self, mock_load, sample_df):
        mock_load.return_value = sample_df

        result = _select_materials(
            dataset_name="test_data",
            criteria={"band_gap_min": 100.0},
        )
        assert "error" in result

    @patch("app.data.store.DataStore.save")
    @patch("app.data.store.DataStore.load")
    def test_custom_output_name(self, mock_load, mock_save, sample_df):
        mock_load.return_value = sample_df
        mock_save.return_value = "/tmp/test.parquet"

        result = _select_materials(
            dataset_name="test_data", output_name="my_picks"
        )
        assert result["dataset_name"] == "my_picks"
