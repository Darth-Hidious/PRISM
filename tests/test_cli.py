"""Tests for CLI commands."""
import pytest
from click.testing import CliRunner
from unittest.mock import patch, MagicMock
from app.cli import cli
from app.agent.events import TextDelta, TurnComplete


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

    @patch("app.agent.factory.create_backend")
    @patch("app.agent.autonomous.run_autonomous_stream")
    def test_run_command(self, mock_stream, mock_backend):
        mock_stream.return_value = iter([TextDelta(text="Silicon has band gap 1.1 eV"), TurnComplete(text="Silicon has band gap 1.1 eV")])
        runner = CliRunner()
        result = runner.invoke(cli, ["run", "What is silicon's band gap?"])
        assert result.exit_code == 0

    def test_resume_flag_exists(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["--help"])
        assert "--resume" in result.output

    @patch("app.cli.main.create_backend")
    @patch("app.cli.main.AgentREPL")
    def test_resume_not_found(self, mock_repl_cls, mock_backend):
        mock_repl = mock_repl_cls.return_value
        mock_repl._load_session.side_effect = FileNotFoundError("not found")
        runner = CliRunner()
        result = runner.invoke(cli, ["--resume", "fake-session-id"])
        assert "not found" in result.output.lower() or "Session not found" in result.output
