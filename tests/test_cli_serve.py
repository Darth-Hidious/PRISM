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

    def test_serve_help_shows_host_and_nginx(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["serve", "--help"])
        assert "--host" in result.output
        assert "--generate-nginx" in result.output

    def test_serve_generate_nginx_default(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["serve", "--generate-nginx"])
        assert result.exit_code == 0
        assert "upstream prism_mcp" in result.output
        assert "127.0.0.1:8000" in result.output
        assert "proxy_pass" in result.output
        assert "proxy_buffering off" in result.output

    def test_serve_generate_nginx_custom_host_port(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["serve", "--generate-nginx", "--host", "0.0.0.0", "--port", "9000"])
        assert result.exit_code == 0
        assert "0.0.0.0:9000" in result.output

    def test_serve_generate_nginx_has_streaming_support(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["serve", "--generate-nginx"])
        assert result.exit_code == 0
        assert 'proxy_set_header Connection ""' in result.output
        assert "proxy_read_timeout 300s" in result.output
        assert "/health" in result.output
