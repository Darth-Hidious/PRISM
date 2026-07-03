"""Tests for the four standalone simulation tools that survived Round 5.

Round 5 (PR #23) collapsed 13 simulation tools → 7:
  - 3 unified dispatchers: structure / sim_run / sim_job (tested in
    tests/test_round5_sim_collapse.py)
  - 4 standalone tools kept because they have different shapes:
    list_potentials, check_hpc_queue, run_convergence_test, run_workflow

This file covers behavior of the 4 standalone tools. The previous
tests for the now-collapsed 9 individual tools (create_structure,
run_simulation, get_job_status, …) were removed when the dispatchers
landed; their replacements live in test_round5_sim_collapse.py.

All tests mocked — no pyiron required.
"""
from unittest.mock import patch, MagicMock

from app.tools.base import ToolRegistry
from app.tools.sim_tools import create_simulation_tools


def _make_registry():
    reg = ToolRegistry()
    create_simulation_tools(reg)
    return reg


def _mock_atoms(formula="Fe4", n=4):
    """Return a MagicMock that behaves like an ASE Atoms object."""
    atoms = MagicMock()
    atoms.get_chemical_formula.return_value = formula
    atoms.__len__ = MagicMock(return_value=n)
    atoms.cell = MagicMock()
    atoms.cell.tolist.return_value = [[2.87, 0, 0], [0, 2.87, 0], [0, 0, 2.87]]
    atoms.positions = MagicMock()
    atoms.positions.tolist.return_value = [[0, 0, 0], [1.435, 1.435, 1.435]]
    atoms.pbc = MagicMock()
    atoms.pbc.tolist.return_value = [True, True, True]
    atoms.get_volume.return_value = 23.64
    atoms.copy.return_value = atoms
    atoms.repeat.return_value = atoms
    atoms.set_cell = MagicMock()
    return atoms


class TestListPotentials:
    @patch("app.tools.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.tools.simulation.bridge.get_bridge")
    def test_basic(self, mock_bridge_fn, _avail):
        import pandas as pd
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        pr = bridge.get_project()
        mock_job = MagicMock()
        pr.create.job.Lammps.return_value = mock_job
        mock_job.list_potentials.return_value = pd.DataFrame({
            "Name": ["Fe_eam", "Al_eam"],
            "Species": [["Fe"], ["Al"]],
            "Model": ["EAM", "EAM"],
        })

        reg = _make_registry()
        result = reg.get("list_potentials").execute(element="Fe")
        assert "potentials" in result
        # Only Fe should remain
        assert len(result["potentials"]) == 1
        assert "Fe" in result["potentials"][0]["name"]


class TestCheckHPCQueue:
    @patch("app.tools.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.tools.simulation.bridge.get_bridge")
    def test_fallback(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        pr = bridge.get_project()
        pr.queue_status.side_effect = Exception("no queue")
        bridge.jobs.to_summary_list.return_value = [
            {"id": "j1", "status": "running", "code": "Lammps"},
        ]

        reg = _make_registry()
        result = reg.get("check_hpc_queue").execute()
        assert result["count"] == 1


class TestRunConvergenceTest:
    @patch("app.tools.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.tools.simulation.bridge.get_bridge")
    def test_basic(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = _mock_atoms()
        pr = bridge.get_project()
        mock_job = MagicMock()
        mock_job.__getitem__ = MagicMock(return_value=-3.5)
        pr.create.job.Lammps.return_value = mock_job

        reg = _make_registry()
        result = reg.get("run_convergence_test").execute(
            structure_id="s1", code="lammps",
            parameter_name="encut", parameter_values=[200, 300, 400]
        )
        assert result["parameter_name"] == "encut"
        assert len(result["energies"]) == 3


class TestRunWorkflow:
    @patch("app.tools.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.tools.simulation.bridge.get_bridge")
    def test_elastic(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = _mock_atoms()
        pr = bridge.get_project()
        mock_wf_job = MagicMock()
        mock_wf_job.status = "finished"
        pr.create.job.ElasticMatrix.return_value = mock_wf_job
        mock_ref = MagicMock()
        pr.create.job.Lammps.return_value = mock_ref
        bridge.jobs.store.return_value = "wf_1"

        reg = _make_registry()
        result = reg.get("run_workflow").execute(
            workflow_type="elastic_constants", structure_id="s1",
            parameters={"code": "lammps"}
        )
        assert result["job_id"] == "wf_1"
        assert result["workflow_type"] == "elastic_constants"

    @patch("app.tools.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.tools.simulation.bridge.get_bridge")
    def test_unknown_workflow(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = _mock_atoms()

        reg = _make_registry()
        result = reg.get("run_workflow").execute(
            workflow_type="nonexistent", structure_id="s1"
        )
        assert "error" in result
