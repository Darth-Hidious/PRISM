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


# Persistent background event loop for all MACE coroutines. JobRunner.submit
# schedules the actual work as an asyncio.Task on the CURRENT loop — with the
# old per-call asyncio.run() that loop closed the moment submit returned,
# cancelling every job ("task cancelled") or wedging it at 'running'. One
# long-lived loop thread keeps submitted jobs alive across tool calls.
_LOOP: asyncio.AbstractEventLoop | None = None
_LOOP_LOCK = threading.Lock()


def _ensure_loop() -> asyncio.AbstractEventLoop:
    global _LOOP
    with _LOOP_LOCK:
        if _LOOP is None or _LOOP.is_closed():
            _LOOP = asyncio.new_event_loop()
            threading.Thread(
                target=_LOOP.run_forever, daemon=True, name="mace-event-loop"
            ).start()
        return _LOOP


def _run_async(coro: Any) -> Any:
    """Bridge an async primitive call into PRISM's sync Tool.func contract.

    Runs the coroutine on the persistent MACE event loop so any background
    tasks it spawns (job workers) outlive this tool call. The coroutines
    themselves return fast (submit returns a JobHandle; control-plane calls
    are store reads) — 120 s is a generous ceiling.
    """
    future = asyncio.run_coroutine_threadsafe(coro, _ensure_loop())
    return future.result(timeout=120)


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

        # Flat agent args → nested MdEquilibrateInput. `ps` (picoseconds)
        # converts to n_steps at the model's 1 fs default timestep.
        model_kwargs = _present(kwargs, "T_K", "options")
        if kwargs.get("ps") is not None:
            model_kwargs["n_steps"] = max(
                100, min(int(round(float(kwargs["ps"]) * 1000.0)), 200_000)
            )
        inp = MdEquilibrateInput(
            structure=_structure_ref_from_flat(kwargs), **model_kwargs
        )
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

        inp = PhononHarmonicInput(
            structure=_structure_ref_from_flat(kwargs),
            **_present(kwargs, "displacement_A", "temperatures_K", "q_mesh", "options"),
        )
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

        inp = ComputeElasticInput(
            structure=_structure_ref_from_flat(kwargs),
            **_present(kwargs, "strain_amplitude", "options"),
        )
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

        # Flat agent args (matrix/solute/phase) → model field names.
        inp = ComputeDiluteSoluteInput(
            matrix_composition=kwargs.get("matrix") or kwargs.get("matrix_composition"),
            matrix_phase=kwargs.get("phase") or kwargs.get("matrix_phase") or "bcc",
            solute_element=kwargs.get("solute") or kwargs.get("solute_element"),
            **_present(kwargs, "n_atoms", "displaced_element", "options"),
        )
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

        # Adapt the flat agent-facing schema {tool, n_atoms, options} to
        # EstimateCostInput's {tool_name, arguments} shape.
        inp = EstimateCostInput(
            tool_name=kwargs["tool"],
            arguments={k: v for k, v in kwargs.items() if k != "tool"},
        )
        bridge = get_mace_bridge()
        result = _run_async(estimate_cost(inp, bridge.runner, bridge.backends))
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
        if result is None:
            return {
                "error": f"no MACE job with id {inp.job_id!r}",
                "hint": "use mace_list_jobs to see known job ids",
            }
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

        # Agent-facing 'status' maps to ListJobsInput.status_filter.
        inp = ListJobsInput(
            limit=kwargs.get("limit", 50),
            status_filter=kwargs.get("status"),
            since_iso8601=kwargs.get("since"),
        )
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

        # Accept both spellings: the schema says cache_uri, older callers
        # and the pydantic model use cache_ref.
        ref = kwargs.get("cache_uri") or kwargs.get("cache_ref")
        if not ref:
            return {"error": "mace_get_cached_structure requires `cache_uri`"}
        inp = GetCachedStructureInput(cache_ref=ref)
        bridge = get_mace_bridge()
        result = _run_async(get_cached_structure(inp, bridge.runner))
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
    "description": (
        "Composition as INTEGER ATOM COUNTS per element; the counts must sum "
        "to n_atoms. Example for n_atoms=64: {\"atoms\": {\"Fe\": 54, "
        "\"Al\": 10}}. NOT atomic fractions. Supported elements: Al, Fe, Hf, "
        "Mo, Nb, Ta, Ti, V, W, Zr (refractory/structural set)."
    ),
    "properties": {
        "atoms": {
            "type": "object",
            "description": "Element symbol → integer atom count.",
            "patternProperties": {
                "^[A-Z][a-z]?$": {"type": "integer", "minimum": 0}
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

# Shared property fragments — the same phase/n_atoms appear in 5 primitives;
# define once so every copy carries a description for the model.
_PHASE_SCHEMA = {
    "type": "string",
    "enum": ["bcc", "fcc", "hcp", "b2", "l12", "sigma"],
    "description": "Crystallographic phase of the supercell to build.",
}

_N_ATOMS_SCHEMA = {
    "type": "integer",
    "minimum": 8,
    "maximum": 432,
    "description": (
        "Total atoms in the generated supercell (must equal the sum of the "
        "composition's atom counts)."
    ),
}

_CACHE_REF_SCHEMA = {
    "type": "string",
    "description": (
        "cache:// URI of an existing structure (from structure_import or a "
        "previous relax result's structure_cif_ref). When set, overrides "
        "composition/phase/n_atoms."
    ),
}


def _structure_ref_from_flat(kwargs: dict) -> dict:
    """Build a StructureRef dict from the flat agent-facing arguments.

    The agent-facing schemas stay flat (composition/phase/n_atoms/cache_ref
    at the top level) because the model fills flat schemas far more
    reliably; the pydantic models want a nested StructureRef.
    """
    if kwargs.get("cache_ref"):
        return {"cache_ref": kwargs["cache_ref"]}
    ref: dict[str, Any] = {
        "composition": kwargs.get("composition"),
        "phase": kwargs.get("phase"),
    }
    if kwargs.get("n_atoms") is not None:
        ref["n_atoms"] = kwargs["n_atoms"]
    return ref


def _present(kwargs: dict, *names: str) -> dict:
    """Subset of kwargs for keys that are present and not None."""
    return {n: kwargs[n] for n in names if kwargs.get(n) is not None}

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
                "phase": _PHASE_SCHEMA,
                "n_atoms": _N_ATOMS_SCHEMA,
                "fmax_eV_per_A": {"type": "number", "minimum": 0.001, "maximum": 1.0, "default": 0.05,
                                  "description": "Force-convergence threshold in eV/Å."},
                "max_steps": {"type": "integer", "minimum": 1, "default": 500,
                              "description": "Maximum optimizer steps before giving up."},
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": ["composition", "phase", "n_atoms"],
            "additionalProperties": False,
        },
        func=_mace_relax_structure,
        requires_approval=True,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_md_equilibrate",
        description=(
            "Run NVT molecular dynamics on a structure at target temperature "
            "to equilibrate thermal motion. Returns a JobHandle. Use this to "
            "check dynamic stability or to seed phonon / elastic calcs from a "
            "thermally-relaxed configuration. Provide composition+phase+n_atoms "
            "OR cache_ref (from structure_import / a previous relax)." + _FRAMEWORK_NOTE
        ),
        input_schema={
            "type": "object",
            "properties": {
                "composition": _COMPOSITION_SCHEMA,
                "phase": _PHASE_SCHEMA,
                "n_atoms": _N_ATOMS_SCHEMA,
                "cache_ref": _CACHE_REF_SCHEMA,
                "T_K": {"type": "number", "minimum": 0.1, "maximum": 5000.0,
                        "description": "Target temperature in Kelvin for the NVT run."},
                "ps": {"type": "number", "minimum": 0.1, "maximum": 200.0, "default": 1.0,
                       "description": "Trajectory length in picoseconds (1 fs timestep)."},
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": ["T_K"],
            "additionalProperties": False,
        },
        func=_mace_md_equilibrate,
        requires_approval=True,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_phonon_harmonic",
        description=(
            "Compute the harmonic phonon spectrum via the finite-displacement "
            "method. Returns a JobHandle that resolves to F_vib(T) and the count "
            "of imaginary modes (use this for dynamic-stability screening: "
            "n_imaginary_modes == 0 is necessary but not sufficient for stability). "
            "Provide composition+phase+n_atoms OR cache_ref."
            + _FRAMEWORK_NOTE
        ),
        input_schema={
            "type": "object",
            "properties": {
                "composition": _COMPOSITION_SCHEMA,
                "phase": _PHASE_SCHEMA,
                "n_atoms": _N_ATOMS_SCHEMA,
                "cache_ref": _CACHE_REF_SCHEMA,
                "displacement_A": {
                    "type": "number", "minimum": 0.001, "maximum": 0.1, "default": 0.01,
                    "description": "Finite-displacement amplitude in Angstroms.",
                },
                "temperatures_K": {
                    "type": "array",
                    "items": {"type": "number"},
                    "default": [0.0, 300.0, 1000.0, 1500.0],
                    "description": "Temperatures at which to evaluate F_vib.",
                },
                "q_mesh": {
                    "type": "array",
                    "items": {"type": "integer", "minimum": 1, "maximum": 16},
                    "minItems": 3,
                    "maxItems": 3,
                    "default": [4, 4, 4],
                    "description": "Phonon q-point mesh [nx, ny, nz].",
                },
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": [],
            "additionalProperties": False,
        },
        func=_mace_phonon_harmonic,
        requires_approval=True,
        source="builtin",
        source_detail="app.tools.mace",
    ))

    registry.register(Tool(
        name="mace_compute_elastic",
        description=(
            "Compute the second-order elastic-constant tensor via strain-stress "
            "linear fits. Returns a JobHandle resolving to C_ij (Voigt), bulk K, "
            "shear G, Young E, Pugh G/B, and Cauchy-pressure indicators. Use this "
            "for the ductile/brittle screen (JOM-2025 Pugh-G/B threshold). "
            "Provide composition+phase+n_atoms OR cache_ref."
            + _FRAMEWORK_NOTE
        ),
        input_schema={
            "type": "object",
            "properties": {
                "composition": _COMPOSITION_SCHEMA,
                "phase": _PHASE_SCHEMA,
                "n_atoms": _N_ATOMS_SCHEMA,
                "cache_ref": _CACHE_REF_SCHEMA,
                "strain_amplitude": {
                    "type": "number",
                    "minimum": 0.0001,
                    "maximum": 0.05,
                    "default": 0.005,
                    "description": "Maximum strain amplitude for the stress-strain linear fits.",
                },
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": [],
            "additionalProperties": False,
        },
        func=_mace_compute_elastic,
        requires_approval=True,
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
                "phase": _PHASE_SCHEMA,
                "n_atoms": _N_ATOMS_SCHEMA,
                "displaced_element": {
                    "type": "string",
                    "pattern": "^[A-Z][a-z]?$",
                    "description": (
                        "Element to swap out for the solute. Defaults to the "
                        "most abundant non-solute element."
                    ),
                },
                "options": _PRIMITIVE_OPTIONS_SCHEMA,
            },
            "required": ["matrix", "solute"],
            "additionalProperties": False,
        },
        func=_mace_compute_dilute_solute,
        requires_approval=True,
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
                    "description": "Which MACE primitive to estimate the cost of.",
                },
                "n_atoms": _N_ATOMS_SCHEMA,
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
                    "description": "Only list jobs in this state.",
                },
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50,
                          "description": "Max jobs to return (newest first)."},
                "since": {"type": "string",
                          "description": "Only jobs created after this ISO-8601 timestamp."},
            },
            "required": [],
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
                "job_id": {"type": "string", "description": "ULID of the job to cancel."},
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
