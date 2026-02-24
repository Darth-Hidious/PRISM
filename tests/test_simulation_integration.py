"""Integration test: structure → simulation → results pipeline (mocked)."""
import pytest
from unittest.mock import patch, MagicMock
from app.tools.base import ToolRegistry
from app.tools.simulation import create_simulation_tools


def _mock_atoms(formula="Al4", n=4):
    atoms = MagicMock()
    atoms.get_chemical_formula.return_value = formula
    atoms.__len__ = MagicMock(return_value=n)
    atoms.cell = MagicMock()
    atoms.cell.tolist.return_value = [[4.05, 0, 0], [0, 4.05, 0], [0, 0, 4.05]]
    atoms.positions = MagicMock()
    atoms.positions.tolist.return_value = [[0, 0, 0]]
    atoms.pbc = MagicMock()
    atoms.pbc.tolist.return_value = [True, True, True]
    atoms.get_volume.return_value = 66.43
    atoms.copy.return_value = atoms
    atoms.repeat.return_value = atoms
    return atoms


class TestEndToEndPipeline:
    """Mocked end-to-end: create structure → run LAMMPS minimization → get results."""

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_create_simulate_get_results(self, mock_bridge_fn, _avail):
        # --- setup bridge mock ---
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        pr = bridge.get_project()

        mock_atoms = _mock_atoms()
        pr.create.structure.bulk.return_value = mock_atoms

        # Structure store: first call stores and returns id, second call retrieves
        stored = {}
        def store_side_effect(a):
            sid = "struct_test_001"
            stored[sid] = a
            return sid
        bridge.structures.store.side_effect = store_side_effect
        bridge.structures.get.side_effect = lambda sid: stored.get(sid, mock_atoms)

        # Job mock
        mock_job = MagicMock()
        mock_job.status = "finished"
        mock_job.__getitem__ = MagicMock(side_effect=lambda k: {
            "energy_tot": -14.97,
            "forces": [[0.0, 0.0, 0.0]] * 4,
            "volume": 66.1,
        }.get(k))
        pr.create.job.Lammps.return_value = mock_job

        job_stored = {}
        def job_store_side_effect(j, job_id=None):
            jid = job_id or "job_test_001"
            job_stored[jid] = j
            return jid
        bridge.jobs.store.side_effect = job_store_side_effect
        bridge.jobs.get.side_effect = lambda jid: job_stored.get(jid, mock_job)

        # --- Build registry ---
        reg = ToolRegistry()
        create_simulation_tools(reg)

        # Step 1: Create structure
        result1 = reg.get("create_structure").execute(element="Al", crystal_structure="fcc")
        assert "error" not in result1
        struct_id = result1["structure_id"]
        assert struct_id == "struct_test_001"
        assert result1["n_atoms"] == 4

        # Step 2: Run LAMMPS minimization
        result2 = reg.get("run_simulation").execute(
            structure_id=struct_id,
            code="lammps",
            potential="Al_eam",
            parameters={"calc_type": "minimize"},
        )
        assert "error" not in result2
        job_id = result2["job_id"]
        assert result2["status"] == "finished"

        # Step 3: Get results
        result3 = reg.get("get_job_results").execute(
            job_id=job_id,
            properties=["energy_tot", "forces", "volume"],
        )
        assert "error" not in result3
        assert result3["energy_tot"] == -14.97
        assert result3["volume"] == 66.1

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    @patch("app.simulation.bridge.get_bridge")
    def test_create_modify_get_info(self, mock_bridge_fn, _avail):
        """Test structure creation → modification → info retrieval chain."""
        bridge = MagicMock()
        mock_bridge_fn.return_value = bridge
        pr = bridge.get_project()

        mock_atoms = _mock_atoms()
        pr.create.structure.bulk.return_value = mock_atoms
        bridge.structures.store.return_value = "struct_mod_001"
        bridge.structures.get.return_value = mock_atoms

        reg = ToolRegistry()
        create_simulation_tools(reg)

        # Create
        r1 = reg.get("create_structure").execute(element="Al")
        assert "error" not in r1

        # Modify (supercell)
        bridge.structures.store.return_value = "struct_mod_002"
        r2 = reg.get("modify_structure").execute(
            structure_id="struct_mod_001",
            operation="supercell",
            params={"nx": 2, "ny": 2, "nz": 2},
        )
        assert "error" not in r2
        assert r2["structure_id"] == "struct_mod_002"

        # Get info
        r3 = reg.get("get_structure_info").execute(structure_id="struct_mod_002")
        assert "error" not in r3
        assert r3["formula"] == "Al4"
