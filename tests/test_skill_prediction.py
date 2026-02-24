"""Tests for the prediction skill."""

from unittest.mock import MagicMock, patch

import numpy as np
import pandas as pd
import pytest

from app.skills.prediction import PREDICT_SKILL, _predict_properties


@pytest.fixture
def mock_prefs(monkeypatch):
    from app.config.preferences import UserPreferences

    prefs = UserPreferences(default_algorithm="random_forest")
    monkeypatch.setattr(
        "app.skills.prediction.UserPreferences.load", lambda: prefs
    )
    return prefs


@pytest.fixture
def sample_df():
    return pd.DataFrame(
        {
            "formula": ["Fe2O3", "Al2O3", "SiO2", "TiO2", "MgO", "CaO"],
            "band_gap": [2.0, 8.8, 9.0, 3.0, 7.8, 7.0],
            "formation_energy_per_atom": [-1.5, -3.4, -3.0, -3.2, -3.0, -3.5],
            "source_id": ["a", "b", "c", "d", "e", "f"],
        }
    )


class TestPredictSkill:
    def test_skill_metadata(self):
        assert PREDICT_SKILL.name == "predict_properties"
        assert len(PREDICT_SKILL.steps) == 5
        tool = PREDICT_SKILL.to_tool()
        assert tool.name == "predict_properties"

    @patch("app.ml.registry.ModelRegistry.load_model")
    @patch("app.data.store.DataStore.save")
    @patch("app.data.store.DataStore.load")
    def test_predict_with_existing_model(
        self, mock_load, mock_save, mock_load_model, mock_prefs, sample_df
    ):
        mock_load.return_value = sample_df

        mock_model = MagicMock()
        mock_model.predict.return_value = np.array([5.0])
        mock_load_model.return_value = mock_model
        mock_save.return_value = "/tmp/test.parquet"

        result = _predict_properties(
            dataset_name="test_data", properties=["band_gap"]
        )

        assert "predictions" in result
        assert "band_gap" in result["predictions"]
        assert result["rows"] == 6

    @patch("app.data.store.DataStore.load")
    def test_dataset_not_found(self, mock_load, mock_prefs):
        mock_load.side_effect = FileNotFoundError()

        result = _predict_properties(dataset_name="nonexistent")
        assert "error" in result
        assert "not found" in result["error"]

    @patch("app.ml.registry.ModelRegistry.load_model")
    @patch("app.data.store.DataStore.load")
    def test_no_numeric_columns(self, mock_load, mock_load_model, mock_prefs):
        df = pd.DataFrame(
            {"formula": ["Fe2O3"], "source_id": ["a"], "provider": ["mp"]}
        )
        mock_load.return_value = df

        result = _predict_properties(dataset_name="text_only")
        assert "error" in result

    @patch("app.ml.trainer.train_model")
    @patch("app.ml.registry.ModelRegistry.save_model")
    @patch("app.ml.registry.ModelRegistry.load_model")
    @patch("app.data.store.DataStore.save")
    @patch("app.data.store.DataStore.load")
    def test_train_if_missing(
        self, mock_load, mock_save, mock_load_model, mock_save_model,
        mock_train, mock_prefs, sample_df
    ):
        mock_load.return_value = sample_df

        # No existing model
        mock_load_model.return_value = None

        mock_model = MagicMock()
        mock_model.predict.return_value = np.array([5.0])
        mock_train.return_value = {
            "model": mock_model,
            "metrics": {"mae": 0.1},
            "algorithm": "random_forest",
            "property_name": "band_gap",
        }
        mock_save.return_value = "/tmp/test.parquet"

        result = _predict_properties(
            dataset_name="test_data", properties=["band_gap"]
        )

        assert "predictions" in result
        mock_train.assert_called_once()
        mock_save_model.assert_called_once()

    @patch("app.data.store.DataStore.load")
    def test_no_formula_column(self, mock_load, mock_prefs):
        df = pd.DataFrame({"value": [1.0, 2.0], "source_id": ["a", "b"]})
        mock_load.return_value = df

        result = _predict_properties(dataset_name="no_formula")
        assert "error" in result
        assert "formula" in result["error"].lower()
