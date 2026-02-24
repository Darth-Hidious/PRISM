"""Tests for the pyiron bridge layer."""
import json
import pytest
from unittest.mock import patch, MagicMock
from app.simulation.bridge import (
    check_pyiron_available,
    _pyiron_missing_error,
    StructureStore,
    JobStore,
    PyironBridge,
    get_bridge,
)


class TestCheckPyironAvailable:
    def test_returns_bool(self):
        result = check_pyiron_available()
        assert isinstance(result, bool)

    def test_missing_error_message(self):
        err = _pyiron_missing_error()
        assert "error" in err
        assert "pyiron_atomistics" in err["error"]


class TestStructureStore:
    def test_store_and_get(self):
        store = StructureStore()
        mock_atoms = MagicMock()
        mock_atoms.get_chemical_formula.return_value = "Fe4"
        mock_atoms.__len__ = MagicMock(return_value=4)

        sid = store.store(mock_atoms)
        assert sid.startswith("struct_")
        assert store.get(sid) is mock_atoms

    def test_get_missing(self):
        store = StructureStore()
        assert store.get("nonexistent") is None

    def test_list_ids(self):
        store = StructureStore()
        s1 = store.store(MagicMock())
        s2 = store.store(MagicMock())
        ids = store.list_ids()
        assert s1 in ids
        assert s2 in ids

    def test_delete(self):
        store = StructureStore()
        sid = store.store(MagicMock())
        assert store.delete(sid) is True
        assert store.get(sid) is None
        assert store.delete(sid) is False

    def test_to_summary_list(self):
        store = StructureStore()
        mock_atoms = MagicMock()
        mock_atoms.get_chemical_formula.return_value = "Si2"
        mock_atoms.__len__ = MagicMock(return_value=2)
        sid = store.store(mock_atoms)

        summaries = store.to_summary_list()
        assert len(summaries) == 1
        assert summaries[0]["id"] == sid
        assert summaries[0]["formula"] == "Si2"
        assert summaries[0]["n_atoms"] == 2


class TestJobStore:
    def test_store_and_get(self):
        store = JobStore()
        mock_job = MagicMock()
        jid = store.store(mock_job)
        assert jid.startswith("job_")
        assert store.get(jid) is mock_job

    def test_store_with_custom_id(self):
        store = JobStore()
        mock_job = MagicMock()
        jid = store.store(mock_job, job_id="my_job")
        assert jid == "my_job"
        assert store.get("my_job") is mock_job

    def test_delete(self):
        store = JobStore()
        jid = store.store(MagicMock())
        assert store.delete(jid) is True
        assert store.get(jid) is None

    def test_to_summary_list(self):
        store = JobStore()
        mock_job = MagicMock()
        mock_job.status = "finished"
        mock_job.__class__.__name__ = "Lammps"
        jid = store.store(mock_job)

        summaries = store.to_summary_list()
        assert len(summaries) == 1
        assert summaries[0]["id"] == jid
        assert summaries[0]["status"] == "finished"


class TestPyironBridge:
    def test_init(self):
        bridge = PyironBridge(project_name="test_proj")
        assert bridge._project is None
        assert bridge._project_name == "test_proj"

    @patch("app.simulation.bridge.check_pyiron_available", return_value=True)
    def test_get_project_lazy(self, _mock):
        with patch("app.simulation.bridge.PyironBridge.get_project") as mock_get:
            mock_get.return_value = MagicMock()
            bridge = PyironBridge()
            pr = bridge.get_project()
            assert pr is not None

    def test_hpc_config_roundtrip(self, tmp_path):
        bridge = PyironBridge()
        # Override config path for test
        bridge._HPC_CONFIG_PATH = tmp_path / "hpc_config.json"

        config = bridge.configure_hpc(
            queue_system="SLURM",
            queue_name="batch",
            cores=16,
            walltime="02:00:00",
        )
        assert config["queue_system"] == "SLURM"
        assert config["cores"] == 16

        loaded = bridge.load_hpc_config()
        assert loaded is not None
        assert loaded["queue_name"] == "batch"
        assert loaded["walltime"] == "02:00:00"

    def test_load_hpc_config_missing(self, tmp_path):
        bridge = PyironBridge()
        bridge._HPC_CONFIG_PATH = tmp_path / "nonexistent.json"
        assert bridge.load_hpc_config() is None

    def test_apply_hpc_config(self):
        bridge = PyironBridge()
        mock_job = MagicMock()
        mock_job.server = MagicMock()

        bridge.apply_hpc_config(mock_job, hpc_config={
            "queue_name": "gpu",
            "cores": 8,
            "walltime": "04:00:00",
        })
        assert mock_job.server.queue == "gpu"
        assert mock_job.server.cores == 8
        assert mock_job.server.run_time == "04:00:00"


class TestGetBridge:
    def test_returns_singleton(self):
        # Reset singleton
        import app.simulation.bridge as mod
        mod._bridge = None
        b1 = get_bridge()
        b2 = get_bridge()
        assert b1 is b2
        mod._bridge = None  # cleanup
