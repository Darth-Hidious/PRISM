"""Tests for CLI commands."""
import pytest
from click.testing import CliRunner
from unittest.mock import patch, MagicMock
from app.cli import cli


class TestCLI:
    def test_help(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["--help"])
        assert result.exit_code == 0
        assert "PRISM" in result.output or "prism" in result.output

    def test_version(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["--version"])
        assert result.exit_code == 0

    def test_run_command_exists(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["run", "--help"])
        assert result.exit_code == 0
        assert "goal" in result.output.lower() or "GOAL" in result.output

    @patch("app.cli.create_backend")
    @patch("app.cli.run_autonomous")
    def test_run_command(self, mock_run, mock_backend):
        mock_run.return_value = "Silicon has band gap 1.1 eV"
        runner = CliRunner()
        result = runner.invoke(cli, ["run", "What is silicon's band gap?"])
        assert result.exit_code == 0
