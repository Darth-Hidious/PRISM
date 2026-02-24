"""Tests for the property selection tool."""

from unittest.mock import patch

import pandas as pd
import pytest

from app.tools.property_selection import _list_predictable_properties


class TestListPredictableProperties:
    @patch("app.data.store.DataStore.load")
    def test_with_numeric_columns(self, mock_load):
        df = pd.DataFrame({
            "formula": ["Fe2O3", "Al2O3", "SiO2"],
            "band_gap": [2.0, 8.8, 9.0],
            "density": [5.2, 3.9, 2.6],
        })
        mock_load.return_value = df

        result = _list_predictable_properties(dataset_name="test_data")

        assert result["dataset_name"] == "test_data"
        assert result["total_rows"] == 3
        assert result["has_formula_column"] is True
        props = {p["property"] for p in result["predictable_properties"]}
        assert "band_gap" in props
        assert "density" in props

    @patch("app.data.store.DataStore.load")
    def test_no_numeric_columns(self, mock_load):
        df = pd.DataFrame({
            "formula": ["Fe2O3", "Al2O3"],
            "source_id": ["a", "b"],
        })
        mock_load.return_value = df

        result = _list_predictable_properties(dataset_name="test_data")

        assert result["predictable_properties"] == []

    @patch("app.data.store.DataStore.load")
    def test_excludes_metadata_columns(self, mock_load):
        df = pd.DataFrame({
            "formula": ["Fe2O3"],
            "band_gap": [2.0],
            "source_id": ["a"],
            "material_id": ["mp-123"],
        })
        mock_load.return_value = df

        result = _list_predictable_properties(dataset_name="test_data")

        props = {p["property"] for p in result["predictable_properties"]}
        assert "band_gap" in props
        assert "source_id" not in props
        assert "material_id" not in props

    @patch("app.data.store.DataStore.load")
    def test_shows_already_predicted(self, mock_load):
        df = pd.DataFrame({
            "formula": ["Fe2O3"],
            "band_gap": [2.0],
            "predicted_band_gap": [2.1],
        })
        mock_load.return_value = df

        result = _list_predictable_properties(dataset_name="test_data")

        assert "band_gap" in result["already_predicted"]
        # predicted_ columns should not appear in predictable list
        props = {p["property"] for p in result["predictable_properties"]}
        assert "predicted_band_gap" not in props

    def test_dataset_not_found(self):
        result = _list_predictable_properties(dataset_name="nonexistent_12345")
        assert "error" in result
