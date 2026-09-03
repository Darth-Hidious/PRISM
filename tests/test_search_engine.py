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
    # dead provider must be an honest non-success status, never "success".
    # It sleeps past the fan-out deadline, so the ENGINE ends it: that is
    # "skipped" (our decision), not "timeout" (its answer). The old tuple
    # encoded the misattribution this file has fixed four times over.
    assert statuses["dead"] != "success"
    assert statuses["dead"] in ("timeout", "http_error", "skipped")


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
    # The result says what it is and when it was fetched: the UI's source
    # table is built from these, and a reader must never be left to guess.
    assert out["data_kind"].startswith("materials")
    from datetime import datetime

    fetched = datetime.fromisoformat(out["fetched_at_iso8601"])
    assert fetched.tzinfo is not None, "a fetch time without a zone is not a time"
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


def test_s3_early_cancellation_is_never_charged_to_the_provider():
    """A provider the ENGINE cancelled for sufficiency is not a failed one.

    The cancellation is our decision, so it is no evidence about the provider
    — the same rule the offline-policy branch already applies. Before the fix
    the CancelledError fell into the failure branch: logged status="timeout",
    error_type="CancelledError", error_message="unknown error"
    (str(CancelledError()) is empty), a warning reading "Provider 'slowpoke'
    failed", and record_failure(). Two searches were enough to reach
    consecutive_failures >= 2 and OPEN the circuit of a provider that had done
    nothing wrong, locking it out for the whole 300s cooldown and persisting
    that to provider_health.json.

    Two searches, because that is exactly what it took to open the circuit.
    """
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.engine import SearchEngine

    reg = ProviderRegistry()
    reg.register(_ok_provider("fast1", delay=0.01, n=6))
    reg.register(_ok_provider("fast2", delay=0.01, n=6))
    reg.register(_slow_fail_provider("slowpoke", delay=30.0))
    health = HealthManager(persist_path=None)
    engine = SearchEngine(
        registry=reg,
        cache=SearchCache(disk_dir=None),
        health_manager=health,
    )

    # Distinct queries so neither search is served from the cache.
    for elements in (["Fe"], ["Ni"]):
        result = asyncio.run(
            engine.search(MaterialSearchQuery(elements=elements, limit=5))
        )
        statuses = {log.provider_id: log.status for log in result.query_log}
        assert statuses["slowpoke"] == "skipped", (
            f"cancelled for sufficiency, reported as {statuses['slowpoke']!r}"
        )
        assert not any("slowpoke" in w for w in result.warnings), result.warnings

    h = health.get("slowpoke")
    assert h.failure_count == 0, "our own cancellation must not count as a failure"
    assert h.consecutive_failures == 0
    assert h.circuit_state == "closed"
    assert h.should_query() is True, (
        "a healthy provider must not be locked out by the engine's own "
        "early-termination"
    )


def test_global_deadline_is_never_charged_to_the_providers_it_cancels():
    """The fan-out deadline cancelling in-flight providers is OUR decision.

    Same rule as the early canceller — and the same bug, on the fourth path.
    Before the fix the deadline cancelled tasks without naming them, so each
    CancelledError fell into the failure branch and struck the provider.
    Measured on this machine's provider_health.json: 35 of 53 providers at
    34-36 consecutive failures in lockstep, including hosts with 650+ successes
    at ~200 ms. Independent hosts do not fail together; one deadline struck
    every in-flight task at once, ~34 times, and persisted it.

    Two searches, because two strikes open a circuit for 300 s.
    """
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.engine import SearchEngine

    from types import SimpleNamespace

    reg = ProviderRegistry()
    for i in range(4):
        p = _slow_fail_provider(f"slow{i}", delay=20.0)
        # A per-provider timeout ABOVE the global, so it is the fan-out
        # deadline — not the provider's own — that ends these tasks.
        p._endpoint = SimpleNamespace(behavior=SimpleNamespace(timeout_ms=20_000))
        reg.register(p)
    health = HealthManager(persist_path=None)
    engine = SearchEngine(
        registry=reg,
        cache=SearchCache(disk_dir=None),
        health_manager=health,
        global_timeout=0.5,
    )
    for elements in (["Fe"], ["Ni"]):
        result = asyncio.run(
            engine.search(MaterialSearchQuery(elements=elements, limit=5))
        )
        statuses = {log.provider_id: log.status for log in result.query_log}
        messages = {log.provider_id: log.error_message for log in result.query_log}
        for i in range(4):
            assert statuses[f"slow{i}"] == "skipped", (
                f"cancelled by our deadline, reported as {statuses[f'slow{i}']!r}"
            )
            assert "deadline" in (messages[f"slow{i}"] or ""), (
                "the log must say it was our deadline, not 'enough results': "
                f"{messages[f'slow{i}']!r}"
            )
    for i in range(4):
        h = health.get(f"slow{i}")
        assert h.failure_count == 0, "our own deadline must not count as a failure"
        assert h.consecutive_failures == 0
        assert h.circuit_state == "closed"
        assert h.should_query() is True


