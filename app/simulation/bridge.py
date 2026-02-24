"""Pyiron bridge layer — lazy init, config management, structure/job stores."""
import json
import uuid
from pathlib import Path
from typing import Any, Dict, List, Optional


def check_pyiron_available() -> bool:
    """Return True if pyiron_atomistics is importable."""
    try:
        import pyiron_atomistics  # noqa: F401
        return True
    except ImportError:
        return False


def _pyiron_missing_error() -> dict:
    """Standard error dict when pyiron is not installed."""
    return {
        "error": (
            "pyiron_atomistics is not installed. "
            "Install simulation extras with: pip install prism-platform[simulation]"
        )
    }


class StructureStore:
    """In-memory store mapping structure IDs to ASE Atoms objects."""

    def __init__(self):
        self._structures: Dict[str, Any] = {}

    def store(self, atoms) -> str:
        """Store an Atoms object, return its ID."""
        sid = f"struct_{uuid.uuid4().hex[:8]}"
        self._structures[sid] = atoms
        return sid

    def get(self, structure_id: str):
        """Return the Atoms object for *structure_id*, or None."""
        return self._structures.get(structure_id)

    def list_ids(self) -> List[str]:
        """Return all stored structure IDs."""
        return list(self._structures.keys())

    def delete(self, structure_id: str) -> bool:
        """Delete a structure by ID. Returns True if found."""
        return self._structures.pop(structure_id, None) is not None

    def to_summary_list(self) -> List[dict]:
        """Return a summary list of all stored structures."""
        summaries = []
        for sid, atoms in self._structures.items():
            summaries.append({
                "id": sid,
                "formula": atoms.get_chemical_formula() if hasattr(atoms, "get_chemical_formula") else str(atoms),
                "n_atoms": len(atoms) if hasattr(atoms, "__len__") else 0,
            })
        return summaries


class JobStore:
    """In-memory store mapping job IDs to pyiron job references."""

    def __init__(self):
        self._jobs: Dict[str, Any] = {}

    def store(self, job, job_id: Optional[str] = None) -> str:
        """Store a pyiron job, return its ID."""
        if job_id is None:
            job_id = f"job_{uuid.uuid4().hex[:8]}"
        self._jobs[job_id] = job
        return job_id

    def get(self, job_id: str):
        """Return the job for *job_id*, or None."""
        return self._jobs.get(job_id)

    def list_ids(self) -> List[str]:
        return list(self._jobs.keys())

    def delete(self, job_id: str) -> bool:
        return self._jobs.pop(job_id, None) is not None

    def to_summary_list(self) -> List[dict]:
        summaries = []
        for jid, job in self._jobs.items():
            status = "unknown"
            code = "unknown"
            try:
                status = str(job.status)
            except Exception:
                pass
            try:
                code = job.__class__.__name__
            except Exception:
                pass
            summaries.append({"id": jid, "status": status, "code": code})
        return summaries


class PyironBridge:
    """Thin bridge between PRISM tools and the pyiron stack.

    Lazily initialises a pyiron Project and holds the structure/job stores
    that simulation tools share.
    """

    def __init__(self, project_name: str = "prism_default"):
        self._project = None
        self._project_name = project_name
        self.structures = StructureStore()
        self.jobs = JobStore()

    # -- lazy project access --------------------------------------------------

    def get_project(self):
        """Return (and lazily create) a pyiron Project."""
        if self._project is None:
            from pyiron_atomistics import Project
            self._project = Project(self._project_name)
        return self._project

    # -- HPC configuration ----------------------------------------------------

    _HPC_CONFIG_PATH = Path.home() / ".prism" / "hpc_config.json"

    def configure_hpc(
        self,
        queue_system: str = "SLURM",
        queue_name: str = "default",
        cores: int = 1,
        memory: Optional[str] = None,
        walltime: str = "01:00:00",
    ) -> dict:
        """Persist HPC queue configuration to ~/.prism/hpc_config.json."""
        config = {
            "queue_system": queue_system.upper(),
            "queue_name": queue_name,
            "cores": cores,
            "memory": memory,
            "walltime": walltime,
        }
        self._HPC_CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
        self._HPC_CONFIG_PATH.write_text(json.dumps(config, indent=2))
        return config

    def load_hpc_config(self) -> Optional[dict]:
        """Load HPC config from disk, or return None if absent."""
        if self._HPC_CONFIG_PATH.exists():
            return json.loads(self._HPC_CONFIG_PATH.read_text())
        return None

    def apply_hpc_config(self, job, hpc_config: Optional[dict] = None) -> None:
        """Apply HPC settings to a pyiron job's server object."""
        cfg = hpc_config or self.load_hpc_config()
        if cfg is None:
            return
        job.server.queue = cfg.get("queue_name", "default")
        job.server.cores = cfg.get("cores", 1)
        if cfg.get("walltime"):
            job.server.run_time = cfg["walltime"]


# Module-level singleton so all tools share the same stores.
_bridge: Optional[PyironBridge] = None


def get_bridge() -> PyironBridge:
    """Return the module-level PyironBridge singleton."""
    global _bridge
    if _bridge is None:
        _bridge = PyironBridge()
    return _bridge
