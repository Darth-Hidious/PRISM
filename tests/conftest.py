"""Shared pytest fixtures.

Forces FakeBackend everywhere by default and routes state to a per-test
temp directory so tests are hermetic and parallel-safe.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

# Make ``mace_core`` and ``mace_mcp`` importable when running tests from a
# source checkout (no install needed).
ROOT = Path(__file__).resolve().parent.parent
for p in (ROOT, ROOT / "src"):
    sp = str(p)
    if sp not in sys.path:
        sys.path.insert(0, sp)


@pytest.fixture(autouse=True)
def isolated_state(tmp_path, monkeypatch):
    """Redirect cache + state to ``tmp_path``; force FakeBackend; no HF_TOKEN."""
    state = tmp_path / "state"
    cache = state / "cache"
    state.mkdir(parents=True, exist_ok=True)
    cache.mkdir(parents=True, exist_ok=True)
    monkeypatch.setenv("MACE_MCP_STATE_DIR", str(state))
    monkeypatch.setenv("MACE_MCP_CACHE_DIR", str(cache))
    monkeypatch.setenv("MACE_MCP_BACKEND", "fake")
    # No env file leak
    monkeypatch.setenv("MACE_MCP_ENV_FILE", str(tmp_path / "nonexistent.env"))
    # No real token
    monkeypatch.delenv("HF_TOKEN", raising=False)
    # Reset the auth module's cache after the env change
    from app.tools.simulation.mace import auth

    auth.reset_cache_for_tests()
    yield
    auth.reset_cache_for_tests()


@pytest.fixture
def runner_factory():
    """Build a fresh ``JobRunner`` against ``FakeBackend``."""
    from app.tools.simulation.mace.backends import FakeBackend
    from app.tools.simulation.mace.jobs import JobRunner, JobStore

    def _make(tmp_path):
        store = JobStore(tmp_path / "jobs.db")
        backends = {"fake": FakeBackend()}
        runner = JobRunner(store=store, backends=backends, cache_root=tmp_path / "cache")
        return runner, store, backends

    return _make


def pytest_collection_modifyitems(config, items):
    """Skip ``live`` tests unless ``MACE_MCP_LIVE=1`` is set."""
    if os.environ.get("MACE_MCP_LIVE") == "1":
        return
    skip_live = pytest.mark.skip(reason="set MACE_MCP_LIVE=1 to run live HF Jobs tests")
    for item in items:
        if "live" in item.keywords:
            item.add_marker(skip_live)
