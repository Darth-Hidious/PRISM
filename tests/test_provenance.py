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
    # OLD ORACLE (wrong): this asserted mace_model == resolve_model() on a
    # FakeBackend run. FakeBackend loads no weights at all — it returns stub
    # values from a lookup table — so that assertion passed only because the
    # builder signed the stub with real MIT MACE weights. It enforced the bug
    # it claimed to guard against. The default-weights half of the oracle is
    # a property of resolve_model() and is asserted directly below; the
    # naming half now guards the correct rule in
    # test_fake_backend_provenance_names_no_weights.
    from app.tools.simulation.mace.core.calculator import resolve_model

    _, _, expected_licence = resolve_model()
    assert expected_licence == "MIT", (
        "the default weights must be MIT-licensed; ASL weights are reachable "
        "only via an explicit MACE_ACCEPT_ASL_LICENSE opt-in"
    )


async def test_fake_backend_provenance_names_no_weights(tmp_path, monkeypatch):
    """A FakeBackend run must not attribute its stub numbers to real weights.

    Regression: provenance.build() called calc_signature() unconditionally, so
    a job executed entirely by the lookup-table FakeBackend (no MACE, no
    torch, no download) emitted mace_model / wasDerivedFrom naming
    ``mace-foundations/mace-mp-0``, ``mace-mp-0b3-medium.model`` and licence
    ``MIT``. calc_signature's own docstring forbids exactly that ("provenance
    that names a model the run did not use is worse than none"), and the
    licence field makes it a licensing claim, not a cosmetic one.
    """
    from app.tools.simulation.mace import auth

    monkeypatch.delenv("MACE_ACCEPT_ASL_LICENSE", raising=False)
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
            raise TimeoutError("fake-weights provenance job did not finish")
        await asyncio.sleep(0.02)

    prov_files = list((tmp_path / "cache").rglob("provenance.json"))
    assert prov_files, "no provenance.json written"
    prov = json.loads(prov_files[0].read_text())

    assert prov["backend"] == "fake"
    model = prov["mace_model"]
    for banned in ("repo_id", "filename", "license"):
        assert banned not in model, (
            f"fake-backend provenance names {banned}={model.get(banned)!r} — "
            "no interatomic potential was loaded"
        )
    assert model["weights"] is None
    assert "fake" in model["weights_absent_reason"]

    # The PROV-O derivation carries the same claim; it must be honest too.
    potential = [
        d for d in prov["wasDerivedFrom"] if d["role"] == "interatomic_potential"
    ][0]
    assert potential.get("weights") is None
    assert "repo_id" not in potential and "license" not in potential


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


def test_real_backend_provenance_still_names_the_resolved_weights(monkeypatch):
    """The honesty fix must not silently blank the weights for real runs.

    Re-homes the two assertions the fake-backend rewrite dropped. The
    original oracle checked ``mace_model["repo_id"]``/``["filename"]``
    against ``resolve_model()``, but did so on a FakeBackend job — which is
    why it enforced the fabrication instead of catching it. The RULE was
    still right for a backend that actually loads a potential, so it is
    asserted here against ``provenance.build`` directly with a non-fake
    backend name. Without this, `if backend == "fake":` could be widened to
    `if True:` and every provenance record in PRISM would lose the weights
    it names with no test failing.
    """
    from app.tools.simulation.mace.core.calculator import resolve_model
    from app.tools.simulation.mace.jobs import provenance as provmod

    monkeypatch.delenv("MACE_ACCEPT_ASL_LICENSE", raising=False)
    monkeypatch.delenv("MACE_MODEL_REPO", raising=False)
    monkeypatch.delenv("MACE_MODEL_FILE", raising=False)
    expected_repo, expected_file, expected_licence = resolve_model()

    prov = provmod.build(
        tool_name="relax_structure",
        job_id="job-test",
        cache_key="key-test",
        input_payload={},
        result_summary={"energy_per_atom_eV": -8.0},
        head="omat_pbe",
        dtype="float64",
        backend="local",
        backend_details={},
        wall_time_s=0.0,
    )

    assert prov["mace_model"]["repo_id"] == expected_repo
    assert prov["mace_model"]["filename"] == expected_file
    assert prov["mace_model"]["license"] == expected_licence
    assert expected_licence == "MIT", (
        "the default weights must be MIT-licensed; ASL weights are reachable "
        "only via an explicit MACE_ACCEPT_ASL_LICENSE opt-in"
    )
    # The PROV-O derivation carries the same names.
    potential = [
        d for d in prov["wasDerivedFrom"] if d["role"] == "interatomic_potential"
    ][0]
    assert potential["repo_id"] == expected_repo
    assert potential["license"] == expected_licence
