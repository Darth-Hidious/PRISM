"""Tests for prism serve command."""
import pytest
from click.testing import CliRunner
from app.cli import cli


class TestServeCommand:
    def test_serve_help(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["serve", "--help"])
        assert result.exit_code == 0
        assert "transport" in result.output.lower()
        assert "stdio" in result.output.lower()

    def test_serve_command_exists(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["serve", "--help"])
        assert result.exit_code == 0
        assert "MCP server" in result.output

    def test_serve_install_flag(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["serve", "--install"])
        assert result.exit_code == 0
        assert "mcpServers" in result.output
        assert "prism" in result.output
