"""Tests for the correlation matrix visualization tool."""

from unittest.mock import patch

import pandas as pd
import pytest

from app.tools.visualization import _plot_correlation_matrix


class TestPlotCorrelationMatrix:
    @patch("app.data.store.DataStore.load")
    def test_generates_plot(self, mock_load, tmp_path):
        df = pd.DataFrame({
            "band_gap": [1.0, 2.0, 3.0, 4.0, 5.0],
            "density": [5.0, 4.0, 3.0, 2.0, 1.0],
            "volume": [10.0, 20.0, 30.0, 40.0, 50.0],
        })
        mock_load.return_value = df
        out = str(tmp_path / "corr.png")

        result = _plot_correlation_matrix(dataset_name="test", output_path=out)

        assert result["success"] is True
        assert result["n_properties"] == 3
        assert (tmp_path / "corr.png").exists()

    @patch("app.data.store.DataStore.load")
    def test_insufficient_columns(self, mock_load):
        df = pd.DataFrame({
            "formula": ["A", "B", "C"],
            "band_gap": [1.0, 2.0, 3.0],
        })
        mock_load.return_value = df

        result = _plot_correlation_matrix(dataset_name="test")

        assert "error" in result
        assert "at least 2" in result["error"]

    @patch("app.data.store.DataStore.load")
    def test_returns_top_correlations(self, mock_load, tmp_path):
        df = pd.DataFrame({
            "a": [1.0, 2.0, 3.0, 4.0],
            "b": [2.0, 4.0, 6.0, 8.0],  # perfectly correlated with a
            "c": [10.0, 5.0, 8.0, 3.0],
        })
        mock_load.return_value = df
        out = str(tmp_path / "corr.png")

        result = _plot_correlation_matrix(dataset_name="test", output_path=out)

        assert len(result["top_correlations"]) > 0
        # a and b should be most correlated
        top = result["top_correlations"][0]
        assert abs(top["correlation"]) > 0.9

    @patch("app.data.store.DataStore.load")
    def test_with_column_filter(self, mock_load, tmp_path):
        df = pd.DataFrame({
            "a": [1.0, 2.0, 3.0],
            "b": [4.0, 5.0, 6.0],
            "c": [7.0, 8.0, 9.0],
        })
        mock_load.return_value = df
        out = str(tmp_path / "corr.png")

        result = _plot_correlation_matrix(dataset_name="test", columns=["a", "b"], output_path=out)

        assert result["n_properties"] == 2
