"""Tests for the validation skill."""

from unittest.mock import patch

import pandas as pd
import pytest

from app.skills.validation import VALIDATE_SKILL, _validate_dataset


class TestValidateSkill:
    def test_skill_metadata(self):
        assert VALIDATE_SKILL.name == "validate_dataset"
        assert VALIDATE_SKILL.category == "validation"
        tool = VALIDATE_SKILL.to_tool()
        assert tool.name == "validate_dataset"

    def test_required_fields_in_schema(self):
        schema = VALIDATE_SKILL.input_schema
        assert "dataset_name" in schema["properties"]
        assert "dataset_name" in schema["required"]

    @patch("app.data.store.DataStore.load")
    def test_validate_with_mock_datastore(self, mock_load):
        df = pd.DataFrame({
            "band_gap": [1.0, 2.0, -0.5],
            "formula": ["A", "B", "C"],
        })
        mock_load.return_value = df

        result = _validate_dataset(dataset_name="test_data")

        assert result["dataset_name"] == "test_data"
        assert result["total_findings"] >= 1  # negative band_gap
        assert len(result["constraint_violations"]) >= 1
        assert "summary" in result

    def test_dataset_not_found(self):
        result = _validate_dataset(dataset_name="nonexistent_12345")
        assert "error" in result

    @patch("app.data.store.DataStore.load")
    def test_end_to_end_clean_data(self, mock_load):
        df = pd.DataFrame({
            "band_gap": [1.0, 2.0, 3.0],
            "formation_energy_per_atom": [-0.5, -0.3, -0.1],
            "formula": ["A", "B", "C"],
        })
        mock_load.return_value = df

        result = _validate_dataset(dataset_name="clean")

        assert result["total_findings"] == 0
        assert "clean" in result["summary"].lower() or "no issues" in result["summary"].lower()
