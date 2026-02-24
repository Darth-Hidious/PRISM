"""Tests for prism data import CLI command."""
import pytest
from click.testing import CliRunner
from unittest.mock import patch, MagicMock

from app.commands.data import data


class TestDataImportCLI:
    def test_import_csv(self, tmp_path):
        csv_file = tmp_path / "sample.csv"
        csv_file.write_text("a,b\n1,2\n3,4\n")

        with patch("app.tools.data._import_dataset") as mock_import:
            mock_import.return_value = {
                "dataset_name": "sample",
                "rows": 2,
                "columns": ["a", "b"],
                "source": str(csv_file),
            }
            runner = CliRunner()
            result = runner.invoke(data, ["import", str(csv_file)])
            assert result.exit_code == 0
            assert "Imported 2 rows" in result.output
            assert "sample" in result.output

    def test_import_with_name(self, tmp_path):
        csv_file = tmp_path / "raw.csv"
        csv_file.write_text("x\n1\n")

        with patch("app.tools.data._import_dataset") as mock_import:
            mock_import.return_value = {
                "dataset_name": "custom",
                "rows": 1,
                "columns": ["x"],
                "source": str(csv_file),
            }
            runner = CliRunner()
            result = runner.invoke(
                data, ["import", str(csv_file), "--name", "custom"]
            )
            assert result.exit_code == 0
            assert "custom" in result.output

    def test_import_error(self, tmp_path):
        csv_file = tmp_path / "bad.csv"
        csv_file.write_text("")

        with patch("app.tools.data._import_dataset") as mock_import:
            mock_import.return_value = {"error": "Failed to read file"}
            runner = CliRunner()
            result = runner.invoke(data, ["import", str(csv_file)])
            assert "Failed to read file" in result.output

    def test_import_nonexistent_file(self):
        runner = CliRunner()
        result = runner.invoke(data, ["import", "/no/such/file.csv"])
        # Click's exists=True check should catch this
        assert result.exit_code != 0
