"""Tests for ML visualization."""
import pytest
import numpy as np
from app.ml.viz import plot_parity, plot_feature_importance


class TestMLViz:
    def test_parity_plot(self, tmp_path):
        y_true = np.array([1.0, 2.0, 3.0, 4.0])
        y_pred = np.array([1.1, 2.2, 2.8, 4.1])
        result = plot_parity(y_true, y_pred, "band_gap", str(tmp_path / "parity.png"))
        assert result.get("success") is True

    def test_feature_importance(self, tmp_path):
        names = ["feat_a", "feat_b", "feat_c"]
        importances = [0.5, 0.3, 0.2]
        result = plot_feature_importance(names, importances, str(tmp_path / "imp.png"))
        assert result.get("success") is True
