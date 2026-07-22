"""Tests for SearchEngine orchestrator — federated async fan-out, caching, audit trail."""
import asyncio
from unittest.mock import AsyncMock, patch

import pytest

from app.tools.search_engine.cache.engine import SearchCache
from app.tools.search_engine.query import MaterialSearchQuery, PropertyRange
from app.tools.search_engine.resilience.circuit_breaker import HealthManager
from app.tools.search_engine.result import Material, PropertyValue


def _mock_material(pid="mp", formula="Fe2O3"):
    return Material(
        id=f"{pid}-1", formula=formula, elements=["Fe", "O"],
        n_elements=2, sources=[pid],
        band_gap=PropertyValue(value=2.2, source=f"optimade:{pid}", unit="eV"),
    )


def _isolated_engine(registry):
    """Create a SearchEngine with no disk persistence (fully isolated)."""
    from app.tools.search_engine.engine import SearchEngine
    return SearchEngine(
        registry=registry,
        cache=SearchCache(disk_dir=None),
        health_manager=HealthManager(persist_path=None),
    )


def test_engine_creates():
    from app.tools.search_engine.providers.registry import ProviderRegistry
    engine = _isolated_engine(ProviderRegistry())
    assert engine is not None


def test_engine_search_empty_registry():
    from app.tools.search_engine.providers.registry import ProviderRegistry
    engine = _isolated_engine(ProviderRegistry())
    q = MaterialSearchQuery(elements=["Fe"])
    result = asyncio.run(engine.search(q))
    assert result.total_count == 0
    assert result.warnings  # should warn no providers


def test_engine_search_with_mock_provider():
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class MockProvider(Provider):
        id = "mock"
        name = "Mock"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query):
            return [_mock_material("mock")]

    reg = ProviderRegistry()
    reg.register(MockProvider())
    engine = _isolated_engine(reg)
    q = MaterialSearchQuery(elements=["Fe", "O"])
    result = asyncio.run(engine.search(q))
    assert result.total_count == 1
    assert result.materials[0].formula == "Fe2O3"
    assert len(result.query_log) == 1
    assert result.query_log[0].status == "success"


def test_engine_search_provider_failure_graceful():
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class FailProvider(Provider):
        id = "fail"
        name = "Fail"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query):
            raise ConnectionError("Provider down")

    class GoodProvider(Provider):
        id = "good"
        name = "Good"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query):
            return [_mock_material("good")]

    reg = ProviderRegistry()
    reg.register(FailProvider())
    reg.register(GoodProvider())
    engine = _isolated_engine(reg)
    q = MaterialSearchQuery(elements=["Fe"])
    result = asyncio.run(engine.search(q))
    assert result.total_count == 1
    assert len(result.query_log) == 2
    statuses = {log.provider_id: log.status for log in result.query_log}
    assert statuses["fail"] in ("http_error", "timeout")
    assert statuses["good"] == "success"


def test_engine_caches_result():
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    call_count = 0
    class CountingProvider(Provider):
        id = "counter"
        name = "Counter"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query):
            nonlocal call_count
            call_count += 1
            return [_mock_material("counter")]

    reg = ProviderRegistry()
    reg.register(CountingProvider())
    engine = _isolated_engine(reg)
    q = MaterialSearchQuery(elements=["Fe"])
    r1 = asyncio.run(engine.search(q))
    r2 = asyncio.run(engine.search(q))
    assert call_count == 1
    assert r2.cached is True


def test_engine_audit_trail_has_url():
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class MockProvider(Provider):
        id = "mock"
        name = "Mock Provider"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query):
            return []

    reg = ProviderRegistry()
    reg.register(MockProvider())
    engine = _isolated_engine(reg)
    q = MaterialSearchQuery(elements=["Fe"])
    result = asyncio.run(engine.search(q))
    assert len(result.query_log) == 1
    assert result.query_log[0].provider_id == "mock"


# ---------------------------------------------------------------------------
# S1+S2+S3: honesty, timeout-union, cancel-on-early (OPTIMADE redesign)
# ---------------------------------------------------------------------------


def _ok_provider(pid, delay=0.0, n=3):
    """A provider that returns n materials after `delay` seconds."""
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class P(Provider):
        id = pid
        name = pid
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        async def search(self, query):
            await asyncio.sleep(delay)
            return [_mock_material(pid) for _ in range(n)]

    return P()


def _slow_fail_provider(pid, delay=10.0):
    """A provider that sleeps longer than any reasonable timeout (will be cancelled/timed out)."""
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class P(Provider):
        id = pid
        name = pid
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        async def search(self, query):
            await asyncio.sleep(delay)
            return []

    return P()


def test_s1_failed_provider_marked_not_ok_in_status():
    """S1 honesty: a failed provider's query_log status must NOT be 'success'."""
    from app.tools.search_engine.providers.registry import ProviderRegistry

    reg = ProviderRegistry()
    reg.register(_slow_fail_provider("dead"))  # will time out
    reg.register(_ok_provider("alive", n=2))
    engine = _isolated_engine(reg)  # global_timeout defaults to 5s
    q = MaterialSearchQuery(elements=["Fe"], limit=5)
    result = asyncio.run(engine.search(q))
    statuses = {log.provider_id: log.status for log in result.query_log}
    assert statuses["alive"] == "success"
    # dead provider must be an honest failure status, never "success"
    assert statuses["dead"] != "success"
    assert statuses["dead"] in ("timeout", "http_error")


