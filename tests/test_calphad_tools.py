"""Tests for CALPHAD tools."""

from unittest.mock import MagicMock, patch

import pytest

from app.tools.base import ToolRegistry
from app.tools.calphad import (
    _calculate_equilibrium,
    _calculate_gibbs_energy,
    _calculate_phase_diagram,
    _import_database,
    _list_databases,
    _list_phases,
    create_calphad_tools,
)


class TestCreateCalphadTools:
    def test_registers_six_tools(self):
        registry = ToolRegistry()
        create_calphad_tools(registry)
        tools = registry.list_tools()
        assert len(tools) == 6
        names = {t.name for t in tools}
        assert "calculate_phase_diagram" in names
        assert "calculate_equilibrium" in names
        assert "calculate_gibbs_energy" in names
        assert "list_calphad_databases" in names
        assert "list_phases" in names
        assert "import_calphad_database" in names


class TestGuardedTools:
    """Calculation tools return error when pycalphad is missing."""

    def test_phase_diagram_guard(self):
        result = _calculate_phase_diagram(
            database_name="test", components=["Al", "Ni"]
        )
        assert "error" in result
        assert "pycalphad" in result["error"]

    def test_equilibrium_guard(self):
        result = _calculate_equilibrium(
            database_name="test",
            components=["Al", "Ni"],
            conditions={"T": 1000},
        )
        assert "error" in result

    def test_gibbs_energy_guard(self):
        result = _calculate_gibbs_energy(
            database_name="test",
            components=["Al", "Ni"],
            phases=["FCC_A1"],
            temperature=1000,
        )
        assert "error" in result

    def test_list_phases_guard(self):
        result = _list_phases(database_name="test")
        assert "error" in result


class TestUnguardedTools:
    """Database management tools work without pycalphad."""

    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_list_databases_works(self, mock_bridge_fn):
        mock_bridge = MagicMock()
        mock_bridge.databases.list_databases.return_value = []
        mock_bridge_fn.return_value = mock_bridge

        result = _list_databases()
        assert result["count"] == 0
        assert result["databases"] == []

    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_import_database_works(self, mock_bridge_fn, tmp_path):
        tdb_file = tmp_path / "test.tdb"
        tdb_file.write_text("$ test")

        mock_bridge = MagicMock()
        mock_bridge.databases.import_database.return_value = {
            "name": "test",
            "path": str(tdb_file),
            "imported": True,
        }
        mock_bridge_fn.return_value = mock_bridge

        result = _import_database(source_path=str(tdb_file))
        assert result["imported"] is True


class TestCalculateToolsWithMock:
    """Test calculate tools with mocked bridge."""

    @patch("app.tools.calphad._guard", return_value=None)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_phase_diagram(self, mock_bridge_fn, mock_guard):
        mock_bridge = MagicMock()
        mock_bridge.calculate_phase_diagram.return_value = {
            "database": "test",
            "components": ["Al", "Ni", "VA"],
            "phases": ["FCC_A1"],
            "n_points": 35,
            "data_points": [],
        }
        mock_bridge_fn.return_value = mock_bridge

        result = _calculate_phase_diagram(
            database_name="test", components=["Al", "Ni"]
        )
        assert result["n_points"] == 35
        assert result["database"] == "test"

    @patch("app.tools.calphad._guard", return_value=None)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_equilibrium(self, mock_bridge_fn, mock_guard):
        mock_bridge = MagicMock()
        mock_bridge.calculate_equilibrium.return_value = {
            "phases_present": ["FCC_A1"],
            "phase_fractions": {"FCC_A1": 1.0},
            "gibbs_energy": -50000.0,
            "database": "test",
            "components": ["Al", "Ni", "VA"],
        }
        mock_bridge_fn.return_value = mock_bridge

        result = _calculate_equilibrium(
            database_name="test",
            components=["Al", "Ni"],
            conditions={"T": 1000, "P": 101325, "X(AL)": 0.3},
        )
        assert "phases_present" in result

    @patch("app.tools.calphad._guard", return_value=None)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_gibbs_energy(self, mock_bridge_fn, mock_guard):
        mock_bridge = MagicMock()
        mock_bridge.calculate_gibbs_energy.return_value = {
            "phases": ["FCC_A1"],
            "temperature": 1000,
            "gibbs_energies": [-40000.0],
            "database": "test",
        }
        mock_bridge_fn.return_value = mock_bridge

        result = _calculate_gibbs_energy(
            database_name="test",
            components=["Al", "Ni"],
            phases=["FCC_A1"],
            temperature=1000,
        )
        assert result["temperature"] == 1000
        assert "gibbs_energies" in result

    @patch("app.tools.calphad._guard", return_value=None)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_list_phases(self, mock_bridge_fn, mock_guard):
        mock_bridge = MagicMock()
        mock_bridge.databases.get_phases.return_value = ["FCC_A1", "BCC_A2", "LIQUID"]
        mock_bridge_fn.return_value = mock_bridge

        result = _list_phases(database_name="test")
        assert result["count"] == 3
        assert "FCC_A1" in result["phases"]

    @patch("app.tools.calphad._guard", return_value=None)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_list_phases_not_found(self, mock_bridge_fn, mock_guard):
        mock_bridge = MagicMock()
        mock_bridge.databases.get_phases.return_value = None
        mock_bridge_fn.return_value = mock_bridge

        result = _list_phases(database_name="nonexistent")
        assert "error" in result
