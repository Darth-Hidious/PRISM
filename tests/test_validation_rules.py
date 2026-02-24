"""Tests for rule-based validation functions."""

import pandas as pd
import numpy as np

from app.validation.rules import (
    detect_outliers,
    check_physical_constraints,
    score_completeness,
    validate_dataset,
)


class TestDetectOutliers:
    def test_no_outliers_normal_data(self):
        df = pd.DataFrame({"band_gap": [1.0, 1.1, 0.9, 1.05, 0.95]})
        findings = detect_outliers(df)
        assert findings == []

    def test_extreme_value_flagged(self):
        # Need enough normal points so the extreme doesn't inflate std too much
        df = pd.DataFrame({"band_gap": [1.0, 1.1, 0.9, 1.05, 0.95, 1.0, 1.02, 0.98, 1.03, 0.97, 100.0]})
        findings = detect_outliers(df)
        assert len(findings) >= 1
        assert findings[0]["type"] == "outlier"
        assert findings[0]["column"] == "band_gap"
        assert findings[0]["value"] == 100.0

    def test_custom_threshold(self):
        df = pd.DataFrame({"val": [1.0, 1.1, 0.9, 1.05, 0.95, 3.0]})
        # With high threshold, 3.0 may not be flagged
        strict = detect_outliers(df, z_threshold=1.5)
        lenient = detect_outliers(df, z_threshold=5.0)
        assert len(strict) >= len(lenient)

    def test_specific_columns(self):
        df = pd.DataFrame({"a": [1, 2, 3, 100], "b": [1, 2, 3, 100]})
        findings = detect_outliers(df, columns=["a"])
        cols = {f["column"] for f in findings}
        assert "b" not in cols


class TestCheckPhysicalConstraints:
    def test_clean_data_no_violations(self):
        df = pd.DataFrame({
            "band_gap": [0.0, 1.0, 2.5],
            "formation_energy_per_atom": [-1.0, 0.0, 1.0],
            "density": [1.0, 5.0, 10.0],
            "volume": [10.0, 20.0, 30.0],
        })
        findings = check_physical_constraints(df)
        assert findings == []

    def test_negative_band_gap(self):
        df = pd.DataFrame({"band_gap": [1.0, -0.5, 2.0]})
        findings = check_physical_constraints(df)
        assert len(findings) == 1
        assert findings[0]["type"] == "constraint_violation"
        assert findings[0]["value"] == -0.5
        assert "band_gap >= 0" in findings[0]["constraint"]

    def test_extreme_formation_energy(self):
        df = pd.DataFrame({"formation_energy_per_atom": [-1.0, -15.0, 0.5]})
        findings = check_physical_constraints(df)
        assert len(findings) == 1
        assert findings[0]["value"] == -15.0

    def test_negative_density(self):
        df = pd.DataFrame({"density": [5.0, -1.0, 10.0]})
        findings = check_physical_constraints(df)
        assert len(findings) == 1
        assert findings[0]["value"] == -1.0

    def test_missing_columns_ignored(self):
        df = pd.DataFrame({"formula": ["Fe2O3", "Al2O3"]})
        findings = check_physical_constraints(df)
        assert findings == []


class TestScoreCompleteness:
    def test_complete_dataset(self):
        df = pd.DataFrame({"a": [1, 2, 3], "b": [4, 5, 6]})
        result = score_completeness(df)
        assert result["overall_completeness"] == 1.0
        assert result["total_rows"] == 3
        assert result["columns_below_50pct"] == []

    def test_with_nulls(self):
        df = pd.DataFrame({"a": [1, None, 3], "b": [4, 5, 6]})
        result = score_completeness(df)
        assert result["overall_completeness"] < 1.0
        assert result["column_completeness"]["b"] == 1.0
        assert result["column_completeness"]["a"] < 1.0

    def test_empty_dataframe(self):
        df = pd.DataFrame({"a": pd.Series([], dtype=float), "b": pd.Series([], dtype=float)})
        result = score_completeness(df)
        assert result["overall_completeness"] == 0.0
        assert result["total_rows"] == 0


class TestValidateDataset:
    def test_aggregation(self):
        df = pd.DataFrame({
            "band_gap": [1.0, -0.5, 2.0, 100.0],
            "formula": ["A", "B", "C", "D"],
        })
        result = validate_dataset(df)
        assert "outliers" in result
        assert "constraint_violations" in result
        assert "completeness" in result
        assert result["total_findings"] == len(result["outliers"]) + len(result["constraint_violations"])

    def test_clean_data_zero_findings(self):
        df = pd.DataFrame({
            "band_gap": [1.0, 1.1, 0.9],
            "formation_energy_per_atom": [-0.5, -0.3, -0.1],
        })
        result = validate_dataset(df)
        assert result["total_findings"] == 0
