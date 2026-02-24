"""Tests for prism mcp commands."""
import pytest
from click.testing import CliRunner
from app.cli import cli


class TestMCPCommands:
    def test_mcp_help(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["mcp", "--help"])
        assert result.exit_code == 0
        assert "init" in result.output
        assert "status" in result.output

    def test_mcp_init(self, tmp_path, monkeypatch):
        monkeypatch.setattr("pathlib.Path.home", lambda: tmp_path)
        runner = CliRunner()
        result = runner.invoke(cli, ["mcp", "init"])
        assert result.exit_code == 0
        assert "Created" in result.output
        config_file = tmp_path / ".prism" / "mcp_servers.json"
        assert config_file.exists()

    def test_mcp_init_existing(self, tmp_path, monkeypatch):
        """If config already exists, don't overwrite."""
        monkeypatch.setattr("pathlib.Path.home", lambda: tmp_path)
        config_dir = tmp_path / ".prism"
        config_dir.mkdir(parents=True)
        (config_dir / "mcp_servers.json").write_text("{}")
        runner = CliRunner()
        result = runner.invoke(cli, ["mcp", "init"])
        assert result.exit_code == 0
        assert "already exists" in result.output

    def test_mcp_status(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["mcp", "status"])
        assert result.exit_code == 0
