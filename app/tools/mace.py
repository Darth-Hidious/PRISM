"""Native PRISM tools wrapping the MACE foundation interatomic-potential primitives.

Mirrors the pattern in `app/tools/calphad.py`: each tool is a thin wrapper
(`_guard()` + private `_func(**kwargs) -> dict`) registered via
`create_mace_tools(registry)`.

The actual physics lives in `app/tools/simulation/mace/` (the merged
mace-mcp subpackage). This file only does the agent-facing surface: input
validation via the pydantic schemas, async→sync bridging, and the
PRISM Tool dataclass registrations.

Framework reality (aerospace-grade honesty): MACE-MH-1 is PyTorch-only as
of 2026; mace-jax doesn't support the multi-head MH-1 architecture yet.
PRISM's broader stack defaults to JAX-native (jax-md, Flax, Equinox,
NumPyro, BlackJAX); MACE is an explicit upstream-pinned PyTorch holdout.
The tool descriptions below state this so the agent doesn't pick MACE
when a JAX-native MLIP would do.
"""

from __future__ import annotations

import asyncio
import atexit
import logging
import threading
from typing import Any

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _guard() -> dict[str, Any] | None:
    """Return an error dict if the mace subpackage's deps aren't installed."""
    from app.tools.simulation.mace_bridge import (
        _mace_missing_error,
        check_mace_available,
    )
    if not check_mace_available():
        return _mace_missing_error()
    return None


class _PersistentLoop:
    """One asyncio loop on a daemon thread, for the whole process lifetime.

    Why this and not a per-call loop:

    ``JobRunner.submit`` does ``asyncio.create_task(self._run_one(...))``
    (fire-and-forget worker) and returns a ``JobHandle`` immediately;
    ``_run_one`` then writes status/result to the disk-backed ``JobStore``
    as the backend runs. That design *requires* an event loop that
    OUTLIVES the submit call.

    A per-call loop forces a lose-lose:
      * ``asyncio.run`` / ``new_event_loop`` tears down right after submit
        returns → the still-pending ``_run_one`` is cancelled before it
        runs (the observed ``status:"cancelled"`` ~1ms after start); or
      * draining the spawned tasks before teardown makes submit BLOCK
        until the job finishes → every candidate serialises → any real
        grid (e.g. alloy_discovery) blows its wall-clock deadline.

    One persistent loop dissolves the dilemma: ``submit`` returns at once,
    ``_run_one`` keeps running on the loop, and independent jobs run
    concurrently in the runner's ``ThreadPoolExecutor``. The sync
    Tool.func contract is honoured by scheduling the coroutine onto the
    loop thread via ``run_coroutine_threadsafe`` and blocking only on the
    *coroutine's own* result (which, for submit, is the fast handle).
    """

    def __init__(self) -> None:
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread: threading.Thread | None = None
        self._lock = threading.Lock()

    def _ensure(self) -> asyncio.AbstractEventLoop:
        # Double-checked init so the loop/thread are created exactly once
        # even under concurrent first calls.
        if self._loop is not None and self._loop.is_running():
            return self._loop
        with self._lock:
            if self._loop is not None and self._loop.is_running():
                return self._loop
            loop = asyncio.new_event_loop()
            thread = threading.Thread(
                target=loop.run_forever,
                name="mace-jobrunner-loop",
                daemon=True,
            )
            thread.start()
            self._loop = loop
            self._thread = thread
            atexit.register(self._close)
            return loop

    def run(self, coro: Any, timeout: float = 300.0) -> Any:
        """Run ``coro`` to completion on the persistent loop and return it.

        For ``JobRunner.submit`` coroutines this returns the JobHandle
        quickly while the spawned ``_run_one`` task continues running on
        this same loop — never cancelled, never serialised.
        """
        loop = self._ensure()
        fut = asyncio.run_coroutine_threadsafe(coro, loop)
        return fut.result(timeout=timeout)

    def _close(self) -> None:
        loop = self._loop
        if loop is None:
            return
        try:
            loop.call_soon_threadsafe(loop.stop)
        except Exception:
            pass


_PERSISTENT_LOOP = _PersistentLoop()


