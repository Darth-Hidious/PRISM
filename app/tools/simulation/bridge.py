"""Pyiron bridge — thin wrapper between PRISM tools and the pyiron stack.

ARCHITECTURAL BOUNDARY (do not violate without discussion)
==========================================================

This module is the ONLY place PRISM imports from pyiron. Everything else
goes through `get_bridge()` and the {Structure,Job}Store helpers exposed
here. That's deliberate. See docs/pyiron_integration.md for the full
rationale; the short version:

  PRISM is an AI-native research workspace.
  pyiron is an atomistic-simulation IDE / orchestrator.

They sit at DIFFERENT levels of the stack. pyiron is a tool inside
PRISM, not a competitor. To keep the boundary clean:

  WE USE
    - pyiron_atomistics    (LAMMPS / VASP / GPAW / SPHInX wrappers + ASE compat)
    - pyiron_base          (Project, Job, HDF5/SQL storage, queue submission)
    - executorlib          (HPC dispatch via SLURM, transitive)

  WE DO NOT USE
    - pyiron_workflow      (graph-based workflow framework — overlaps with our
                            skills + YAML workflows; different abstraction)
    - pyiron_core          (visual GUI for graph workflows — pure UI, irrelevant)

  WE DO NOT DUPLICATE
    - HDF5 storage of simulation results (pyiron owns this; our artifact
      store records pointers/summaries, not the bulk data)
    - The pyiron job database (SQL) — we keep our own JobStore as a
      session-local cache of references, not a parallel database
    - SLURM submission for atomistic codes (always go through
      `pyiron job.server.queue` / executorlib; our compute broker is for
      generic GPU work — training, inference, container jobs — NOT
      atomistic sim)
    - ASE-compatible structure objects (always use pyiron's, never roll
      our own)

  RESULT-LAYER POSITIONING
    - pyiron's HDF5 = source of truth for simulation outputs
    - PRISM's artifact store (Memex layer) = high-level summaries + the
      pyiron job_id as provenance pointer; the agent recalls the
      pointer, then dereferences via this bridge if it needs the full
      HDF5 data

If you find yourself reaching for pyiron_workflow's graph nodes or
duplicating HDF5 storage, stop and re-read this docstring. The whole
point of having pyiron as a backend is to NOT solve problems they've
already solved well.
"""
import json
import uuid
from pathlib import Path
from typing import Any, Dict, List, Optional

# Same pinned window as `prism pyiron install` (crates/cli/src/pyiron_cmd.rs)
# — keep the two in sync. pyiron_atomistics >=0.6 is the line that ships
# Python 3.9–3.14 wheels (brings pyiron_base); the old `pyiron` meta package
# pins pre-3.14 deps that try to BUILD numpy/pandas from source — never
# install that here.
_PYIRON_SPEC = ["pyiron_atomistics>=0.5,<0.6"]

# One-shot guard: a failed install (no network, no pip) must not re-run a
# multi-minute pip attempt on every tool call in this process.
_AUTO_PROVISION_ATTEMPTED = False


def _try_auto_provision() -> bool:
    """Best-effort pip install into the running interpreter. NEVER raises."""
    global _AUTO_PROVISION_ATTEMPTED
    if _AUTO_PROVISION_ATTEMPTED:
        return False
    _AUTO_PROVISION_ATTEMPTED = True
    try:
        import subprocess
        import sys
        result = subprocess.run(
            [sys.executable, "-m", "pip", "install", *_PYIRON_SPEC],
            capture_output=True,
            timeout=600,
        )
        return result.returncode == 0
    except Exception:
        return False


def check_pyiron_available(auto_provision: bool = False) -> bool:
    """Return True if pyiron_atomistics is importable, optionally
    auto-installing it on first miss.

    pyiron_atomistics 0.6+ supports Python 3.9–3.14 (atomistics 0.3.6+).
    Earlier PRISM versions gated this at python_version<'3.14' which
    excluded macOS arm64 default Python; the gate is now <'3.15'.

    auto_provision defaults to FALSE because this is called from startup
    paths (capabilities catalog, plugin bootstrap, mcp_server) — a pip run
    there blocks backend boot for minutes. Only actual simulation TOOL
    CALLS pass auto_provision=True (see sim_tools._guard). The install is
    one-shot per process and never raises: if pip fails (offline,
    sandboxed), this returns False and the calling tool surfaces the
    standard error dict — the TUI/backend must never crash over it.
    """
    try:
        import pyiron_atomistics  # noqa: F401
        return True
    except ImportError:
        pass
    if auto_provision and _try_auto_provision():
        try:
            import importlib
            importlib.invalidate_caches()
            import pyiron_atomistics  # noqa: F401
            return True
        except ImportError:
            return False
    return False


def _pyiron_missing_error() -> dict:
    """Standard error dict when pyiron is not installed."""
    return {
        "error": (
            "pyiron_atomistics is not installed and automatic installation "
            "failed (offline?). Run `prism pyiron install`, or "
            "`pip install prism-platform[simulation]`."
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
