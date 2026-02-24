"""Tests for simulation tools (all mocked — no pyiron required)."""
import pytest
from unittest.mock import patch, MagicMock, PropertyMock
import numpy as np

from app.tools.base import ToolRegistry
from app.tools.simulation import create_simulation_tools


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

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


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

class TestToolRegistration:
    def test_all_tools_registered(self):
        reg = _make_registry()
        names = [t.name for t in reg.list_tools()]
        expected = [
            "create_structure", "modify_structure", "get_structure_info",
            "list_potentials", "run_simulation", "get_job_status",
            "get_job_results", "list_jobs", "delete_job",
            "submit_hpc_job", "check_hpc_queue",
            "run_convergence_test", "run_workflow",
        ]
        for e in expected:
            assert e in names, f"Missing tool: {e}"
        assert len(names) == 13


# ---------------------------------------------------------------------------
# Guard: pyiron not installed
# ---------------------------------------------------------------------------

class TestPyironGuard:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=False)
    def test_create_structure_guard(self, _m):
        reg = _make_registry()
        result = reg.get("create_structure").execute(element="Fe")
        assert "error" in result
        assert "pyiron_atomistics" in result["error"]

    @patch("app.simulation.bridge.check_pyiron_available", return_value=False)
    def test_run_simulation_guard(self, _m):
        reg = _make_registry()
        result = reg.get("run_simulation").execute(structure_id="x")
        assert "error" in result


# ---------------------------------------------------------------------------
# C-1: Structure Tools
# ---------------------------------------------------------------------------

