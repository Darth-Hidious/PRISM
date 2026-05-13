"""Backend abstract interface and selection heuristic."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any, Callable

from ..auth import get_backend_override
from ..schemas import Backend as BackendName

# Progress callback signature: progress(percent, message, step, total)
ProgressCb = Callable[[float, str, int, int], None]


# Tools that are GPU-bound enough that we *prefer* HF Jobs by default.
_GPU_BOUND_TOOLS = frozenset({"compute_elastic", "phonon_harmonic", "md_equilibrate"})


@dataclass
class BackendJob:
    """Spec for one backend execution."""

    tool_name: str
    input_payload: dict[str, Any]
    cache_key: str
    seed: int = 20260506
    timeout_seconds: int = 3600
    extras: dict[str, Any] = field(default_factory=dict)


class Backend(ABC):
    """A compute backend.

    Implementations execute one ``BackendJob`` and return a ``dict`` shaped
    like the corresponding ``*Result`` schema, plus a ``cif_text`` field
    (optional, the structure CIF text) and a ``backend_details`` field
    (free-form metadata used by provenance).

    Execution is synchronous from the runner's point of view (the runner
    runs each backend in a worker task). Backends MUST honour
    ``progress`` callbacks where possible so MCP progress notifications
    flow back to the client.
    """

    name: str = ""

    @abstractmethod
    def execute(self, job: BackendJob, progress: ProgressCb | None = None) -> dict[str, Any]:
        """Execute the job. Returns a result dict on success; raises on failure."""

    @abstractmethod
    def cancel(self, job_id: str) -> None:  # noqa: D401
        """Best-effort cancel. Idempotent."""

    def estimate_seconds(self, job: BackendJob) -> int:  # pragma: no cover - simple default
        """Wall-time estimate. Override per backend for accuracy."""
        return 60


def select_backend(
    tool_name: str,
    n_atoms: int,
    requested: BackendName,
    backends: dict[BackendName, Backend],
) -> Backend:
    """Pick a backend instance based on tool, N_atoms, and caller preference.

    Rules (in order):

    1. ``MACE_MCP_BACKEND`` env override always wins (e.g. ``"fake"`` in CI).
    2. Caller-supplied ``backend`` other than ``"auto"`` is honoured.
    3. Otherwise:
       - GPU-bound tool OR ``n_atoms > 30``       -> ``hf_jobs`` if available, else ``local``
       - else                                     -> ``local`` if available, else ``hf_jobs``
       - fallback                                 -> ``fake``
    """
    override = get_backend_override()
    if override and override in backends:
        return backends[override]

    if requested != "auto" and requested in backends:
        return backends[requested]

    needs_gpu = tool_name in _GPU_BOUND_TOOLS or n_atoms > 30
    if needs_gpu:
        if "hf_jobs" in backends:
            return backends["hf_jobs"]
        if "local" in backends:
            return backends["local"]
    else:
        if "local" in backends:
            return backends["local"]
        if "hf_jobs" in backends:
            return backends["hf_jobs"]
    return backends["fake"]