def test_per_provider_timeout_is_never_charged_to_the_provider():
    """A provider's own timeout expiring is our deadline, not its answer.

    Before the fix this path called record_failure(): OQMD, whose recorded
    mean SUCCESS latency is 3.5 s, was struck whenever its tail crossed the
    8 s default, and two searches opened its circuit. A slow host is not a
    dead one; only a failure that reached the endpoint and came back may
    strike it. A genuinely hung host still costs at most the global deadline,
    in parallel with everyone else.
    """
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.engine import SearchEngine

    from types import SimpleNamespace

    class Tail(Provider):
        id = "tail"
        name = "tail"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        async def search(self, query):
            await asyncio.sleep(5.0)
            return []

    tail = Tail()
    # The provider's OWN timeout, well below the global, so its deadline —
    # not the fan-out's — is the one that expires.
    tail._endpoint = SimpleNamespace(behavior=SimpleNamespace(timeout_ms=200))
    reg = ProviderRegistry()
    reg.register(tail)
    health = HealthManager(persist_path=None)
    engine = SearchEngine(
        registry=reg,
        cache=SearchCache(disk_dir=None),
        health_manager=health,
        global_timeout=3.0,
    )
    for elements in (["Fe"], ["Ni"]):
        result = asyncio.run(
            engine.search(MaterialSearchQuery(elements=elements, limit=5))
        )
        statuses = {log.provider_id: log.status for log in result.query_log}
        assert statuses["tail"] == "timeout"
    h = health.get("tail")
    assert h.failure_count == 0, "a timeout is our deadline, not the provider's answer"
    assert h.consecutive_failures == 0
    assert h.circuit_state == "closed"
    assert h.should_query() is True


def test_engine_records_providers_own_query_description():
    """The audit trail records each provider's OWN intended query
    (describe_query), not a blanket OPTIMADE translation."""
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class NativeProvider(Provider):
        id = "native"
        name = "Native"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        def describe_query(self, query):
            return "NATIVE-DSL elements=Fe"

        async def search(self, query):
            return [_mock_material("native")]

    reg = ProviderRegistry()
    reg.register(NativeProvider())
    engine = _isolated_engine(reg)
    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"])))
    assert result.query_log[0].query_description == "NATIVE-DSL elements=Fe"


def test_engine_describe_query_failure_never_fails_the_provider_query():
    """Item 6: describe_query is audit formatting. A provider whose
    description code RAISES must still be queried and logged as success,
    with an explicit failure marker in query_description -- never an
    operational failure, never an escaped exception."""
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class BrokenDescribeProvider(Provider):
        id = "broken_describe"
        name = "BrokenDescribe"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        def describe_query(self, query):
            raise RuntimeError("audit formatting blew up")

        async def search(self, query):
            return [_mock_material("bd")]

    reg = ProviderRegistry()
    reg.register(BrokenDescribeProvider())
    engine = _isolated_engine(reg)
    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"])))
    log = result.query_log[0]
    assert log.status == "success"  # the search itself was unaffected
    assert log.result_count == 1
    assert "describe_query failed" in log.query_description


def test_engine_describe_query_non_str_return_never_fails_the_provider_query():
    """The guard must catch non-str RETURNS, not only raised exceptions: a
    describe_query returning None or a coroutine used to pass the guard and
    then blow up ProviderQueryLog(query_description=...) validation AFTER a
    successful search -- defeating the guard's whole purpose. The search
    stays a success; the marker names the wrong type."""
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class NoneDescribeProvider(Provider):
        id = "none_describe"
        name = "NoneDescribe"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        def describe_query(self, query):
            return None  # broken adapter: forgot to return the string

        async def search(self, query):
            return [_mock_material("nd")]

    class CoroDescribeProvider(Provider):
        id = "coro_describe"
        name = "CoroDescribe"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        async def describe_query(self, query):  # accidentally async
            return "never awaited"

        async def search(self, query):
            return [_mock_material("cd")]

    reg = ProviderRegistry()
    reg.register(NoneDescribeProvider())
    reg.register(CoroDescribeProvider())
    engine = _isolated_engine(reg)
    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"])))
    logs = {log.provider_id: log for log in result.query_log}
    assert logs["none_describe"].status == "success"
    assert logs["none_describe"].result_count == 1
    assert "NoneType" in logs["none_describe"].query_description
    assert "not str" in logs["none_describe"].query_description
    assert logs["coro_describe"].status == "success"
    assert logs["coro_describe"].result_count == 1
    assert "not str" in logs["coro_describe"].query_description


