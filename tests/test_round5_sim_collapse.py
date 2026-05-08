"""Tests for the Round 5 sim_tools + calphad collapses.

These tests use mocked underlying impls so they can run on any Python
environment (including Python 3.14 where pyiron_atomistics + pyiron_base
0.9+ aren't yet supported). The actual pyiron path is exercised by
end-to-end smoke tests when running in a pyiron-capable env.
"""
from unittest.mock import patch, MagicMock

import pytest

from app.tools.base import ToolRegistry


# ---------------------------------------------------------------------------
# Helpers — bypass the pyiron-availability check so the dispatchers register
# even on environments without pyiron installed (e.g. Python 3.14).
# ---------------------------------------------------------------------------

@pytest.fixture
def sim_registry():
    """Force pyiron_available=True so create_simulation_tools registers
    the unified dispatchers, then patch the bridge layer so dispatcher
    calls go through cleanly without touching pyiron."""
    with patch("app.tools.simulation.bridge.check_pyiron_available", return_value=True):
        from app.tools.sim_tools import create_simulation_tools
        reg = ToolRegistry()
        create_simulation_tools(reg)
        yield reg


@pytest.fixture
def calphad_registry():
    with patch("app.tools.simulation.calphad_bridge.check_calphad_available", return_value=True):
        from app.tools.calphad import create_calphad_tools
        reg = ToolRegistry()
        create_calphad_tools(reg)
        yield reg


# ===========================================================================
# Sim collapse: 13 → 7
# ===========================================================================

class TestSimRegistration:
    def test_unified_tools_registered(self, sim_registry):
        names = [t.name for t in sim_registry.list_tools()]
        # 3 unified
        assert "structure" in names
        assert "sim_run" in names
        assert "sim_job" in names
        # 4 standalone (kept — different shapes/concepts)
        assert "list_potentials" in names
        assert "check_hpc_queue" in names
        assert "run_convergence_test" in names
        assert "run_workflow" in names

    def test_old_names_removed(self, sim_registry):
        names = [t.name for t in sim_registry.list_tools()]
        for old in (
            "create_structure", "modify_structure", "get_structure_info",
            "run_simulation", "submit_hpc_job",
            "get_job_status", "get_job_results", "list_jobs", "delete_job",
        ):
            assert old not in names, f"{old} should have been collapsed"

    def test_total_count(self, sim_registry):
        names = [t.name for t in sim_registry.list_tools()]
        assert len(names) == 7  # was 13 pre-Round-5

    def test_sim_run_requires_approval(self, sim_registry):
        """sim_run is money/compute-spending — must be approval-gated."""
        assert sim_registry.get("sim_run").requires_approval is True

    def test_structure_no_approval(self, sim_registry):
        """Structure ops are cheap CPU — should NOT require approval."""
        assert sim_registry.get("structure").requires_approval is False

    def test_sim_job_no_approval(self, sim_registry):
        """Job mgmt ops are cheap polling/cleanup — should NOT require approval."""
        assert sim_registry.get("sim_job").requires_approval is False


class TestStructureDispatcher:
    def test_missing_action(self, sim_registry):
        r = sim_registry.get("structure").execute()
        assert "error" in r and "Missing 'action'" in r["error"]

    def test_unknown_action(self, sim_registry):
        r = sim_registry.get("structure").execute(action="bogus")
        assert "error" in r and "Unknown action" in r["error"]

    def test_create_requires_element(self, sim_registry):
        r = sim_registry.get("structure").execute(action="create")
        assert "error" in r and "element" in r["error"]

    def test_modify_requires_id_and_op(self, sim_registry):
        r = sim_registry.get("structure").execute(action="modify")
        assert "error" in r
        assert "structure_id" in r["error"] and "operation" in r["error"]

    def test_info_requires_id(self, sim_registry):
        r = sim_registry.get("structure").execute(action="info")
        assert "error" in r and "structure_id" in r["error"]

    def test_create_dispatches(self, sim_registry):
        with patch("app.tools.sim_tools._create_structure", return_value={"structure_id": "s1"}) as m:
            r = sim_registry.get("structure").execute(action="create", element="Al")
            assert r == {"structure_id": "s1"}
            # action= is consumed by the dispatcher before forwarding
            m.assert_called_once_with(element="Al")

    def test_modify_dispatches(self, sim_registry):
        with patch("app.tools.sim_tools._modify_structure", return_value={"structure_id": "s1", "operation": "supercell"}) as m:
            sim_registry.get("structure").execute(
                action="modify", structure_id="s1", operation="supercell",
            )
            m.assert_called_once()

    def test_info_dispatches(self, sim_registry):
        with patch("app.tools.sim_tools._get_structure_info", return_value={"formula": "Al"}) as m:
            sim_registry.get("structure").execute(action="info", structure_id="s1")
            m.assert_called_once()


class TestSimRunDispatcher:
    def test_missing_structure_id_local(self, sim_registry):
        r = sim_registry.get("sim_run").execute()
        assert "error" in r and "structure_id" in r["error"]

    def test_unknown_target(self, sim_registry):
        r = sim_registry.get("sim_run").execute(target="moon")
        assert "error" in r and "Unknown target" in r["error"]

    def test_local_dispatches(self, sim_registry):
        with patch("app.tools.sim_tools._run_simulation", return_value={"job_id": "j1"}) as m:
            sim_registry.get("sim_run").execute(target="local", structure_id="s1")
            m.assert_called_once()

    def test_hpc_dispatches(self, sim_registry):
        with patch("app.tools.sim_tools._submit_hpc_job", return_value={"job_id": "j1"}) as m:
            sim_registry.get("sim_run").execute(target="hpc", structure_id="s1", queue="big")
            m.assert_called_once()

    def test_default_target_is_local(self, sim_registry):
        with patch("app.tools.sim_tools._run_simulation", return_value={"job_id": "j1"}) as m:
            sim_registry.get("sim_run").execute(structure_id="s1")
            m.assert_called_once()


