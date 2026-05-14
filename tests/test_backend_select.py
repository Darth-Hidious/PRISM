"""Backend selection heuristic."""

from __future__ import annotations

from app.tools.simulation.mace.backends import FakeBackend, LocalBackend
from app.tools.simulation.mace.backends.base import select_backend


def _backends(*names: str) -> dict:
    objs = {"fake": FakeBackend(), "local": LocalBackend(), "hf_jobs": _DummyHf()}
    return {n: objs[n] for n in names}


class _DummyHf:
    name = "hf_jobs"

    def execute(self, *a, **kw):  # pragma: no cover - never called in this test
        raise NotImplementedError

    def cancel(self, *a, **kw):
        pass


def test_env_override_wins(monkeypatch):
    monkeypatch.setenv("MACE_MCP_BACKEND", "fake")
    from app.tools.simulation.mace import auth

    auth.reset_cache_for_tests()
    backends = _backends("fake", "local", "hf_jobs")
    b = select_backend("relax_structure", n_atoms=100, requested="hf_jobs", backends=backends)
    assert b.name == "fake"


def test_caller_override_honoured(monkeypatch):
    monkeypatch.delenv("MACE_MCP_BACKEND", raising=False)
    from app.tools.simulation.mace import auth

    auth.reset_cache_for_tests()
    backends = _backends("fake", "local", "hf_jobs")
    b = select_backend("relax_structure", n_atoms=100, requested="local", backends=backends)
    assert b.name == "local"


def test_auto_gpu_bound_prefers_hf(monkeypatch):
    monkeypatch.delenv("MACE_MCP_BACKEND", raising=False)
    from app.tools.simulation.mace import auth

    auth.reset_cache_for_tests()
    backends = _backends("fake", "local", "hf_jobs")
    b = select_backend("phonon_harmonic", n_atoms=16, requested="auto", backends=backends)
    assert b.name == "hf_jobs"


def test_auto_small_n_prefers_local(monkeypatch):
    monkeypatch.delenv("MACE_MCP_BACKEND", raising=False)
    from app.tools.simulation.mace import auth

    auth.reset_cache_for_tests()
    backends = _backends("fake", "local", "hf_jobs")
    b = select_backend("relax_structure", n_atoms=16, requested="auto", backends=backends)
    assert b.name == "local"


def test_auto_large_n_prefers_hf(monkeypatch):
    monkeypatch.delenv("MACE_MCP_BACKEND", raising=False)
    from app.tools.simulation.mace import auth

    auth.reset_cache_for_tests()
    backends = _backends("fake", "local", "hf_jobs")
    b = select_backend("relax_structure", n_atoms=100, requested="auto", backends=backends)
    assert b.name == "hf_jobs"


def test_fallback_to_fake(monkeypatch):
    monkeypatch.delenv("MACE_MCP_BACKEND", raising=False)
    from app.tools.simulation.mace import auth

    auth.reset_cache_for_tests()
    backends = _backends("fake")
    b = select_backend("relax_structure", n_atoms=100, requested="auto", backends=backends)
    assert b.name == "fake"
