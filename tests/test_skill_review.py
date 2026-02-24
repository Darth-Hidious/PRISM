"""Tests for the review skill."""

from unittest.mock import patch

import pandas as pd
import pytest

from app.skills.review import REVIEW_SKILL, _review_dataset


class TestReviewSkill:
    def test_skill_metadata(self):
        assert REVIEW_SKILL.name == "review_dataset"
        assert REVIEW_SKILL.category == "review"
        tool = REVIEW_SKILL.to_tool()
        assert tool.name == "review_dataset"

    @patch("app.data.store.DataStore.load")
    def test_review_clean_data(self, mock_load):
        df = pd.DataFrame({
            "band_gap": [1.0, 2.0, 3.0],
            "formula": ["A", "B", "C"],
        })
        mock_load.return_value = df

        result = _review_dataset(dataset_name="clean")

        assert result["dataset_name"] == "clean"
        assert result["quality_score"] == 1.0
        assert result["severity_counts"]["critical"] == 0

    @patch("app.data.store.DataStore.load")
    def test_review_with_violations(self, mock_load):
        df = pd.DataFrame({
            "band_gap": [1.0, -0.5, 2.0],
            "formula": ["A", "B", "C"],
        })
        mock_load.return_value = df

        result = _review_dataset(dataset_name="bad")

        assert result["quality_score"] < 1.0
        assert result["severity_counts"]["critical"] >= 1
        assert len(result["findings"]) >= 1

    @patch("app.data.store.DataStore.load")
    def test_review_without_llm_prompt(self, mock_load):
        df = pd.DataFrame({"band_gap": [1.0, 2.0]})
        mock_load.return_value = df

        result = _review_dataset(dataset_name="test", include_llm_prompt=False)

        assert "review_prompt" not in result

    @patch("app.data.store.DataStore.load")
    def test_review_with_llm_prompt(self, mock_load):
        df = pd.DataFrame({"band_gap": [1.0, 2.0, -0.5]})
        mock_load.return_value = df

        result = _review_dataset(dataset_name="test", include_llm_prompt=True)

        assert "review_prompt" in result
        assert "Review this materials dataset" in result["review_prompt"]
        assert "constraint violations" in result["review_prompt"]

    def test_dataset_not_found(self):
        result = _review_dataset(dataset_name="nonexistent_12345")
        assert "error" in result
