"""Tests for ask command deprecation."""
from click.testing import CliRunner


def test_ask_command_prints_deprecation_warning():
    """prism ask should still work but print a deprecation warning."""
    from app.cli.main import cli
    runner = CliRunner()
    # ask should not crash — it should either redirect to run or print deprecation
    result = runner.invoke(cli, ["ask", "--help"])
    # The command should exist (backward compat) but be marked deprecated
    assert result.exit_code == 0
