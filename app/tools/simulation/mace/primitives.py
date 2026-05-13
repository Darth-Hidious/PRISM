"""The 5 MACE primitive tools.

Every primitive:
  1. Validates input via its pydantic schema.
  2. Picks a backend (heuristic, overrideable).
  3. Computes a cache key from the canonical structure spec + head + params.
  4. Submits a job to ``JobRunner`` and returns a ``JobHandle`` (or an inline
     result if the cache already contains one).

The MCP layer wraps these so the public tool surface matches the schema
names verbatim.
"""

from __future__ import annotations

from typing import Any

from . import __version__ as TOOL_VERSION
from .backends.base import select_backend
from .cache.hashing import cache_key as compute_cache_key
from .cache.hashing import canonical_structure_repr
from .ids import git_sha
from .jobs.runner import JobRunner
from .schemas import (
    ComputeDiluteSoluteInput,
    ComputeElasticInput,
    JobHandle,
    MdEquilibrateInput,
    PhononHarmonicInput,
    RelaxStructureInput,
    StructureRef,
)


def _structure_repr_from_ref(ref: StructureRef, fallback_seed: int) -> dict[str, Any]:
    if ref.cache_ref is not None:
        return {"cache_ref": ref.cache_ref}
    assert ref.composition is not None and ref.phase is not None
    return canonical_structure_repr(
        ref.composition.atoms,
        ref.phase,
        ref.n_atoms,
        fallback_seed,
    )


async def relax_structure(
    inp: RelaxStructureInput,
    runner: JobRunner,
    backends: dict[str, Any],
) -> JobHandle:
    """Build supercell + relax. See :class:`RelaxStructureInput` for params."""
    structure = canonical_structure_repr(
        inp.composition.atoms, inp.phase, inp.n_atoms, inp.options.seed
    )
    calc_params = {
        "dtype": inp.options.dtype,
        "fmax_eV_per_A": inp.fmax_eV_per_A,
        "max_steps": inp.max_steps,
    }
    key = compute_cache_key(
        tool_name="relax_structure",
        tool_version=TOOL_VERSION,
        structure=structure,
        head=inp.options.head,
        calc_params=calc_params,
        mace_core_git_sha=git_sha(),
    )
    backend = select_backend(
        "relax_structure", inp.n_atoms, inp.options.backend, backends
    )
    handle = await runner.submit(
        tool_name="relax_structure",
        input_payload=inp.model_dump(by_alias=False),
        cache_key=key,
        backend_name=backend.name,
        seed=inp.options.seed,
        progress_token=inp.options.progress_token,
        timeout_seconds=inp.options.timeout_seconds,
    )
    return handle


async def compute_elastic(
    inp: ComputeElasticInput,
    runner: JobRunner,
    backends: dict[str, Any],
) -> JobHandle:
    s_repr = _structure_repr_from_ref(inp.structure, inp.options.seed)
    calc_params = {
        "dtype": inp.options.dtype,
        "strain_amplitude": inp.strain_amplitude,
    }
    key = compute_cache_key(
        tool_name="compute_elastic",
        tool_version=TOOL_VERSION,
        structure=s_repr,
        head=inp.options.head,
        calc_params=calc_params,
        mace_core_git_sha=git_sha(),
    )
    n_atoms = inp.structure.n_atoms or 100
    backend = select_backend("compute_elastic", n_atoms, inp.options.backend, backends)
    handle = await runner.submit(
        tool_name="compute_elastic",
        input_payload=inp.model_dump(by_alias=False),
        cache_key=key,
        backend_name=backend.name,
        seed=inp.options.seed,
        progress_token=inp.options.progress_token,
        timeout_seconds=inp.options.timeout_seconds,
    )
    return handle


async def compute_dilute_solute(
    inp: ComputeDiluteSoluteInput,
    runner: JobRunner,
    backends: dict[str, Any],
) -> JobHandle:
    structure = canonical_structure_repr(
        inp.matrix_composition.atoms,
        inp.matrix_phase,
        inp.n_atoms,
        inp.options.seed,
    )
    calc_params = {
        "dtype": inp.options.dtype,
        "solute": inp.solute_element,
        "displaced": inp.displaced_element or "auto",
    }
    key = compute_cache_key(
        tool_name="compute_dilute_solute",
        tool_version=TOOL_VERSION,
        structure=structure,
        head=inp.options.head,
        calc_params=calc_params,
        mace_core_git_sha=git_sha(),
    )
    backend = select_backend(
        "compute_dilute_solute", inp.n_atoms, inp.options.backend, backends
    )
    handle = await runner.submit(
        tool_name="compute_dilute_solute",
        input_payload=inp.model_dump(by_alias=False),
        cache_key=key,
        backend_name=backend.name,
        seed=inp.options.seed,
        progress_token=inp.options.progress_token,
        timeout_seconds=inp.options.timeout_seconds,
    )
    return handle


async def md_equilibrate(
    inp: MdEquilibrateInput,
    runner: JobRunner,
    backends: dict[str, Any],
) -> JobHandle:
    s_repr = _structure_repr_from_ref(inp.structure, inp.options.seed)
    calc_params = {
        "dtype": inp.options.dtype,
        "T_K": inp.T_K,
        "n_steps": inp.n_steps,
        "timestep_fs": inp.timestep_fs,
        "friction_per_fs": inp.friction_per_fs,
        "ensemble": inp.ensemble,
    }
    key = compute_cache_key(
        tool_name="md_equilibrate",
        tool_version=TOOL_VERSION,
        structure=s_repr,
        head=inp.options.head,
        calc_params=calc_params,
        mace_core_git_sha=git_sha(),
    )
    n_atoms = inp.structure.n_atoms or 100
    backend = select_backend("md_equilibrate", n_atoms, inp.options.backend, backends)
    handle = await runner.submit(
        tool_name="md_equilibrate",
        input_payload=inp.model_dump(by_alias=False),
        cache_key=key,
        backend_name=backend.name,
        seed=inp.options.seed,
        progress_token=inp.options.progress_token,
        timeout_seconds=inp.options.timeout_seconds,
    )
    return handle


async def phonon_harmonic(
    inp: PhononHarmonicInput,
    runner: JobRunner,
    backends: dict[str, Any],
) -> JobHandle:
    s_repr = _structure_repr_from_ref(inp.structure, inp.options.seed)
    calc_params = {
        "dtype": inp.options.dtype,
        "displacement_A": inp.displacement_A,
        "q_mesh": list(inp.q_mesh),
        "temperatures_K": list(inp.temperatures_K),
    }
    key = compute_cache_key(
        tool_name="phonon_harmonic",
        tool_version=TOOL_VERSION,
        structure=s_repr,
        head=inp.options.head,
        calc_params=calc_params,
        mace_core_git_sha=git_sha(),
    )
    n_atoms = inp.structure.n_atoms or 100
    backend = select_backend("phonon_harmonic", n_atoms, inp.options.backend, backends)
    handle = await runner.submit(
        tool_name="phonon_harmonic",
        input_payload=inp.model_dump(by_alias=False),
        cache_key=key,
        backend_name=backend.name,
        seed=inp.options.seed,
        progress_token=inp.options.progress_token,
        timeout_seconds=inp.options.timeout_seconds,
    )
    return handle
