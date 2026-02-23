"""Tests for predictor module."""
import tempfile
import pytest
import numpy as np
from app.ml.predictor import Predictor
from app.ml.registry import ModelRegistry


class TestPredictor:
    def _train_and_save_model(self, tmpdir):
        from sklearn.ensemble import RandomForestRegressor
        from app.ml.features import composition_features
        # Train with same feature count as composition_features produces
        n_features = len(composition_features("Si"))
        model = RandomForestRegressor(n_estimators=5, random_state=42)
        X = np.random.rand(30, n_features)
        y = np.random.rand(30)
        model.fit(X, y)

        registry = ModelRegistry(models_dir=tmpdir)
        registry.save_model(model, "band_gap", "random_forest", {"mae": 0.1})
        return registry

    def test_predict_from_formula(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = self._train_and_save_model(tmpdir)
            predictor = Predictor(registry=registry)
            result = predictor.predict("Si", property_name="band_gap", algorithm="random_forest")
            assert "prediction" in result
            assert "formula" in result
            assert result["formula"] == "Si"

    def test_predict_unknown_property(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ModelRegistry(models_dir=tmpdir)
            predictor = Predictor(registry=registry)
            result = predictor.predict("Si", property_name="nonexistent", algorithm="random_forest")
            assert "error" in result
