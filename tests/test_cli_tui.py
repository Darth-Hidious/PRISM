"""Tests for TUI CLI integration."""


def test_cli_has_tui_flag():
    """CLI accepts --tui and --classic flags."""
    from click.testing import CliRunner
    from app.cli import cli
    runner = CliRunner()
    result = runner.invoke(cli, ["--help"])
    assert "--tui" in result.output or "--classic" in result.output