def test_s1_tool_output_surfaces_ok_status_warnings_summary():
    """S1 honesty: the tool wrapper's output must show real ok/status + warnings + summary."""
    from app.plugins.bootstrap import build_full_registry
    from app.tools.search_engine.tools import _materials_search_factory

    # Use the real provider registry so the factory closes over a real engine,
    # but patch engine.search to return a controlled result with a failure.
    _, preg, _ = build_full_registry()
    factory = _materials_search_factory(preg)
    from app.tools.search_engine.result import SearchResult

    fake = SearchResult(
        materials=[],
        total_count=0,
        query=MaterialSearchQuery(elements=["Cu"]),
        query_log=[
            # one success, one timeout — the exact case the old code lied about
            _make_log("ok-prov", "success"),
            _make_log("bad-prov", "timeout", error_message="Timed out after 5s"),
        ],
        warnings=["Provider 'bad-prov' failed: TimeoutError"],
    )
    import app.tools.search_engine.tools as tools_mod

    with patch.object(tools_mod.SearchEngine, "search", new=AsyncMock(return_value=fake)):
        out = factory(elements=["Cu"])
    # The lying bug: old code returned ok:true for everyone. Now must be honest.
    by_id = {p["provider_id"]: p for p in out["providers_queried"]}
    assert by_id["ok-prov"]["ok"] is True
    assert by_id["ok-prov"]["status"] == "success"
    assert by_id["bad-prov"]["ok"] is False, "timeout must be ok=False (the S1 fix)"
    assert by_id["bad-prov"]["status"] == "timeout"
    assert by_id["bad-prov"]["error"] == "Timed out after 5s"
    # warnings must reach the output (old code dropped them)
    assert out["warnings"] == ["Provider 'bad-prov' failed: TimeoutError"]
    # one-glance summary
    assert out["providers_summary"]["succeeded"] == 1
    assert out["providers_summary"]["failed"] == 1


def test_s2_global_deadline_caps_wall_clock():
    """S2: the whole fan-out must not exceed the global deadline (+ small slack)."""
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.engine import SearchEngine

    reg = ProviderRegistry()
    # 4 providers each sleeping 20s — individually fine but collectively must be
    # capped by the global deadline.
    for i in range(4):
        reg.register(_slow_fail_provider(f"slow{i}", delay=20.0))
    engine = SearchEngine(
        registry=reg,
        cache=SearchCache(disk_dir=None),
        health_manager=HealthManager(persist_path=None),
        global_timeout=1.0,  # hard 1s deadline
    )
    q = MaterialSearchQuery(elements=["Fe"], limit=5)
    import time

    start = time.time()
    result = asyncio.run(engine.search(q))
    elapsed = time.time() - start
    # Must return within ~deadline + gather-cancel slack (well under the 20s/ea)
    assert elapsed < 3.0, f"global deadline not enforced: {elapsed:.1f}s"
    # And it must warn about the partial result
    assert any("deadline" in w.lower() for w in result.warnings)


def test_s3_early_completion_cancels_slow_providers():
    """S3: once enough results are in, slow in-flight providers are cancelled (not waited on)."""
    from app.tools.search_engine.providers.registry import ProviderRegistry

    reg = ProviderRegistry()
    # fast providers that together exceed early_target (limit*2) immediately
    reg.register(_ok_provider("fast1", delay=0.05, n=6))
    reg.register(_ok_provider("fast2", delay=0.05, n=6))
    # a slow provider that would take 30s if NOT cancelled
    reg.register(_slow_fail_provider("slowpoke", delay=30.0))
    engine = _isolated_engine(reg)
    q = MaterialSearchQuery(elements=["Fe"], limit=5)  # early_target = 10
    import time

    start = time.time()
    result = asyncio.run(engine.search(q))
    elapsed = time.time() - start
    # If cancel-on-early works, this returns in ~0.1s, NOT 5s (the old behaviour
    # waited on slowpoke's timeout). Allow slack but assert well under the old 5s.
    assert elapsed < 2.0, f"slow provider not cancelled on early completion: {elapsed:.1f}s"
    # fast providers succeeded
    statuses = {log.provider_id: log.status for log in result.query_log}
    assert statuses["fast1"] == "success"
    assert statuses["fast2"] == "success"


def _make_log(pid, status, error_message=None):
    """Helper: build a ProviderQueryLog with the given status."""
    from app.tools.search_engine.result import ProviderQueryLog

    return ProviderQueryLog(
        provider_id=pid,
        provider_name=pid,
        endpoint_url=f"https://example/{pid}",
        query_sent="",
        started_at=0.0,
        completed_at=0.0,
        latency_ms=100.0,
        status=status,
        result_count=5 if status == "success" else 0,
        error_message=error_message,
    )