class TestCreateStructure:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_basic(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        mock_atoms = _mock_atoms()
        bridge.get_project().create.structure.bulk.return_value = mock_atoms
        bridge.structures.store.return_value = "struct_abc"

        reg = _make_registry()
        result = reg.get("create_structure").execute(element="Fe", crystal_structure="bcc")
        assert result["structure_id"] == "struct_abc"
        assert result["n_atoms"] == 4

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_with_supercell(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        mock_atoms = _mock_atoms("Fe32", 32)
        bridge.get_project().create.structure.bulk.return_value = mock_atoms
        mock_atoms.repeat.return_value = mock_atoms
        bridge.structures.store.return_value = "struct_xyz"

        reg = _make_registry()
        result = reg.get("create_structure").execute(
            element="Fe", crystal_structure="bcc", repeat_x=2, repeat_y=2, repeat_z=2
        )
        assert result["structure_id"] == "struct_xyz"
        mock_atoms.repeat.assert_called_once_with([2, 2, 2])


class TestModifyStructure:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_supercell(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        mock_atoms = _mock_atoms()
        bridge.structures.get.return_value = mock_atoms
        bridge.structures.store.return_value = "struct_new"

        reg = _make_registry()
        result = reg.get("modify_structure").execute(
            structure_id="struct_abc", operation="supercell", params={"nx": 3, "ny": 3, "nz": 3}
        )
        assert result["structure_id"] == "struct_new"
        assert result["operation"] == "supercell"

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_not_found(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = None

        reg = _make_registry()
        result = reg.get("modify_structure").execute(structure_id="bad_id", operation="supercell")
        assert "error" in result

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_unknown_operation(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = _mock_atoms()

        reg = _make_registry()
        result = reg.get("modify_structure").execute(
            structure_id="s1", operation="unknown_op"
        )
        assert "error" in result
        assert "Unknown operation" in result["error"]


class TestGetStructureInfo:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_basic(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = _mock_atoms()

        reg = _make_registry()
        result = reg.get("get_structure_info").execute(structure_id="struct_abc")
        assert result["formula"] == "Fe4"
        assert result["n_atoms"] == 4
        assert result["volume"] == 23.64

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_not_found(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = None

        reg = _make_registry()
        result = reg.get("get_structure_info").execute(structure_id="nope")
        assert "error" in result


class TestListPotentials:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
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


# ---------------------------------------------------------------------------
# C-2: Simulation / Job Tools
# ---------------------------------------------------------------------------

class TestRunSimulation:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_lammps(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = _mock_atoms()
        pr = bridge.get_project()
        mock_job = MagicMock()
        mock_job.status = "finished"
        pr.create.job.Lammps.return_value = mock_job
        bridge.jobs.store.return_value = "job_123"

        reg = _make_registry()
        result = reg.get("run_simulation").execute(
            structure_id="struct_abc", code="lammps", potential="Fe_eam"
        )
        assert result["job_id"] == "job_123"
        assert result["code"] == "lammps"

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_unsupported_code(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = _mock_atoms()

        reg = _make_registry()
        result = reg.get("run_simulation").execute(
            structure_id="s1", code="unknown_code"
        )
        assert "error" in result
        assert "Unsupported code" in result["error"]


class TestGetJobStatus:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_basic(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        mock_job = MagicMock()
        mock_job.status = "finished"
        mock_job.job_name = "test_job"
        bridge.jobs.get.return_value = mock_job

        reg = _make_registry()
        result = reg.get("get_job_status").execute(job_id="job_1")
        assert result["status"] == "finished"

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_not_found(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.jobs.get.return_value = None

        reg = _make_registry()
        result = reg.get("get_job_status").execute(job_id="bad_id")
        assert "error" in result


class TestGetJobResults:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_finished_job(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        mock_job = MagicMock()
        mock_job.status = "finished"
        mock_job.__getitem__ = MagicMock(side_effect=lambda k: {
            "energy_tot": -3.75,
            "volume": 23.6,
        }.get(k))
        bridge.jobs.get.return_value = mock_job

        reg = _make_registry()
        result = reg.get("get_job_results").execute(
            job_id="j1", properties=["energy_tot", "volume"]
        )
        assert result["energy_tot"] == -3.75
        assert result["volume"] == 23.6

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_not_finished(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        mock_job = MagicMock()
        mock_job.status = "running"
        bridge.jobs.get.return_value = mock_job

        reg = _make_registry()
        result = reg.get("get_job_results").execute(job_id="j1")
        assert "error" in result
        assert "not finished" in result["error"]


class TestListJobs:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_empty(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.jobs.to_summary_list.return_value = []

        reg = _make_registry()
        result = reg.get("list_jobs").execute()
        assert result["jobs"] == []
        assert result["count"] == 0

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_with_filter(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.jobs.to_summary_list.return_value = [
            {"id": "j1", "status": "finished", "code": "Lammps"},
            {"id": "j2", "status": "running", "code": "Vasp"},
        ]

        reg = _make_registry()
        result = reg.get("list_jobs").execute(status_filter="finished")
        assert result["count"] == 1
        assert result["jobs"][0]["id"] == "j1"


class TestDeleteJob:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_without_confirm(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge

        reg = _make_registry()
        result = reg.get("delete_job").execute(job_id="j1")
        assert "error" in result
        assert "confirm" in result["error"]

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_with_confirm(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.jobs.get.return_value = MagicMock()

        reg = _make_registry()
        result = reg.get("delete_job").execute(job_id="j1", confirm=True)
        assert result["deleted"] == "j1"


# ---------------------------------------------------------------------------
# C-3: HPC + Workflow Tools
# ---------------------------------------------------------------------------

class TestSubmitHPCJob:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_basic(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = _mock_atoms()
        pr = bridge.get_project()
        mock_job = MagicMock()
        mock_job.status = "submitted"
        mock_job.server = MagicMock()
        pr.create.job.Lammps.return_value = mock_job
        bridge.jobs.store.return_value = "hpc_job_1"

        reg = _make_registry()
        result = reg.get("submit_hpc_job").execute(
            structure_id="s1", code="lammps", queue="gpu", cores=8, walltime="02:00:00"
        )
        assert result["job_id"] == "hpc_job_1"
        assert result["queue"] == "gpu"
        assert result["cores"] == 8


class TestCheckHPCQueue:
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
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
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
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
    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
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

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_unknown_workflow(self, mock_bridge_fn, _avail):
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        bridge.structures.get.return_value = _mock_atoms()

        reg = _make_registry()
        result = reg.get("run_workflow").execute(
            workflow_type="nonexistent", structure_id="s1"
        )
        assert "error" in result
        assert "Unknown workflow" in result["error"]
