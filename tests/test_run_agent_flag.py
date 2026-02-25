"""Tests for --agent flag on run command."""
from unittest.mock import patch, MagicMock
from click.testing import CliRunner


def test_run_help_shows_agent_flag():
    from app.cli.main import cli
    runner = CliRunner()
    result = runner.invoke(cli, ["run", "--help"])
    assert "--agent" in result.output


def test_run_with_unknown_agent_errors():
    from app.cli.main import cli
    from app.agent.agent_registry import AgentRegistry
    runner = CliRunner()
    # Mock build_full_registry to return an empty agent registry
    empty_agent_reg = AgentRegistry()
    mock_tool_reg = MagicMock()
    mock_provider_reg = MagicMock()
    with patch("app.plugins.bootstrap.build_full_registry",
               return_value=(mock_tool_reg, mock_provider_reg, empty_agent_reg)):
        result = runner.invoke(cli, ["run", "--agent", "nonexistent", "test goal"])
        assert "Unknown agent" in result.output or result.exit_code != 0
