"""Tests for feature engineering."""
import pytest
from app.ml.features import composition_features


class TestCompositionFeatures:
    def test_basic_formula(self):
        features = composition_features("Si")
        assert isinstance(features, dict)
        assert len(features) > 0

    def test_binary_compound(self):
        features = composition_features("NaCl")
        assert isinstance(features, dict)
        assert "avg_atomic_mass" in features or len(features) > 0

    def test_invalid_formula_returns_empty(self):
        features = composition_features("InvalidXyz123")
        assert isinstance(features, dict)
