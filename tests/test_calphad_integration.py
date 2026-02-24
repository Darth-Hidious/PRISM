"""Integration tests for CALPHAD — end-to-end flows with mocked pycalphad."""

import sys
import types
from pathlib import Path
from unittest.mock import MagicMock, patch

import numpy as np
import pytest

from app.simulation.calphad_bridge import (
    CalphadBridge,
    DatabaseStore,
    check_calphad_available,
)


class TestCalphadAvailabilityIntegration:
    def test_check_returns_false(self):
        """pycalphad is not installed in test environment."""
        assert check_calphad_available() is False


class TestBootstrapWithoutPycalphad:
    def test_build_full_registry_works(self):
        """build_full_registry() works without pycalphad (graceful skip)."""
        from app.plugins.bootstrap import build_full_registry

        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        # CALPHAD tools should NOT be registered
        assert "calculate_phase_diagram" not in names
        assert "calculate_equilibrium" not in names
        # Skills should still be registered
        assert "analyze_phases" in names
        assert "plan_simulations" in names


class TestDatabaseStoreLifecycle:
    def test_import_list_load(self, tmp_path):
        """Import → List → Load lifecycle with mocked pycalphad."""
        src_dir = tmp_path / "source"
        src_dir.mkdir()
        src_file = src_dir / "sgte.tdb"
        src_file.write_text("$ SGTE pure elements database\nELEMENT AL FCC_A1 26.9815 0 0\n")

        db_dir = tmp_path / "databases"
        store = DatabaseStore(base_dir=db_dir)

        # Step 1: Import
        result = store.import_database(str(src_file))
        assert result["imported"] is True
        assert result["name"] == "sgte"

        # Step 2: List
        databases = store.list_databases()
        assert len(databases) == 1
        assert databases[0]["name"] == "sgte"
        assert databases[0]["size_kb"] > 0

        # Step 3: Load (with mocked pycalphad)
        mock_db = MagicMock()
        mock_pycalphad = types.ModuleType("pycalphad")
        mock_pycalphad.Database = MagicMock(return_value=mock_db)

        with patch.dict(sys.modules, {"pycalphad": mock_pycalphad}):
            loaded = store.load("sgte")
            assert loaded is mock_db


class TestCalphadToolsWithMockedBridge:
    """Test tools return correct structure with mocked bridge."""

    @patch("app.tools.calphad._guard", return_value=None)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_phase_diagram_structure(self, mock_bridge_fn, mock_guard):
        mock_bridge = MagicMock()
        mock_bridge.calculate_phase_diagram.return_value = {
            "database": "sgte",
            "components": ["W", "Rh", "VA"],
            "phases": ["FCC_A1", "BCC_A2", "HCP_A3"],
            "n_points": 35,
            "data_points": [{"temperature": 300, "phases_present": ["BCC_A2"]}],
        }
        mock_bridge_fn.return_value = mock_bridge

        from app.tools.calphad import _calculate_phase_diagram

        result = _calculate_phase_diagram(
            database_name="sgte", components=["W", "Rh"]
        )
        assert result["database"] == "sgte"
        assert result["n_points"] == 35
        assert len(result["data_points"]) == 1

    @patch("app.tools.calphad._guard", return_value=None)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_equilibrium_structure(self, mock_bridge_fn, mock_guard):
        mock_bridge = MagicMock()
        mock_bridge.calculate_equilibrium.return_value = {
            "phases_present": ["BCC_A2"],
            "phase_fractions": {"BCC_A2": 1.0},
            "gibbs_energy": -45000.0,
            "database": "sgte",
            "components": ["W", "Rh", "VA"],
        }
        mock_bridge_fn.return_value = mock_bridge

        from app.tools.calphad import _calculate_equilibrium

        result = _calculate_equilibrium(
            database_name="sgte",
            components=["W", "Rh"],
            conditions={"T": 1500, "P": 101325, "X(W)": 0.5},
        )
        assert "phases_present" in result
        assert "phase_fractions" in result
        assert result["gibbs_energy"] < 0


class TestAnalyzePhasesIntegration:
    @patch("app.simulation.calphad_bridge.check_calphad_available", return_value=True)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_end_to_end(self, mock_bridge_fn, mock_available):
        mock_bridge = MagicMock()
        mock_bridge.databases.get_phases.return_value = [
            "BCC_A2", "FCC_A1", "SIGMA", "LIQUID"
        ]
        mock_bridge.calculate_equilibrium.return_value = {
            "phases_present": ["BCC_A2", "SIGMA"],
            "phase_fractions": {"BCC_A2": 0.7, "SIGMA": 0.3},
            "gibbs_energy": -55000.0,
        }
        mock_bridge.calculate_phase_diagram.return_value = {
            "n_points": 18,
            "data_points": [],
        }
        mock_bridge_fn.return_value = mock_bridge

        from app.skills.phase_analysis import _analyze_phases

        result = _analyze_phases(
            database_name="sgte",
            components=["W", "Rh"],
            temperature=1500,
            composition={"X(W)": 0.5},
        )

        assert result["database"] == "sgte"
        assert result["components"] == ["W", "Rh"]
        assert result["available_phases"] == 4
        assert "BCC_A2" in result["stable_phases"]
        assert "SIGMA" in result["stable_phases"]
        assert result["phase_fractions"]["BCC_A2"] == 0.7


class TestSimulationPlanRouting:
    @patch("app.data.store.DataStore.load")
    def test_phase_stability_routes_to_calphad(self, mock_load):
        import pandas as pd
        from app.config.preferences import UserPreferences

        mock_load.return_value = pd.DataFrame({"formula": ["W2Rh"]})

        with patch(
            "app.skills.simulation_plan.UserPreferences.load",
            return_value=UserPreferences(compute_budget="local"),
        ):
            from app.skills.simulation_plan import _plan_simulations

            result = _plan_simulations(
                dataset_name="test",
                simulation_types=["phase_stability"],
            )
            assert result["jobs"][0]["method"] == "calphad"
            assert "code" not in result["jobs"][0]

    @patch("app.data.store.DataStore.load")
    def test_dft_backward_compatible(self, mock_load):
        import pandas as pd
        from app.config.preferences import UserPreferences

        mock_load.return_value = pd.DataFrame({"formula": ["W2Rh"]})

        with patch(
            "app.skills.simulation_plan.UserPreferences.load",
            return_value=UserPreferences(compute_budget="local"),
        ):
            from app.skills.simulation_plan import _plan_simulations

            result = _plan_simulations(
                dataset_name="test",
                simulation_types=["energy_minimization"],
            )
            assert result["jobs"][0]["method"] == "dft"
            assert result["jobs"][0]["code"] == "lammps"


class TestThermocalcPlugin:
    def test_register_skips_without_tcpython(self):
        """ThermoCalc plugin registers nothing when TC-Python is not installed."""
        from app.plugins.thermocalc import register

        mock_registry = MagicMock()
        register(mock_registry)
        # Should not have registered any tools
        mock_registry.tool_registry.register.assert_not_called()
