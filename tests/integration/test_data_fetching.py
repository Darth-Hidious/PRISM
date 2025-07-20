import pytest
from app.cli import fetch_material

from click.testing import CliRunner
from app.cli import cli

def test_fetch_from_jarvis():
    """Test fetching data from JARVIS."""
    runner = CliRunner()
    # Test with a common material
    result = runner.invoke(cli, ["fetch-material", "--source", "jarvis", "--formula", "NaCl"])
    assert result.exit_code == 0
    assert "NaCl" in result.output

    # Test with a more complex material
    result = runner.invoke(cli, ["fetch-material", "--source", "jarvis", "--formula", "YBa2Cu3O7"])
    assert result.exit_code == 0
    assert "YBa2Cu3O7" in result.output


def test_fetch_from_nomad():
    """Test fetching data from NOMAD."""
    runner = CliRunner()
    # Test with a common material
    result = runner.invoke(cli, ["fetch-material", "--source", "nomad", "--formula", "NaCl"])
    assert result.exit_code == 0
    assert "NaCl" in result.output

    # Test with a more complex material
    result = runner.invoke(cli, ["fetch-material", "--source", "nomad", "--formula", "YBa2Cu3O7"])
    assert result.exit_code == 0
    assert "YBa2Cu3O7" in result.output
