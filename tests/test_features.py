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
        features = composition_features("")
        assert isinstance(features, dict)
        assert len(features) == 0

    def test_unknown_elements_partial_features(self):
        # Elements not in ELEMENT_DATA still get basic count features
        features = composition_features("InvalidXyz123")
        assert isinstance(features, dict)
        assert "n_elements" in features
        # No property stats since elements not in lookup table
        assert "avg_atomic_mass" not in features
