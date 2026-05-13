"""End-to-end primitive tests against the FakeBackend, through the runner."""

from __future__ import annotations

import asyncio

import pytest

from app.tools.simulation.mace.backends import FakeBackend
from app.tools.simulation.mace.jobs import JobRunner, JobStore
from app.tools.simulation.mace.schemas import (
    Composition,
    ComputeDiluteSoluteInput,
    ComputeElasticInput,
    MdEquilibrateInput,
    PhononHarmonicInput,
    PrimitiveOptions,
    RelaxStructureInput,
    StructureRef,
)
from app.tools.simulation.mace import primitives as tprim


def _new_runner(tmp_path):
    store = JobStore(tmp_path / "jobs.db")
    backends = {"fake": FakeBackend()}
    runner = JobRunner(store=store, backends=backends, cache_root=tmp_path / "cache")
    return runner, store, backends


async def _wait_until(runner, store, job_id, *, timeout=5.0):
    deadline = asyncio.get_event_loop().time() + timeout
    while True:
        rec = store.get(job_id)
        if rec and rec.status in {"succeeded", "failed", "cancelled"}:
            return rec
        if asyncio.get_event_loop().time() > deadline:
            raise TimeoutError(f"job {job_id} did not finish in {timeout}s")
        await asyncio.sleep(0.02)


async def test_relax_fake_end_to_end(tmp_path):
    runner, store, backends = _new_runner(tmp_path)
    inp = RelaxStructureInput(
        composition=Composition(atoms={"Fe": 50, "Ti": 50}),
        phase="bcc",
        n_atoms=100,
        options=PrimitiveOptions(backend="fake"),
    )
    handle = await tprim.relax_structure(inp, runner, backends)
    assert handle.status in {"queued", "succeeded"}
    rec = await _wait_until(runner, store, handle.job_id)
    assert rec.status == "succeeded"
    assert rec.result["energy_per_atom_eV"] < 0
    assert rec.result["structure_cif_ref"].startswith("cache://")
    assert rec.provenance_ref is not None


async def test_relax_cache_hit_inline(tmp_path):
    runner, store, backends = _new_runner(tmp_path)
    inp = RelaxStructureInput(
        composition=Composition(atoms={"Fe": 50, "Ti": 50}),
        phase="bcc",
        n_atoms=100,
        options=PrimitiveOptions(backend="fake"),
    )
    handle1 = await tprim.relax_structure(inp, runner, backends)
    await _wait_until(runner, store, handle1.job_id)
    handle2 = await tprim.relax_structure(inp, runner, backends)
    # Same inputs -> same cache key -> immediate inline success.
    assert handle2.cache_hit is True
    assert handle2.status == "succeeded"
    assert handle2.result["energy_per_atom_eV"] == pytest.approx(
        handle1.result["energy_per_atom_eV"] if handle1.result
        else store.get(handle1.job_id).result["energy_per_atom_eV"]
    )


async def test_compute_elastic_end_to_end(tmp_path):
    runner, store, backends = _new_runner(tmp_path)
    inp = ComputeElasticInput(
        structure=StructureRef(
            composition=Composition(atoms={"Mo": 25, "Nb": 25, "Ta": 25, "V": 25}),
            phase="bcc",
            n_atoms=100,
        ),
        options=PrimitiveOptions(backend="fake"),
    )
    handle = await tprim.compute_elastic(inp, runner, backends)
    rec = await _wait_until(runner, store, handle.job_id)
    assert rec.status == "succeeded"
    assert "pugh_G_over_B" in rec.result
    assert rec.result["pugh_verdict"] in {"ductile", "brittle"}


async def test_md_writes_traj_ref(tmp_path):
    runner, store, backends = _new_runner(tmp_path)
    inp = MdEquilibrateInput(
        structure=StructureRef(
            composition=Composition(atoms={"Fe": 50, "Ti": 50}),
            phase="bcc",
            n_atoms=100,
        ),
        T_K=1500.0,
        n_steps=200,
        options=PrimitiveOptions(backend="fake"),
    )
    handle = await tprim.md_equilibrate(inp, runner, backends)
    rec = await _wait_until(runner, store, handle.job_id)
    assert rec.status == "succeeded"
    assert rec.result["traj_ref"].startswith("cache://")
    assert rec.result["is_dynamically_stable"] is True


async def test_phonon_returns_fvib_per_temp(tmp_path):
    runner, store, backends = _new_runner(tmp_path)
    inp = PhononHarmonicInput(
        structure=StructureRef(
            composition=Composition(atoms={"Mo": 50, "Nb": 50}),
            phase="bcc",
            n_atoms=100,
        ),
        temperatures_K=[0.0, 300.0, 1000.0, 1500.0],
        options=PrimitiveOptions(backend="fake"),
    )
    handle = await tprim.phonon_harmonic(inp, runner, backends)
    rec = await _wait_until(runner, store, handle.job_id)
    assert rec.status == "succeeded"
    assert len(rec.result["F_vib_eV_per_atom"]) == 4
    assert rec.result["n_imaginary_modes"] == 0


async def test_compute_dilute_solute(tmp_path):
    runner, store, backends = _new_runner(tmp_path)
    inp = ComputeDiluteSoluteInput(
        matrix_composition=Composition(atoms={"Mo": 25, "Nb": 25, "Ta": 25, "V": 25}),
        matrix_phase="bcc",
        n_atoms=100,
        solute_element="Fe",
        options=PrimitiveOptions(backend="fake"),
    )
    handle = await tprim.compute_dilute_solute(inp, runner, backends)
    rec = await _wait_until(runner, store, handle.job_id)
    assert rec.status == "succeeded"
    assert rec.result["solute_element"] == "Fe"
    assert rec.result["displaced_element"] in {"Mo", "Nb", "Ta", "V"}
