"""Tests for model trainer and registry."""
import tempfile
import pytest
import numpy as np
from app.ml.trainer import train_model, AVAILABLE_ALGORITHMS
from app.ml.registry import ModelRegistry


class TestTrainer:
    def test_available_algorithms(self):
        assert "random_forest" in AVAILABLE_ALGORITHMS
        assert "xgboost" in AVAILABLE_ALGORITHMS or "gradient_boosting" in AVAILABLE_ALGORITHMS

    def test_train_random_forest(self):
        X = np.random.rand(50, 5)
        y = np.random.rand(50)
        result = train_model(X, y, algorithm="random_forest", property_name="test_prop")
        assert "metrics" in result
        assert "mae" in result["metrics"]
        assert result["metrics"]["mae"] >= 0

    def test_train_returns_model(self):
        X = np.random.rand(50, 5)
        y = np.random.rand(50)
        result = train_model(X, y, algorithm="random_forest", property_name="test")
        assert "model" in result


class TestModelRegistry:
    def test_save_and_load(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ModelRegistry(models_dir=tmpdir)

            from sklearn.ensemble import RandomForestRegressor
            model = RandomForestRegressor(n_estimators=5)
            X = np.random.rand(20, 3)
            y = np.random.rand(20)
            model.fit(X, y)

            registry.save_model(model, "band_gap", "random_forest", {"mae": 0.1})

            loaded = registry.load_model("band_gap", "random_forest")
            assert loaded is not None

    def test_list_models(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ModelRegistry(models_dir=tmpdir)
            models = registry.list_models()
            assert isinstance(models, list)
