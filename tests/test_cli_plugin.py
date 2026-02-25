"""Tests for prism plugin CLI commands."""
import pytest
from click.testing import CliRunner
from unittest.mock import patch

from app.cli import cli


class TestPluginList:
    def test_list_no_plugins(self):
        with patch("app.plugins.loader.discover_entry_point_plugins", return_value=[]):
            with patch("app.plugins.loader.discover_local_plugins", return_value=[]):
                runner = CliRunner()
                result = runner.invoke(cli, ["plugin", "list"])
                assert result.exit_code == 0
                assert "No plugins found" in result.output

    def test_list_with_entry_point_plugins(self):
        with patch("app.plugins.loader.discover_entry_point_plugins", return_value=["my_ep_plugin"]):
            with patch("app.plugins.loader.discover_local_plugins", return_value=[]):
                runner = CliRunner()
                result = runner.invoke(cli, ["plugin", "list"])
                assert result.exit_code == 0
                assert "my_ep_plugin" in result.output

    def test_list_with_local_plugins(self):
        with patch("app.plugins.loader.discover_entry_point_plugins", return_value=[]):
            with patch("app.plugins.loader.discover_local_plugins", return_value=["local_one"]):
                runner = CliRunner()
                result = runner.invoke(cli, ["plugin", "list"])
                assert result.exit_code == 0
                assert "local_one" in result.output


class TestPluginInit:
    def test_init_creates_template(self, tmp_path):
        with patch("app.cli.main.Path") as MockPath:
            # Make Path.home() return our tmp_path
            MockPath.home.return_value = tmp_path
            plugin_dir = tmp_path / ".prism" / "plugins"
            plugin_dir.mkdir(parents=True)

            # The real Path is used elsewhere, so just test the CLI output
            runner = CliRunner()
            # Actually, let's test with real filesystem
        # Use real filesystem test
        pass

    def test_init_creates_file(self, tmp_path, monkeypatch):
        monkeypatch.setattr("app.cli.main.Path.home", lambda: tmp_path)

        runner = CliRunner()
        result = runner.invoke(cli, ["plugin", "init", "test_plugin"])
        assert result.exit_code == 0
        assert "Created plugin template" in result.output

        plugin_file = tmp_path / ".prism" / "plugins" / "test_plugin.py"
        assert plugin_file.exists()
        content = plugin_file.read_text()
        assert "def register(registry)" in content

    def test_init_existing_plugin(self, tmp_path, monkeypatch):
        monkeypatch.setattr("app.cli.main.Path.home", lambda: tmp_path)

        plugin_dir = tmp_path / ".prism" / "plugins"
        plugin_dir.mkdir(parents=True)
        (plugin_dir / "existing.py").write_text("# already here")

        runner = CliRunner()
        result = runner.invoke(cli, ["plugin", "init", "existing"])
        assert result.exit_code == 0
        assert "already exists" in result.output
