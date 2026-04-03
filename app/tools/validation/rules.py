"""Rule-based data validation for materials datasets."""

from __future__ import annotations

import pandas as pd


def detect_outliers(
    df: pd.DataFrame,
    columns: list[str] | None = None,
    z_threshold: float = 3.0,
) -> list[dict]:
    """Flag rows where |z-score| > threshold for numeric columns."""
    numeric = df.select_dtypes(include=["float64", "float32", "int64", "int32"])
    if columns:
        numeric = numeric[[c for c in columns if c in numeric.columns]]

    findings: list[dict] = []
    for col in numeric.columns:
        series = numeric[col].dropna()
        if len(series) < 2:
            continue
        mean = series.mean()
        std = series.std()
        if std == 0:
            continue
        for idx in series.index:
            val = series[idx]
            z = abs((val - mean) / std)
            if z > z_threshold:
                findings.append({
                    "type": "outlier",
                    "column": col,
                    "row": int(idx),
                    "value": float(val),
                    "z_score": round(float(z), 2),
                })
    return findings


# Physical constraints for materials science columns
_CONSTRAINTS: list[dict] = [
    {"column": "band_gap", "op": ">=", "bound": 0, "label": "band_gap >= 0"},
    {"column": "formation_energy_per_atom", "op": ">=", "bound": -10, "label": "formation_energy_per_atom >= -10 eV"},
    {"column": "formation_energy_per_atom", "op": "<=", "bound": 5, "label": "formation_energy_per_atom <= 5 eV"},
    {"column": "density", "op": ">", "bound": 0, "label": "density > 0"},
    {"column": "volume", "op": ">", "bound": 0, "label": "volume > 0"},
]


def check_physical_constraints(df: pd.DataFrame) -> list[dict]:
    """Check materials science constraints on known columns."""
    findings: list[dict] = []
    for rule in _CONSTRAINTS:
        col = rule["column"]
        if col not in df.columns:
            continue
        series = df[col].dropna()
        for idx in series.index:
            val = series[idx]
            violated = False
            if rule["op"] == ">=" and val < rule["bound"]:
                violated = True
            elif rule["op"] == "<=" and val > rule["bound"]:
                violated = True
            elif rule["op"] == ">" and val <= rule["bound"]:
                violated = True
            if violated:
                findings.append({
                    "type": "constraint_violation",
                    "column": col,
                    "row": int(idx),
                    "value": float(val),
                    "constraint": rule["label"],
                })
    return findings


def score_completeness(df: pd.DataFrame) -> dict:
    """Score dataset completeness: % non-null per column, overall score."""
    if len(df) == 0:
        return {
            "overall_completeness": 0.0,
            "column_completeness": {},
            "total_rows": 0,
            "columns_below_50pct": list(df.columns),
        }

    col_scores: dict[str, float] = {}
    for col in df.columns:
        col_scores[col] = round(float(df[col].notna().sum() / len(df)), 4)

    overall = round(float(sum(col_scores.values()) / len(col_scores)), 4) if col_scores else 0.0
    below_50 = [c for c, s in col_scores.items() if s < 0.5]

    return {
        "overall_completeness": overall,
        "column_completeness": col_scores,
        "total_rows": len(df),
        "columns_below_50pct": below_50,
    }


def validate_dataset(df: pd.DataFrame, z_threshold: float = 3.0) -> dict:
    """Run all validations and return combined results."""
    outliers = detect_outliers(df, z_threshold=z_threshold)
    violations = check_physical_constraints(df)
    completeness = score_completeness(df)
    return {
        "outliers": outliers,
        "constraint_violations": violations,
        "completeness": completeness,
        "total_findings": len(outliers) + len(violations),
    }
