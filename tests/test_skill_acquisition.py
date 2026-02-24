"""Tests for the acquisition skill."""

from unittest.mock import patch

import pandas as pd
import pytest

from app.skills.acquisition import ACQUIRE_SKILL, _acquire_materials


@pytest.fixture
def mock_prefs(monkeypatch):
    from app.config.preferences import UserPreferences

    prefs = UserPreferences(default_providers=["optimade"], max_results_per_source=10)
    monkeypatch.setattr(
        "app.skills.acquisition.UserPreferences.load", lambda: prefs
    )
    return prefs


class TestAcquireSkill:
    def test_skill_metadata(self):
        assert ACQUIRE_SKILL.name == "acquire_materials"
        assert len(ACQUIRE_SKILL.steps) == 4
        tool = ACQUIRE_SKILL.to_tool()
        assert tool.name == "acquire_materials"

    @patch("app.data.store.DataStore.save")
    @patch("app.data.normalizer.normalize_records")
    @patch("app.data.collector.OPTIMADECollector.collect")
    def test_acquire_optimade(self, mock_collect, mock_normalize, mock_save, mock_prefs):
        mock_collect.return_value = [
            {"source_id": "mp:1", "formula": "WRh", "elements": ["W", "Rh"]},
        ]
        mock_normalize.return_value = pd.DataFrame(
            [{"source_id": "mp:1", "formula": "WRh", "elements": "Rh,W"}]
        )
        mock_save.return_value = "/tmp/test.parquet"

        result = _acquire_materials(elements=["W", "Rh"])

        assert result["total_records"] == 1
        assert result["dataset_name"] == "acquired_materials"
        mock_collect.assert_called_once()
        mock_normalize.assert_called_once()

    @patch("app.data.store.DataStore.save")
    @patch("app.data.normalizer.normalize_records")
    @patch("app.data.collector.MPCollector.collect")
    @patch("app.data.collector.OPTIMADECollector.collect")
    def test_acquire_both_sources(
        self, mock_opt, mock_mp, mock_normalize, mock_save, mock_prefs
    ):
        mock_prefs.default_providers = ["optimade", "mp"]

        mock_opt.return_value = [{"source_id": "opt:1", "formula": "WRh"}]
        mock_mp.return_value = [{"source_id": "mp:1", "formula": "WRh"}]
        mock_normalize.return_value = pd.DataFrame(
            [{"source_id": "opt:1"}, {"source_id": "mp:1"}]
        )
        mock_save.return_value = "/tmp/test.parquet"

        result = _acquire_materials(elements=["W", "Rh"])
        assert result["total_records"] == 2
        assert "optimade" in result["sources_queried"]
        assert "mp" in result["sources_queried"]

    @patch("app.data.collector.OPTIMADECollector.collect")
    def test_acquire_no_records(self, mock_collect, mock_prefs):
        mock_collect.return_value = []
        result = _acquire_materials(elements=["Zz"])
        assert "error" in result

    @patch("app.data.store.DataStore.save")
    @patch("app.data.normalizer.normalize_records")
    @patch("app.data.collector.OPTIMADECollector.collect")
    def test_custom_dataset_name(self, mock_collect, mock_norm, mock_save, mock_prefs):
        mock_collect.return_value = [{"source_id": "x:1"}]
        mock_norm.return_value = pd.DataFrame([{"source_id": "x:1"}])
        mock_save.return_value = "/tmp/test.parquet"

        result = _acquire_materials(elements=["Fe"], dataset_name="iron_alloys")
        assert result["dataset_name"] == "iron_alloys"
