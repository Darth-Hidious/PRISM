"""Control-plane tools: get_job, cancel_job, list_jobs, estimate_cost,
get_cached_structure.

All synchronous (no job creation). They read SQLite + the disk cache.
"""

from __future__ import annotations

from typing import Any

from .cache import CacheStore
from .cache.hashing import (
    cache_key as compute_cache_key,
    canonical_structure_repr,
    parse_cache_uri,
)
from .ids import git_sha
from .jobs.runner import JobRunner
from .schemas import (
    CancelJobInput,
    CancelJobResult,
    EstimateCostInput,
    EstimateCostResult,
    GetCachedStructureInput,
    GetCachedStructureResult,
    GetJobInput,
    JobRecord,
    ListJobsInput,
    ListJobsResult,
)
from . import __version__ as TOOL_VERSION


# Cost model (seconds, gpu-seconds, usd) keyed by tool + N-atoms tier.
# Seeded from the *_run.log files in phase_diagrams/.
# Format: (wall_s, gpu_s, usd) for L4×1 baseline ($1.80/h ≈ $0.0005/s).
_COST_MODEL: dict[str, dict[str, tuple[int, int, float]]] = {
    "relax_structure": {
        "tiny": (60, 0, 0.00),         # local CPU, N≤30
        "small": (240, 240, 0.12),     # HF Jobs L4×1, N≤100
        "medium": (900, 900, 0.45),    # HF Jobs L4×1, N≤216
        "large": (1800, 1800, 0.90),   # HF Jobs L4×1, N>216
    },
    "compute_elastic": {
        "tiny": (300, 0, 0.00),
        "small": (1200, 1200, 0.60),
        "medium": (2400, 2400, 1.20),
        "large": (3600, 3600, 1.80),
    },
    "compute_dilute_solute": {
        "tiny": (300, 0, 0.00),
        "small": (1200, 1200, 0.60),
        "medium": (2700, 2700, 1.35),
        "large": (5400, 5400, 2.70),
    },
    "md_equilibrate": {
        "tiny": (120, 0, 0.00),
        "small": (600, 600, 0.30),
        "medium": (1800, 1800, 0.90),
        "large": (3600, 3600, 1.80),
    },
    "phonon_harmonic": {
        "tiny": (1800, 1800, 0.90),
        "small": (3600, 3600, 1.80),
        "medium": (5400, 5400, 2.70),
        "large": (9000, 9000, 4.50),
    },
}


def _size_tier(n_atoms: int) -> str:
    if n_atoms <= 30:
        return "tiny"
    if n_atoms <= 100:
        return "small"
    if n_atoms <= 216:
        return "medium"
    return "large"


# ----------------------------------------------------------------------

async def get_job(inp: GetJobInput, runner: JobRunner) -> JobRecord | None:
    return runner.store.get(inp.job_id)


async def cancel_job(inp: CancelJobInput, runner: JobRunner) -> CancelJobResult:
    new_status = await runner.cancel(inp.job_id)
    rec = runner.store.get(inp.job_id)
    if rec is None:
        return CancelJobResult(job_id=inp.job_id, status="cancelled", message="unknown job")
    return CancelJobResult(
        job_id=inp.job_id, status=rec.status, message=new_status
    )


async def list_jobs(inp: ListJobsInput, runner: JobRunner) -> ListJobsResult:
    rows = runner.store.list(
        limit=inp.limit,
        status_filter=inp.status_filter,
        since_iso=inp.since_iso8601,
    )
    return ListJobsResult(jobs=rows)


async def estimate_cost(
    inp: EstimateCostInput,
    runner: JobRunner,
    backends: dict[str, Any],
) -> EstimateCostResult:
    args = inp.arguments
    tool = inp.tool_name
    # Extract N_atoms heuristically.
    n_atoms = args.get("n_atoms")
    if n_atoms is None and "structure" in args:
        s = args["structure"]
        n_atoms = s.get("n_atoms")
    if n_atoms is None:
        n_atoms = 100
    tier = _size_tier(int(n_atoms))
    wall_s, gpu_s, usd = _COST_MODEL[tool][tier]

    # Check the cache.
    cache_hit = False
    try:
        # Build the same cache key the primitive would compute.
        if tool == "relax_structure":
            structure = canonical_structure_repr(
                args["composition"]["atoms"],
                args.get("phase", "bcc"),
                int(n_atoms),
                args.get("options", {}).get("seed", 20260506),
            )
            calc_params = {
                "dtype": args.get("options", {}).get("dtype", "float64"),
                "fmax_eV_per_A": args.get("fmax_eV_per_A", 0.05),
                "max_steps": args.get("max_steps", 200),
            }
        else:
            # For non-relax tools, recompute the key with whatever structure spec
            # we have. Cache-hit detection here is best-effort.
            structure = args.get("structure", {})
            calc_params = {"dtype": args.get("options", {}).get("dtype", "float64")}
        key = compute_cache_key(
            tool_name=tool,
            tool_version=TOOL_VERSION,
            structure=structure,
            head=args.get("options", {}).get("head", "omat_pbe"),
            calc_params=calc_params,
            mace_core_git_sha=git_sha(),
        )
        cs = CacheStore(runner.cache.root)
        cache_hit = cs.has_result(key)
    except Exception:
        cache_hit = False

    backend_rec = "fake"
    if "hf_jobs" in backends:
        if tool in {"phonon_harmonic", "md_equilibrate", "compute_elastic"} or int(n_atoms) > 30:
            backend_rec = "hf_jobs"
        else:
            backend_rec = "local" if "local" in backends else "hf_jobs"
    elif "local" in backends:
        backend_rec = "local"

    if cache_hit:
        return EstimateCostResult(
            estimated_wall_seconds=0,
            estimated_gpu_seconds=0,
            estimated_usd=0.0,
            backend_recommended=backend_rec,
            cache_hit=True,
            notes="cache hit; no compute needed",
        )
    return EstimateCostResult(
        estimated_wall_seconds=wall_s,
        estimated_gpu_seconds=gpu_s,
        estimated_usd=usd,
        backend_recommended=backend_rec,
        cache_hit=False,
        notes=f"size_tier={tier}",
    )


async def get_cached_structure(
    inp: GetCachedStructureInput,
    runner: JobRunner,
) -> GetCachedStructureResult:
    key, _kind = parse_cache_uri(inp.cache_ref)
    cif = runner.cache.read_structure_cif(key)
    if cif is None:
        raise ValueError(f"no cached structure for {inp.cache_ref!r}")
    meta = runner.cache.read_meta(key) or {}
    return GetCachedStructureResult(
        cif_text=cif,
        n_atoms=int(meta.get("n_atoms", 0)),
        composition=meta.get("composition") or {},
        phase=meta.get("phase"),
        head=meta.get("head"),
        source_job_id=meta.get("source_job_id"),
        created_at=meta.get("created_at"),
    )
