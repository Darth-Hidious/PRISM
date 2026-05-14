"""Provenance JSON has all required fields and never leaks HF_TOKEN."""

from __future__ import annotations

import asyncio
import json

from app.tools.simulation.mace.backends import FakeBackend
from app.tools.simulation.mace.jobs import JobRunner, JobStore
from app.tools.simulation.mace.schemas import Composition, PrimitiveOptions, RelaxStructureInput
from app.tools.simulation.mace import primitives as tprim


REQUIRED_FIELDS = {
    "tool_name",
    "tool_version",
    "job_id",
    "cache_key",
    "input",
    "result_summary",
    "mace_model",
    "versions",
    "host",
    "git",
    "backend",
    "wall_time_s",
    "created_at_iso8601",
}


async def test_provenance_written_with_required_fields(tmp_path, monkeypatch):
    monkeypatch.setenv("HF_TOKEN", "hf_fake_token_DO_NOT_LEAK_ME_42")
    from app.tools.simulation.mace import auth

    auth.reset_cache_for_tests()

    store = JobStore(tmp_path / "jobs.db")
    backends = {"fake": FakeBackend()}
    runner = JobRunner(store=store, backends=backends, cache_root=tmp_path / "cache")

    inp = RelaxStructureInput(
        composition=Composition(atoms={"Fe": 50, "Ti": 50}),
        phase="bcc",
        n_atoms=100,
        options=PrimitiveOptions(backend="fake"),
    )
    handle = await tprim.relax_structure(inp, runner, backends)
    # Spin until done
    deadline = asyncio.get_event_loop().time() + 5.0
    while True:
        rec = store.get(handle.job_id)
        if rec and rec.status == "succeeded":
            break
        if asyncio.get_event_loop().time() > deadline:
            raise TimeoutError("provenance test job did not finish")
        await asyncio.sleep(0.02)

    cache_root = tmp_path / "cache"
    # Find the provenance file for whatever cache_key we produced.
    prov_files = list(cache_root.rglob("provenance.json"))
    assert prov_files, "no provenance.json written"
    prov = json.loads(prov_files[0].read_text())
    missing = REQUIRED_FIELDS - set(prov)
    assert not missing, f"missing keys: {missing}"
    assert prov["mace_model"]["repo_id"] == "mace-foundations/mace-mh-1"


async def test_provenance_scrubs_hf_token(tmp_path, monkeypatch):
    sentinel = "hf_fake_token_DO_NOT_LEAK_ME_4242"
    monkeypatch.setenv("HF_TOKEN", sentinel)
    from app.tools.simulation.mace import auth

    auth.reset_cache_for_tests()

    store = JobStore(tmp_path / "jobs.db")
    backends = {"fake": FakeBackend()}
    runner = JobRunner(store=store, backends=backends, cache_root=tmp_path / "cache")

    inp = RelaxStructureInput(
        composition=Composition(atoms={"Fe": 50, "Ti": 50}),
        phase="bcc",
        n_atoms=100,
        options=PrimitiveOptions(backend="fake"),
    )
    handle = await tprim.relax_structure(inp, runner, backends)
    deadline = asyncio.get_event_loop().time() + 5.0
    while True:
        rec = store.get(handle.job_id)
        if rec and rec.status == "succeeded":
            break
        if asyncio.get_event_loop().time() > deadline:
            raise TimeoutError("token-scrub test job did not finish")
        await asyncio.sleep(0.02)

    cache_root = tmp_path / "cache"
    prov_files = list(cache_root.rglob("provenance.json"))
    for p in prov_files:
        text = p.read_text()
        assert sentinel not in text, f"HF_TOKEN leaked into {p}"
