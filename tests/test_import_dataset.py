"""Tests for import_dataset tool."""
import json
import pytest
from pathlib import Path
from unittest.mock import patch, MagicMock
from app.tools.data import _import_dataset

# _import_dataset imports DataStore inside, at "app.data.store.DataStore"
MOCK_STORE = "app.data.store.DataStore"


class TestImportDataset:
    def test_import_csv(self, tmp_path):
        csv_file = tmp_path / "test.csv"
        csv_file.write_text("name,value\nA,1\nB,2\n")

        with patch(MOCK_STORE) as MockStore:
            mock_store = MagicMock()
            MockStore.return_value = mock_store
            result = _import_dataset(file_path=str(csv_file))

        assert result["dataset_name"] == "test"
        assert result["rows"] == 2
        assert "name" in result["columns"]
        assert "value" in result["columns"]
        mock_store.save.assert_called_once()

    def test_import_json(self, tmp_path):
        json_file = tmp_path / "data.json"
        json_file.write_text(json.dumps([{"x": 1}, {"x": 2}]))

        with patch(MOCK_STORE) as MockStore:
            MockStore.return_value = MagicMock()
            result = _import_dataset(file_path=str(json_file))

        assert result["dataset_name"] == "data"
        assert result["rows"] == 2

    def test_import_with_custom_name(self, tmp_path):
        csv_file = tmp_path / "raw.csv"
        csv_file.write_text("col\n1\n")

        with patch(MOCK_STORE) as MockStore:
            MockStore.return_value = MagicMock()
            result = _import_dataset(
                file_path=str(csv_file), dataset_name="my_dataset"
            )

        assert result["dataset_name"] == "my_dataset"

    def test_import_missing_file(self):
        result = _import_dataset(file_path="/nonexistent/file.csv")
        assert "error" in result
        assert "not found" in result["error"].lower()

    def test_import_unsupported_format(self, tmp_path):
        bad_file = tmp_path / "data.xyz"
        bad_file.write_text("whatever")

        result = _import_dataset(file_path=str(bad_file))
        assert "error" in result
        assert "Unsupported format" in result["error"]

    def test_import_format_override(self, tmp_path):
        # File has .txt extension but we force csv
        txt_file = tmp_path / "data.txt"
        txt_file.write_text("a,b\n1,2\n")

        with patch(MOCK_STORE) as MockStore:
            MockStore.return_value = MagicMock()
            result = _import_dataset(
                file_path=str(txt_file), file_format="csv"
            )

        assert result["rows"] == 1
        assert result["columns"] == ["a", "b"]

    def test_import_parquet(self, tmp_path):
        try:
            import pandas as pd
            pq_file = tmp_path / "data.parquet"
            df = pd.DataFrame({"x": [1, 2, 3]})
            df.to_parquet(pq_file)

            with patch(MOCK_STORE) as MockStore:
                MockStore.return_value = MagicMock()
                result = _import_dataset(file_path=str(pq_file))

            assert result["rows"] == 3
            assert result["columns"] == ["x"]
        except ImportError:
            pytest.skip("pyarrow not installed")
