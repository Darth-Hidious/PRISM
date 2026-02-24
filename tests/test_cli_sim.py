"""Tests for the `prism sim` CLI command group."""
import pytest
from unittest.mock import patch, MagicMock
from click.testing import CliRunner
from app.cli import cli


@pytest.fixture
def runner():
    return CliRunner()


class TestSimStatus:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=False)
    def test_no_pyiron(self, _m, runner):
        result = runner.invoke(cli, ["sim", "status"])
        assert result.exit_code == 0
        assert "not installed" in result.output

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_with_pyiron(self, mock_bridge_fn, _avail, runner):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.get_project().path = "/tmp/test_proj"
        bridge.load_hpc_config.return_value = None
        bridge.jobs.to_summary_list.return_value = []
        bridge.structures.to_summary_list.return_value = []

        result = runner.invoke(cli, ["sim", "status"])
        assert result.exit_code == 0
        assert "yes" in result.output


class TestSimJobs:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=False)
    def test_no_pyiron(self, _m, runner):
        result = runner.invoke(cli, ["sim", "jobs"])
        assert result.exit_code == 0
        assert "not installed" in result.output

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_empty_jobs(self, mock_bridge_fn, _avail, runner):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.jobs.to_summary_list.return_value = []

        result = runner.invoke(cli, ["sim", "jobs"])
        assert result.exit_code == 0
        assert "No simulation jobs" in result.output


class TestSimInit:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=False)
    def test_no_pyiron(self, _m, runner):
        result = runner.invoke(cli, ["sim", "init"])
        assert result.exit_code == 0
        assert "not installed" in result.output

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.PyironBridge")
    def test_init_success(self, MockBridge, _avail, runner):
        mock_bridge = MagicMock()
        MockBridge.return_value = mock_bridge
        mock_bridge.get_project().path = "/tmp/prism_default"

        result = runner.invoke(cli, ["sim", "init"])
        assert result.exit_code == 0
        assert "initialised" in result.output