def _run_async(coro: Any) -> Any:
    """Bridge an async primitive call into PRISM's sync Tool.func contract.

    PRISM tool_server.py runs synchronously over stdin/stdout JSON-line;
    the mace primitives are async because JobRunner submits work to a
    thread pool and returns a handle the agent later polls. All such
    coroutines run on a single process-lifetime loop (see
    :class:`_PersistentLoop` for the full rationale) so submit is
    non-blocking and concurrent jobs stay concurrent.
    """
    return _PERSISTENT_LOOP.run(coro)


def _ok_dump(model_obj: Any) -> dict[str, Any]:
    """Pydantic v2 / v1 compat: return a JSON-serialisable dict."""
    if hasattr(model_obj, "model_dump"):
        return model_obj.model_dump(mode="json")
    if hasattr(model_obj, "dict"):
        return model_obj.dict()
    # Fallback for non-pydantic returns (e.g. already a dict).
    return dict(model_obj)


# ===========================================================================
# Primitive tools (5) — submit compute jobs, return JobHandle, agent polls.
# All five are approval-gated because the platform backend spends compute
# credits via marc27 `ml_predict` jobs. Fake/local backends still flow
# through approval for consistency; the bridge dispatches on backend choice.
# ===========================================================================

def _mace_relax_structure(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.primitives import relax_structure
        from app.tools.simulation.mace.schemas import RelaxStructureInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = RelaxStructureInput(**kwargs)
        bridge = get_mace_bridge()
        handle = _run_async(relax_structure(inp, bridge.runner, bridge.backends))
        return _ok_dump(handle)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_relax_structure failed")
        return {"error": str(e), "type": type(e).__name__}


def _mace_md_equilibrate(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.primitives import md_equilibrate
        from app.tools.simulation.mace.schemas import MdEquilibrateInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = MdEquilibrateInput(**kwargs)
        bridge = get_mace_bridge()
        handle = _run_async(md_equilibrate(inp, bridge.runner, bridge.backends))
        return _ok_dump(handle)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_md_equilibrate failed")
        return {"error": str(e), "type": type(e).__name__}


def _mace_phonon_harmonic(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.primitives import phonon_harmonic
        from app.tools.simulation.mace.schemas import PhononHarmonicInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = PhononHarmonicInput(**kwargs)
        bridge = get_mace_bridge()
        handle = _run_async(phonon_harmonic(inp, bridge.runner, bridge.backends))
        return _ok_dump(handle)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_phonon_harmonic failed")
        return {"error": str(e), "type": type(e).__name__}


def _mace_compute_elastic(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.primitives import compute_elastic
        from app.tools.simulation.mace.schemas import ComputeElasticInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = ComputeElasticInput(**kwargs)
        bridge = get_mace_bridge()
        handle = _run_async(compute_elastic(inp, bridge.runner, bridge.backends))
        return _ok_dump(handle)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_compute_elastic failed")
        return {"error": str(e), "type": type(e).__name__}


def _mace_compute_dilute_solute(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.primitives import compute_dilute_solute
        from app.tools.simulation.mace.schemas import ComputeDiluteSoluteInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = ComputeDiluteSoluteInput(**kwargs)
        bridge = get_mace_bridge()
        handle = _run_async(compute_dilute_solute(inp, bridge.runner, bridge.backends))
        return _ok_dump(handle)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_compute_dilute_solute failed")
        return {"error": str(e), "type": type(e).__name__}


# ===========================================================================
# Control-plane tools (5) — synchronous reads against the cache + job store.
# No compute spent → requires_approval=False.
# ===========================================================================

def _mace_estimate_cost(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.control import estimate_cost
        from app.tools.simulation.mace.schemas import EstimateCostInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = EstimateCostInput(**kwargs)
        bridge = get_mace_bridge()
        result = _run_async(estimate_cost(inp, bridge.runner))
        return _ok_dump(result)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_estimate_cost failed")
        return {"error": str(e), "type": type(e).__name__}


def _mace_get_job(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.control import get_job
        from app.tools.simulation.mace.schemas import GetJobInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = GetJobInput(**kwargs)
        bridge = get_mace_bridge()
        result = _run_async(get_job(inp, bridge.runner))
        return _ok_dump(result)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_get_job failed")
        return {"error": str(e), "type": type(e).__name__}


def _mace_list_jobs(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.control import list_jobs
        from app.tools.simulation.mace.schemas import ListJobsInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = ListJobsInput(**kwargs)
        bridge = get_mace_bridge()
        result = _run_async(list_jobs(inp, bridge.runner))
        return _ok_dump(result)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_list_jobs failed")
        return {"error": str(e), "type": type(e).__name__}


def _mace_cancel_job(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.control import cancel_job
        from app.tools.simulation.mace.schemas import CancelJobInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = CancelJobInput(**kwargs)
        bridge = get_mace_bridge()
        result = _run_async(cancel_job(inp, bridge.runner))
        return _ok_dump(result)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_cancel_job failed")
        return {"error": str(e), "type": type(e).__name__}


def _mace_get_cached_structure(**kwargs: Any) -> dict[str, Any]:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.mace.control import get_cached_structure
        from app.tools.simulation.mace.schemas import GetCachedStructureInput
        from app.tools.simulation.mace_bridge import get_mace_bridge

        inp = GetCachedStructureInput(**kwargs)
        bridge = get_mace_bridge()
        result = _run_async(get_cached_structure(inp, bridge.cache))
        return _ok_dump(result)
    except Exception as e:  # noqa: BLE001
        logger.exception("mace_get_cached_structure failed")
        return {"error": str(e), "type": type(e).__name__}


# ===========================================================================
# Shared schema fragments — pulled directly from the pydantic models so the
# JSON Schema fed to the LLM matches the validation surface exactly.
# ===========================================================================

_COMPOSITION_SCHEMA = {
    "type": "object",
    "description": "Composition as element → atomic fraction (must sum to 1.0).",
    "properties": {
        "atoms": {
            "type": "object",
            "patternProperties": {
                "^[A-Z][a-z]?$": {"type": "number", "minimum": 0.0, "maximum": 1.0}
            },
            "additionalProperties": False,
        }
    },
    "required": ["atoms"],
}

_STRUCTURE_REF_SCHEMA = {
    "type": "object",
    "description": (
        "Either a fresh composition+phase OR a cache_ref pointing at a previously "
        "computed CIF. Exactly one of (composition+phase) OR cache_ref must be set."
    ),
    "properties": {
        "composition": _COMPOSITION_SCHEMA,
        "phase": {
            "type": "string",
            "enum": ["bcc", "fcc", "hcp", "b2", "l12", "sigma", "amorphous"],
            "description": "Crystallographic phase to build.",
        },
        "n_atoms": {
            "type": "integer",
            "minimum": 2,
            "maximum": 1000,
            "description": "Total atoms in the supercell.",
        },
        "cache_ref": {
            "type": "string",
            "description": "cache:// URI from a previous tool call.",
        },
    },
}

_PRIMITIVE_OPTIONS_SCHEMA = {
    "type": "object",
    "description": "Per-call execution options.",
    "properties": {
        "backend": {
            "type": "string",
            "enum": ["auto", "fake", "local", "hf_jobs", "platform"],
            "default": "auto",
            "description": (
                "Compute backend selection. 'auto' = heuristic (small + CPU-OK "
                "jobs local; GPU-bound or large jobs prefer 'platform' if "
                "PRISM_PROJECT_ID is set, else 'hf_jobs' if HF_TOKEN set, else "
                "'local'). 'platform' submits to marc27 ml_predict (production "
                "path; supports relax + md tasks today, others fall back). "
                "'hf_jobs' is the open-source HF Pro fallback. 'local' runs "
                "MACE in-process. 'fake' is for tests only."
            ),
        },
        "head": {
            "type": "string",
            "default": "omat_pbe",
            "description": (
                "MACE-MH-1 head selector. Options: omat_pbe (default — inorganic "
                "crystals), omol (molecules), oc20 (surfaces), spice (small molecules), "
                "rgd1 (reactive small-molecule chemistry), mptrj (Materials Project "
                "baseline), matpes (R²SCAN-level inorganic)."
            ),
        },
        "dtype": {"type": "string", "enum": ["float32", "float64"], "default": "float64"},
        "seed": {"type": "integer", "default": 20260506},
        "timeout_seconds": {"type": "integer", "minimum": 1, "default": 3600},
        "progress_token": {"type": "string", "description": "Optional MCP-style progress token."},
    },
}


# ===========================================================================
# Tool registration
# ===========================================================================

# Standard framework-note appended to every primitive's description so the
# LLM agent sees the PyTorch-vs-JAX reality on every selection.
_FRAMEWORK_NOTE = (
    " Framework: PyTorch (MACE-MH-1 is shipped torch-only by upstream as of 2026; "
    "mace-jax does not support the multi-head MH-1 architecture). PRISM's broader "
    "stack is JAX-native by default — prefer a JAX-native MLIP (chgnet, m3gnet, "
    "mace-mp-0, alignn-ff, orb-v3, mattersim) when MH-1's cross-domain coverage "
    "isn't required."
)


def create_mace_tools(registry: ToolRegistry) -> None:
    """Register the 10 MACE tools (5 primitives + 5 control-plane) on the registry."""

    # --- Primitives (approval-gated; may spend compute) -------------------

    registry.register(Tool(
        name="mace_relax_structure",
        description=(
            "Build a supercell from composition + phase and relax it to a local "
            "energy minimum using a MACE foundation interatomic potential. Returns "
            "a JobHandle; poll with mace_get_job. Caches results by canonical "
            "(structure, head, calc_params) so re-asking is free." + _FRAMEWORK_NOTE
        ),
        input_schema={
            "type": "object",
            "properties": {
                "composition": _COMPOSITION_SCHEMA,
                "phase": {"type": "string", "enum": ["bcc", "fcc", "hcp", "b2", "l12", "sigma"]},
                "n_atoms": {"type": "integer", "minimum": 2, "maximum": 1000},
                "fmax_eV_per_A": {"type": "number", "minimum": 0.001, "maximum": 1.0, "default": 0.05},
                "max_steps": {"type": "integer", "minimum": 1, "default": 500},
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": ["composition", "phase", "n_atoms"],
            "additionalProperties": False,
        },
        func=_mace_relax_structure,
        requires_approval=True,
        requires_elicitation=True,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_md_equilibrate",
        description=(
            "Run NVT molecular dynamics on a structure at target temperature "
            "to equilibrate thermal motion. Returns a JobHandle. Use this to "
            "check dynamic stability or to seed phonon / elastic calcs from a "
            "thermally-relaxed configuration." + _FRAMEWORK_NOTE
        ),
        input_schema={
            "type": "object",
            "properties": {
                "composition": _COMPOSITION_SCHEMA,
                "phase": {"type": "string", "enum": ["bcc", "fcc", "hcp", "b2", "l12", "sigma"]},
                "n_atoms": {"type": "integer", "minimum": 2, "maximum": 1000},
                "T_K": {"type": "number", "minimum": 0.1, "maximum": 5000.0},
                "ps": {"type": "number", "minimum": 0.001, "default": 10.0,
                       "description": "Trajectory length in picoseconds."},
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": ["composition", "phase", "n_atoms", "T_K"],
            "additionalProperties": False,
        },
        func=_mace_md_equilibrate,
        requires_approval=True,
        requires_elicitation=True,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_phonon_harmonic",
        description=(
            "Compute the harmonic phonon spectrum via the finite-displacement "
            "method. Returns a JobHandle that resolves to F_vib(T) and the count "
            "of imaginary modes (use this for dynamic-stability screening: "
            "n_imaginary_modes == 0 is necessary but not sufficient for stability)."
            + _FRAMEWORK_NOTE
        ),
        input_schema={
            "type": "object",
            "properties": {
                "composition": _COMPOSITION_SCHEMA,
                "phase": {"type": "string", "enum": ["bcc", "fcc", "hcp", "b2", "l12", "sigma"]},
                "n_atoms": {"type": "integer", "minimum": 2, "maximum": 1000},
                "supercell": {
                    "type": "array",
                    "items": {"type": "integer", "minimum": 1, "maximum": 8},
                    "minItems": 3,
                    "maxItems": 3,
                    "default": [2, 2, 2],
                    "description": "Supercell multipliers [nx, ny, nz] for the phonon calc.",
                },
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": ["composition", "phase", "n_atoms"],
            "additionalProperties": False,
        },
        func=_mace_phonon_harmonic,
        requires_approval=True,
        requires_elicitation=True,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_compute_elastic",
        description=(
            "Compute the second-order elastic-constant tensor via strain-stress "
            "linear fits. Returns a JobHandle resolving to C_ij (Voigt), bulk K, "
            "shear G, Young E, Pugh G/B, and Cauchy-pressure indicators. Use this "
            "for the ductile/brittle screen (JOM-2025 Pugh-G/B threshold)."
            + _FRAMEWORK_NOTE
        ),
        input_schema={
            "type": "object",
            "properties": {
                "composition": _COMPOSITION_SCHEMA,
                "phase": {"type": "string", "enum": ["bcc", "fcc", "hcp", "b2", "l12", "sigma"]},
                "n_atoms": {"type": "integer", "minimum": 2, "maximum": 1000},
                "strain_amplitude": {
                    "type": "number",
                    "minimum": 0.0001,
                    "maximum": 0.05,
                    "default": 0.01,
                },
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": ["composition", "phase", "n_atoms"],
            "additionalProperties": False,
        },
        func=_mace_compute_elastic,
        requires_approval=True,
        requires_elicitation=True,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_compute_dilute_solute",
        description=(
            "Compute the dilute solute formation/substitution energy for a single "
            "solute atom in a matrix supercell. Returns a JobHandle resolving to "
            "Esub plus an interpretive flag (favourable / neutral / unfavourable). "
            "Use this for ISRU substitution screening (e.g. asking 'can we replace "
            "Fe with a lunar-regolith-available element here?')."
            + _FRAMEWORK_NOTE
        ),
        input_schema={
            "type": "object",
            "properties": {
                "matrix": _COMPOSITION_SCHEMA,
                "solute": {
                    "type": "string",
                    "description": "Element symbol of the substitutional solute (e.g. 'Fe').",
                    "pattern": "^[A-Z][a-z]?$",
                },
                "phase": {"type": "string", "enum": ["bcc", "fcc", "hcp", "b2", "l12", "sigma"]},
                "n_atoms": {"type": "integer", "minimum": 8, "maximum": 1000},
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": ["matrix", "solute", "phase", "n_atoms"],
            "additionalProperties": False,
        },
        func=_mace_compute_dilute_solute,
        requires_approval=True,
        requires_elicitation=True,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    # --- Control plane (read-only; no approval needed) --------------------

    registry.register(Tool(
        name="mace_estimate_cost",
        description=(
            "Estimate wall time, GPU seconds, and USD cost for a MACE primitive "
            "before submitting. Read-only — does not launch any job. Use this to "
            "budget-check before calling a primitive."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "tool": {
                    "type": "string",
                    "enum": [
                        "relax_structure",
                        "md_equilibrate",
                        "phonon_harmonic",
                        "compute_elastic",
                        "compute_dilute_solute",
                    ],
                },
                "n_atoms": {"type": "integer", "minimum": 2, "maximum": 1000},
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": ["tool", "n_atoms"],
            "additionalProperties": False,
        },
        func=_mace_estimate_cost,
        requires_approval=False,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_get_job",
        description=(
            "Fetch the current status + result (if ready) of a MACE job by id. "
            "Polls the local SQLite job store; if the job is still running, "
            "returns the latest progress (percent, message, step/total)."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "ULID returned by a primitive call."},
            },
            "required": ["job_id"],
            "additionalProperties": False,
        },
        func=_mace_get_job,
        requires_approval=False,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_list_jobs",
        description=(
            "List MACE jobs in the local job store, filtered by status or tool. "
            "Use to recover from session interruptions or to inventory cache hits."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["queued", "submitted", "running", "succeeded", "failed", "cancelled"],
                },
                "tool_name": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50},
                "offset": {"type": "integer", "minimum": 0, "default": 0},
            },
            "additionalProperties": False,
        },
        func=_mace_list_jobs,
        requires_approval=False,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_cancel_job",
        description=(
            "Cancel a queued or running MACE job. No-op if the job already "
            "succeeded / failed. Safe to call multiple times."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "job_id": {"type": "string"},
            },
            "required": ["job_id"],
            "additionalProperties": False,
        },
        func=_mace_cancel_job,
        requires_approval=False,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_get_cached_structure",
        description=(
            "Resolve a cache:// URI returned by a previous MACE primitive into "
            "the inline CIF text plus its provenance bundle path. Use this when "
            "threading a relaxed structure into a downstream tool (e.g. relax → "
            "compute_elastic via cache_ref)."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "cache_uri": {
                    "type": "string",
                    "pattern": "^cache://",
                    "description": "URI from JobHandle.result.structure_cif_ref.",
                },
            },
            "required": ["cache_uri"],
            "additionalProperties": False,
        },
        func=_mace_get_cached_structure,
        requires_approval=False,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    logger.info("Registered 10 MACE tools (5 primitives + 5 control-plane)")