def _make_log(pid, status, error_message=None):
    """Helper: build a ProviderQueryLog with the given status."""
    from app.tools.search_engine.result import ProviderQueryLog

    return ProviderQueryLog(
        provider_id=pid,
        provider_name=pid,
        endpoint_url=f"https://example/{pid}",
        query_description="",
        started_at=0.0,
        completed_at=0.0,
        latency_ms=100.0,
        status=status,
        result_count=5 if status == "success" else 0,
        error_message=error_message,
    )


# ---------------------------------------------------------------------------
# S7: capability-aware coverage + client-side post-filter
# ---------------------------------------------------------------------------


def test_s7_post_filter_drops_materials_outside_property_range():
    """S7: a band_gap range must narrow results client-side (OPTIMADE providers
    can't filter on it server-side, so we do it locally and report it)."""
    from app.tools.search_engine.engine import _post_filter_client_side
    from app.tools.search_engine.result import Material, PropertyValue

    mats = [
        Material(id="a", formula="A", elements=["A"], n_elements=1, sources=["x"],
                 band_gap=PropertyValue(value=0.2, source="x")),   # below range -> drop
        Material(id="b", formula="B", elements=["B"], n_elements=1, sources=["x"],
                 band_gap=PropertyValue(value=1.5, source="x")),   # in range -> keep
        Material(id="c", formula="C", elements=["C"], n_elements=1, sources=["x"],
                 band_gap=PropertyValue(value=5.0, source="x")),   # above range -> drop
        Material(id="d", formula="D", elements=["D"], n_elements=1, sources=["x"]),  # missing -> drop
    ]
    q = MaterialSearchQuery(elements=["Cu"], band_gap=PropertyRange(min=0.5, max=3.0))
    out = _post_filter_client_side(mats, q)
    kept_ids = {m.id for m in out}
    assert kept_ids == {"b"}, f"only the in-range material survives; got {kept_ids}"


def test_s7_coverage_reports_filter_strength_and_post_filter():
    """S7: the coverage block surfaces the strongest server-side filter + whether
    property filters were applied client-side."""
    from app.tools.search_engine.engine import _coverage_for_query

    # element + band_gap: strength is 'element' (server), band_gap is client-side
    cov = _coverage_for_query(
        MaterialSearchQuery(elements=["Cu"], band_gap=PropertyRange(min=0.5, max=3.0))
    )
    assert cov["filter_strength"] == "element"
    assert "band_gap" in cov["property_filters_present"]
    assert "atomgpt" in cov["providers_supporting_property_filter"]

    # formula only: strength is 'formula', no property filters
    cov2 = _coverage_for_query(MaterialSearchQuery(formula="Cu2O"))
    assert cov2["filter_strength"] == "formula"
    assert cov2["property_filters_present"] == []


def test_s7_coverage_in_engine_output():
    """S7: a search with a property range returns the coverage block, and
    out-of-range materials are dropped client-side (honestly reported)."""
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    bg = PropertyValue(value=0.1, source="x")  # OUT of the [0.5, 3.0] range

    class P(Provider):
        id = "mock"
        name = "Mock"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        async def search(self, query):
            return [
                Material(id="out", formula="A", elements=["A"], n_elements=1,
                         sources=["mock"], band_gap=bg),
                Material(id="ok", formula="B", elements=["B"], n_elements=1,
                         sources=["mock"],
                         band_gap=PropertyValue(value=1.5, source="x")),
            ]

    reg = ProviderRegistry()
    reg.register(P())
    engine = _isolated_engine(reg)
    q = MaterialSearchQuery(elements=["Cu"], band_gap=PropertyRange(min=0.5, max=3.0))
    result = asyncio.run(engine.search(q))
    # Only the in-range material survives client-side post-filter
    assert result.total_count == 1
    assert result.materials[0].id == "ok"
    # Coverage honestly reports client-side filtering happened
    assert result.coverage["filter_strength"] == "element"
    assert "band_gap" in result.coverage["property_filters_present"]
    assert result.coverage["client_side_post_filtered"] is True
    assert result.coverage["dropped_by_post_filter"] == 1

