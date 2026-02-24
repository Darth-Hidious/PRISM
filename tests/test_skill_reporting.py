"""Tests for the reporting skill."""

from pathlib import Path
from unittest.mock import patch

import pandas as pd
import pytest

from app.skills.reporting import REPORT_SKILL, _generate_report


@pytest.fixture
def mock_prefs(monkeypatch, tmp_path):
    from app.config.preferences import UserPreferences

    prefs = UserPreferences(
        output_dir=str(tmp_path / "output"), report_format="markdown"
    )
    monkeypatch.setattr(
        "app.skills.reporting.UserPreferences.load", lambda: prefs
    )
    return prefs


@pytest.fixture
def sample_df():
    return pd.DataFrame(
        {
            "formula": ["Fe2O3", "Al2O3", "SiO2"],
            "band_gap": [2.0, 8.8, 9.0],
            "predicted_band_gap": [2.1, 8.5, 9.2],
            "source_id": ["a", "b", "c"],
        }
    )


class TestReportSkill:
    def test_skill_metadata(self):
        assert REPORT_SKILL.name == "generate_report"
        tool = REPORT_SKILL.to_tool()
        assert tool.name == "generate_report"

    @patch("app.data.store.DataStore.load")
    def test_generate_markdown(self, mock_load, mock_prefs, sample_df):
        mock_load.return_value = sample_df

        result = _generate_report(dataset_name="test_data")

        assert result["format"] == "markdown"
        report_path = result["report_path"]
        content = Path(report_path).read_text()
        assert "# PRISM Report" in content
        assert "Dataset Summary" in content
        assert "Fe2O3" in content
        assert "Property Statistics" in content
        assert "ML Predictions" in content
        assert "band_gap" in content

    @patch("app.data.store.DataStore.load")
    def test_data_preview_table(self, mock_load, mock_prefs, sample_df):
        mock_load.return_value = sample_df

        result = _generate_report(dataset_name="test_data")

        content = Path(result["report_path"]).read_text()
        assert "Data Preview" in content
        assert "| formula" in content

    def test_report_without_dataset(self, mock_prefs):
        result = _generate_report(dataset_name="nonexistent", title="Empty Report")

        assert result["format"] == "markdown"
        content = Path(result["report_path"]).read_text()
        assert "Empty Report" in content

    @patch("app.data.store.DataStore.load")
    def test_custom_output_path(self, mock_load, mock_prefs, sample_df, tmp_path):
        mock_load.return_value = sample_df
        out = str(tmp_path / "custom_report.md")

        result = _generate_report(dataset_name="test_data", output_path=out)

        assert result["report_path"] == out
        assert Path(out).exists()

    @patch("app.data.store.DataStore.load")
    def test_custom_title(self, mock_load, mock_prefs, sample_df):
        mock_load.return_value = sample_df

        result = _generate_report(dataset_name="test_data", title="My Custom Title")

        content = Path(result["report_path"]).read_text()
        assert "My Custom Title" in content
