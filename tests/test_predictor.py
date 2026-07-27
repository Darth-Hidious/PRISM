"""Tests for predictor module."""
import tempfile
import importlib.util

import pytest
import numpy as np
from app.tools.ml.predictor import Predictor
from app.tools.ml.registry import ModelRegistry

#: scikit-learn ships in the `[ml]` extra, not in a default provision, so a
#: normally-installed box has to say "skipped, and here is why" rather than
#: fail. A red suite nobody can run teaches everyone to ignore the suite.
requires_sklearn = pytest.mark.skipif(
    importlib.util.find_spec("sklearn") is None,
    reason="needs scikit-learn from the `[ml]` extra: "
    "pip install 'prism-platform[ml]'",
)


class TestPredictor:
    def _train_and_save_model(self, tmpdir):
        from sklearn.ensemble import RandomForestRegressor
        from app.tools.ml.features import composition_features
        # Train with same feature count as composition_features produces
        n_features = len(composition_features("Si"))
        model = RandomForestRegressor(n_estimators=5, random_state=42)
        X = np.random.rand(30, n_features)
        y = np.random.rand(30)
        model.fit(X, y)

        registry = ModelRegistry(models_dir=tmpdir)
        registry.save_model(model, "band_gap", "random_forest", {"mae": 0.1})
        return registry

    @requires_sklearn
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
