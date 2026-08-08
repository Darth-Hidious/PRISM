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


@pytest.fixture(scope="session", autouse=True)
def isolated_prism_state(tmp_path_factory):
    """Keep the whole suite out of the developer's real ``~/.prism``.

    `SearchEngine.__init__` defaults to
    `HealthManager(persist_path=DEFAULT_HEALTH_PATH)` and
    `SearchCache(disk_dir=DEFAULT_CACHE_DIR)`, both under the real
    `~/.prism/cache/`. `test_fork_safety.py` runs a GENUINE materials search by
    design — the whole point of that file is that a mock does not load the
    framework whose atfork handler is the fault — so a plain `pytest tests/`
    opened circuit breakers in the developer's actual provider-health file. A
    reviewer hit exactly that here and could not restore the prior bytes.

    SESSION scope, deliberately. The first version of this was folded into the
    function-scoped `isolated_state` and did NOT work: `poisoned_process` is
    module-scoped, and higher-scoped fixtures are set up BEFORE lower-scoped
    ones, so the patch was not active when the real search ran. Verified by
    md5 of the real file across a full-suite run — the other search test files
    went clean while `test_fork_safety.py` still wrote it.

    Patched as module attributes because both are read at construction time,
    so this covers every engine built anywhere under test rather than only the
    ones that remember to pass overrides.
    """
    from _pytest.monkeypatch import MonkeyPatch

    from app.tools.search_engine import engine as _search_engine

    mp = MonkeyPatch()
    cache_dir = tmp_path_factory.mktemp("prism-cache")
    mp.setattr(_search_engine, "DEFAULT_CACHE_DIR", cache_dir)
    mp.setattr(
        _search_engine, "DEFAULT_HEALTH_PATH", cache_dir / "provider_health.json"
    )
    yield
    mp.undo()


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


# ---------------------------------------------------------------------------
# MARC27 platform HTTP stubbing
# ---------------------------------------------------------------------------
#
# Every platform tool (`platform_status`, `platform_jobs`,
# `platform_workflows`, `mcp_services`, `knowledge_write`,
# `agent_capabilities`) now goes through `PlatformClient` — auth-header
# selection and URL building live there, and the only socket call is
# `Session.request`. The older per-module stubs (`app.tools.<mod>.requests.get`)
# therefore intercepted nothing: those tests either short-circuited on the
# auth check or escaped to the real network. Stub the client's transport
# instead, so the tool, the client, auth resolution and URL construction
# all run for real.


NOT_CONNECTED = "not connected to the platform"


def assert_not_connected(result):
    """Assert the shared unauthenticated contract: name the cause AND the fix.

    One place to update if the message changes again.
    """
    assert "error" in result, f"expected an error dict, got {result!r}"
    assert NOT_CONNECTED in result["error"], result["error"]
    assert "prism login" in result["error"], result["error"]


class _StubResponse:
    def __init__(self, payload, status_code):
        self._payload = payload
        self.status_code = status_code

    @property
    def text(self):
        import json as _json

        return _json.dumps(self._payload)

    @property
    def content(self):
        return self.text.encode()

    def json(self):
        return self._payload


class PlatformHTTPStub:
    """Records what `PlatformClient` actually put on the wire."""

    def __init__(self):
        self.calls: list[dict] = []
        self.payload = {"ok": True}
        self.status_code = 200

    @property
    def urls(self) -> list:
        return [c["url"] for c in self.calls]

    @property
    def bodies(self) -> list:
        return [c["json"] for c in self.calls]

    def urls_for(self, method: str) -> list:
        return [c["url"] for c in self.calls if c["method"] == method.upper()]


@pytest.fixture
def platform_http(monkeypatch):
    """Fake the socket under `PlatformClient`; assert on real URLs/bodies."""
    import requests

    stub = PlatformHTTPStub()

    def _fake_request(_session, method, url, **kwargs):
        stub.calls.append({
            "method": method.upper(),
            "url": url,
            "json": kwargs.get("json"),
            "params": kwargs.get("params"),
            "headers": kwargs.get("headers") or {},
        })
        return _StubResponse(stub.payload, stub.status_code)

    monkeypatch.setattr(requests.Session, "request", _fake_request)
    return stub


@pytest.fixture(autouse=True)
def _reset_platform_client():
    """`platform()` memoises ONE client per process and resolves credentials
    in its constructor, so a client built under a previous test's environment
    would leak into the next. Reset the singleton around every test."""
    from app.tools import _platform_client

    _platform_client._CLIENT = None
    yield
    _platform_client._CLIENT = None


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