class TestSimJobDispatcher:
    def test_missing_action(self, sim_registry):
        r = sim_registry.get("sim_job").execute()
        assert "error" in r and "Missing 'action'" in r["error"]

    def test_unknown_action(self, sim_registry):
        r = sim_registry.get("sim_job").execute(action="explode")
        assert "error" in r and "Unknown action" in r["error"]

    def test_status_requires_job_id(self, sim_registry):
        r = sim_registry.get("sim_job").execute(action="status")
        assert "error" in r and "job_id" in r["error"]

    def test_results_requires_job_id(self, sim_registry):
        r = sim_registry.get("sim_job").execute(action="results")
        assert "error" in r and "job_id" in r["error"]

    def test_delete_requires_job_id(self, sim_registry):
        r = sim_registry.get("sim_job").execute(action="delete")
        assert "error" in r and "job_id" in r["error"]

    def test_list_no_required_args(self, sim_registry):
        with patch("app.tools.sim_tools._list_jobs", return_value={"jobs": [], "count": 0}) as m:
            r = sim_registry.get("sim_job").execute(action="list")
            assert "error" not in r
            m.assert_called_once()

    def test_status_dispatches(self, sim_registry):
        with patch("app.tools.sim_tools._get_job_status", return_value={"status": "finished"}) as m:
            sim_registry.get("sim_job").execute(action="status", job_id="j1")
            m.assert_called_once()

    def test_results_dispatches(self, sim_registry):
        with patch("app.tools.sim_tools._get_job_results", return_value={"energy_tot": -1.5}) as m:
            sim_registry.get("sim_job").execute(action="results", job_id="j1")
            m.assert_called_once()

    def test_delete_dispatches(self, sim_registry):
        with patch("app.tools.sim_tools._delete_job", return_value={"deleted": "j1"}) as m:
            sim_registry.get("sim_job").execute(action="delete", job_id="j1", confirm=True)
            m.assert_called_once()


# ===========================================================================
# Calphad collapse: 6 → 2
# ===========================================================================

class TestCalphadRegistration:
    def test_both_tools_registered(self, calphad_registry):
        names = [t.name for t in calphad_registry.list_tools()]
        assert "calphad" in names
        assert "calphad_compute" in names

    def test_old_names_removed(self, calphad_registry):
        names = [t.name for t in calphad_registry.list_tools()]
        for old in (
            "calculate_phase_diagram", "calculate_equilibrium", "calculate_gibbs_energy",
            "list_calphad_databases", "list_phases", "import_calphad_database",
        ):
            assert old not in names, f"{old} should have been collapsed"

    def test_calphad_compute_requires_approval(self, calphad_registry):
        """Compute actions are real-money — must be approval-gated."""
        assert calphad_registry.get("calphad_compute").requires_approval is True

    def test_calphad_no_approval(self, calphad_registry):
        """Catalog/IO ops are cheap — should NOT require approval."""
        assert calphad_registry.get("calphad").requires_approval is False


class TestCalphadDispatcher:
    def test_missing_action(self, calphad_registry):
        r = calphad_registry.get("calphad").execute()
        assert "error" in r and "Missing 'action'" in r["error"]

    def test_list_phases_requires_db(self, calphad_registry):
        r = calphad_registry.get("calphad").execute(action="list_phases")
        assert "error" in r and "database_name" in r["error"]

    def test_import_requires_path(self, calphad_registry):
        r = calphad_registry.get("calphad").execute(action="import")
        assert "error" in r and "source_path" in r["error"]

    def test_list_databases_dispatches(self, calphad_registry):
        with patch("app.tools.calphad._list_databases", return_value={"databases": [], "count": 0}) as m:
            r = calphad_registry.get("calphad").execute(action="list_databases")
            assert "error" not in r
            m.assert_called_once()


class TestCalphadComputeDispatcher:
    def test_missing_action(self, calphad_registry):
        r = calphad_registry.get("calphad_compute").execute()
        assert "error" in r and "Missing 'action'" in r["error"]

    def test_phase_diagram_requires_db(self, calphad_registry):
        r = calphad_registry.get("calphad_compute").execute(action="phase_diagram")
        assert "error" in r and "database_name" in r["error"]

    def test_equilibrium_requires_conditions(self, calphad_registry):
        r = calphad_registry.get("calphad_compute").execute(
            action="equilibrium", database_name="d", components=["A", "B"],
        )
        assert "error" in r and "conditions" in r["error"]

    def test_gibbs_requires_temperature(self, calphad_registry):
        r = calphad_registry.get("calphad_compute").execute(
            action="gibbs", database_name="d", components=["A", "B"], phases=["P"],
        )
        assert "error" in r and "temperature" in r["error"]

    def test_phase_diagram_dispatches(self, calphad_registry):
        with patch("app.tools.calphad._calculate_phase_diagram", return_value={"diagram": {}}) as m:
            calphad_registry.get("calphad_compute").execute(
                action="phase_diagram",
                database_name="alni",
                components=["Al", "Ni"],
            )
            m.assert_called_once()
