"""Partial is a third state, not a flavour of success.

Pins the completeness accounting of the federated search_engine:

- OPTIMADE pagination follows ``links.next`` instead of dressing the first
  page up as the whole answer, and writes ``pages_fetched``/``truncated``/
  ``available`` from measured values (they had ZERO writers before).
- A provider skipped by an open circuit breaker produces a ``circuit_open``
  query-log entry (the status was counted in tools.py but had ZERO
  producers, so the summary was structurally always 0).
- ``parse_error`` has a real producer, ``error_raw`` is written, and
  ``http_status_code`` carries the REAL wire status (503 on a 503) instead
  of a fabricated literal 200 on success and null on failure.
- A partial fan-out is cached for minutes, not pinned for 24 hours.
"""
from __future__ import annotations

import asyncio
import json

import httpx
import pytest

from app.tools.search_engine.cache.engine import SearchCache
from app.tools.search_engine.engine import PARTIAL_RESULT_TTL_SECONDS, SearchEngine
from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
from app.tools.search_engine.providers.endpoint import (
    BehaviorConfig,
    CapabilitiesConfig,
    ProviderEndpoint,
)
from app.tools.search_engine.providers.optimade import OptimadeProvider
from app.tools.search_engine.providers.registry import ProviderRegistry
from app.tools.search_engine.query import MaterialSearchQuery
from app.tools.search_engine.resilience.circuit_breaker import HealthManager
from app.tools.search_engine.result import Material, ProviderPage


# ---------------------------------------------------------------------------
# Shared plumbing
# ---------------------------------------------------------------------------


def _material(pid: str, n: int) -> Material:
    return Material(
        id=f"{pid}-{n}", formula="Fe2O3", elements=["Fe", "O"],
        n_elements=2, sources=[pid],
    )


def _isolated_engine(registry: ProviderRegistry) -> SearchEngine:
    return SearchEngine(
        registry=registry,
        cache=SearchCache(disk_dir=None),
        health_manager=HealthManager(persist_path=None),
    )


def _provider(pid: str, behaviour):
    """A minimal Provider whose search() delegates to `behaviour`."""

    class P(Provider):
        id = pid
        name = pid
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        async def search(self, query):
            return await behaviour(query)

    return P()


def _endpoint(pid="opt", url="https://example.org/optimade") -> ProviderEndpoint:
    return ProviderEndpoint(
        id=pid, name="Opt", base_url=url,
        api_type="optimade", tier=1, enabled=True,
        behavior=BehaviorConfig(timeout_ms=5000),
        capabilities=CapabilitiesConfig(filterable_fields=["elements"]),
    )


def _optimade_entry(n: int) -> dict:
    return {
        "id": f"opt-{n}",
        "attributes": {
            "chemical_formula_reduced": "Fe2O3",
            "elements": ["Fe", "O"],
            "nelements": 2,
        },
    }


class _FakeResponse:
    def __init__(self, payload: dict, status_code: int = 200):
        self._payload = payload
        self.status_code = status_code

    def raise_for_status(self):
        if self.status_code >= 400:
            request = httpx.Request("GET", "https://example.org/optimade")
            raise httpx.HTTPStatusError(
                f"HTTP {self.status_code}",
                request=request,
                response=httpx.Response(self.status_code, request=request),
            )

    def json(self):
        return self._payload


class _FakeClient:
    """Stands in for httpx.AsyncClient; serves canned pages keyed by URL."""

    # {url: payload-or-status-int}
    pages: dict = {}
    requested: list = []

    def __init__(self, *args, **kwargs):
        pass

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return False

    async def get(self, url, params=None):
        _FakeClient.requested.append(url)
        page = _FakeClient.pages[url]
        if isinstance(page, int):
            return _FakeResponse({}, status_code=page)
        return _FakeResponse(page)


@pytest.fixture()
def fake_httpx(monkeypatch):
    _FakeClient.pages = {}
    _FakeClient.requested = []
    monkeypatch.setattr(httpx, "AsyncClient", _FakeClient)
    return _FakeClient


FIRST_URL = "https://example.org/optimade/v1/structures"


# ---------------------------------------------------------------------------
# OPTIMADE pagination (C: optimade.py fetched ONE page, never links.next)
# ---------------------------------------------------------------------------


def test_optimade_follows_next_links_until_the_limit(fake_httpx):
    """limit 5 against a server paging 2 rows at a time must walk 3 pages,
    not silently return the first 2 as success."""
    fake_httpx.pages = {
        FIRST_URL: {
            "data": [_optimade_entry(1), _optimade_entry(2)],
            "meta": {"data_returned": 5},
            "links": {"next": "https://example.org/optimade/v1/structures?page=2"},
        },
        "https://example.org/optimade/v1/structures?page=2": {
            "data": [_optimade_entry(3), _optimade_entry(4)],
            "meta": {"data_returned": 5},
            "links": {"next": {"href": "https://example.org/optimade/v1/structures?page=3"}},
        },
        "https://example.org/optimade/v1/structures?page=3": {
            "data": [_optimade_entry(5)],
            "meta": {"data_returned": 5},
            "links": {},
        },
    }
    p = OptimadeProvider(endpoint=_endpoint())
    page = asyncio.run(p.search(MaterialSearchQuery(elements=["Fe"], limit=5)))

    assert isinstance(page, ProviderPage)
    assert len(page.materials) == 5
    assert page.pages_fetched == 3, "links.next must actually be followed"
    assert page.truncated is False
    assert page.available == 5
    assert page.http_status_code == 200, "the REAL last-page status, not a literal"
    assert len(fake_httpx.requested) == 3


def test_optimade_server_page_cap_without_next_is_truncated(fake_httpx):
    """The exact brief scenario: a provider with a small server-side page cap
    and no next link must NOT report its 2 rows as the full answer to a
    limit-50 query when its own total says 100 matched."""
    fake_httpx.pages = {
        FIRST_URL: {
            "data": [_optimade_entry(1), _optimade_entry(2)],
            "meta": {"data_returned": 100},
            "links": {},
        },
    }
    p = OptimadeProvider(endpoint=_endpoint())
    page = asyncio.run(p.search(MaterialSearchQuery(elements=["Fe"], limit=50)))

    assert len(page.materials) == 2
    assert page.available == 100
    assert page.truncated is True, (
        "100 matched, 2 returned, 50 asked: this is a partial result and "
        "must say so"
    )


def test_optimade_endpoint_max_results_cap_is_reported_as_truncation(fake_httpx):
    """An endpoint clipped by its OWN behavior.max_results still owes the
    caller the truth.

    `limit` is min(query.limit, behavior.max_results) BEFORE the fetch, and
    the truncation check compared that already-clipped value with itself, so
    for a capped endpoint the condition could never fire. oqmd ships
    max_results=500 while the tool's limit goes to 10000, so this is the
    ordinary path. Measured before the fix: 20 asked, endpoint cap 5,
    provider's own meta.data_returned=50 -> 5 rows returned, truncated=False
    and the engine's `complete` flag True on a 5-of-50 answer.
    """
    fake_httpx.pages = {
        FIRST_URL: {
            "data": [_optimade_entry(n) for n in range(5)],
            "meta": {"data_returned": 50},
            "links": {"next": "https://example.org/optimade/v1/structures?page=2"},
        },
    }
    ep = _endpoint()
    ep.behavior.max_results = 5
    p = OptimadeProvider(endpoint=ep)
    page = asyncio.run(p.search(MaterialSearchQuery(elements=["Fe"], limit=20)))

    assert len(page.materials) == 5
    assert page.available == 50
    assert page.truncated is True, (
        "20 asked, 5 returned because of this endpoint's own max_results cap, "
        "50 matched: that is a slice, not the answer"
    )

    # And the consequence the agent actually reads.
    reg = ProviderRegistry()
    reg.register(OptimadeProvider(endpoint=ep))
    result = asyncio.run(
        _isolated_engine(reg).search(MaterialSearchQuery(elements=["Fe"], limit=20))
    )
    assert result.query_log[0].truncated is True
    assert result.complete is False, "a 5-of-50 answer is not a complete one"


def test_optimade_mid_chain_failure_returns_partial_not_success_shaped_lie(fake_httpx):
    """A page-two failure keeps page one's materials but marks the result
    truncated with the verbatim reason — never a bare success."""
    fake_httpx.pages = {
        FIRST_URL: {
            "data": [_optimade_entry(1), _optimade_entry(2)],
            "meta": {"data_returned": 4},
            "links": {"next": "https://example.org/optimade/v1/structures?page=2"},
        },
        "https://example.org/optimade/v1/structures?page=2": 500,
    }
    p = OptimadeProvider(endpoint=_endpoint())
    page = asyncio.run(p.search(MaterialSearchQuery(elements=["Fe"], limit=4)))

    assert len(page.materials) == 2
    assert page.truncated is True
    assert page.note and "pagination stopped" in page.note


def test_optimade_first_page_failure_still_raises(fake_httpx):
    """Nothing fetched means the query failed — that stays an exception for
    the circuit breaker, not an empty 'partial success'."""
    fake_httpx.pages = {FIRST_URL: 500}
    p = OptimadeProvider(endpoint=_endpoint())
    with pytest.raises(httpx.HTTPStatusError):
        asyncio.run(p.search(MaterialSearchQuery(elements=["Fe"], limit=4)))


def test_optimade_pagination_accounting_reaches_the_query_log(fake_httpx):
    """pages_fetched/truncated/available had ZERO writers; they must now
    arrive in the engine's ProviderQueryLog from measured values (a 2-page
    walk, so a hardcoded pages_fetched=1 cannot pass)."""
    fake_httpx.pages = {
        FIRST_URL: {
            "data": [_optimade_entry(1)],
            "meta": {"data_returned": 30},
            "links": {"next": "https://example.org/optimade/v1/structures?page=2"},
        },
        "https://example.org/optimade/v1/structures?page=2": {
            "data": [_optimade_entry(2)],
            "meta": {"data_returned": 30},
            "links": {},
        },
    }
    reg = ProviderRegistry()
    reg.register(OptimadeProvider(endpoint=_endpoint()))
    engine = _isolated_engine(reg)
    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"], limit=10)))

    log = result.query_log[0]
    assert log.status == "success"
    assert log.pages_fetched == 2, "measured pages, not a hardcoded default"
    assert log.truncated is True, "30 exist, 10 asked, 2 returned"
    assert log.available == 30
    assert log.http_status_code == 200
    # A truncated success is a PARTIAL result: the whole answer must say so.
    assert result.complete is False


# ---------------------------------------------------------------------------
# circuit_open producer (C: counted in tools.py, produced nowhere)
# ---------------------------------------------------------------------------


def test_breaker_open_provider_is_logged_as_circuit_open():
    async def _ok(query):
        return [_material("healthy", 1)]

    reg = ProviderRegistry()
    reg.register(_provider("healthy", _ok))
    reg.register(_provider("broken", _ok))
    engine = _isolated_engine(reg)
    # Open broken's circuit: two consecutive failures.
    engine._health.get("broken").record_failure()
    engine._health.get("broken").record_failure()

    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"])))
    statuses = {log.provider_id: log.status for log in result.query_log}
    assert statuses["healthy"] == "success"
    assert statuses["broken"] == "circuit_open", (
        "a breaker-skipped provider must appear in the log — 'queried 1 of 1' "
        "and 'queried 1 of 2' were previously indistinguishable"
    )
    open_log = next(l for l in result.query_log if l.provider_id == "broken")
    assert open_log.pages_fetched == 0
    assert result.complete is False, "an unconsulted provider means partial"


def test_all_breakers_open_still_reports_the_skips():
    async def _ok(query):  # pragma: no cover - never reached
        return []

    reg = ProviderRegistry()
    reg.register(_provider("only", _ok))
    engine = _isolated_engine(reg)
    engine._health.get("only").record_failure()
    engine._health.get("only").record_failure()

    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"])))
    assert [l.status for l in result.query_log] == ["circuit_open"]
    assert result.complete is False


# ---------------------------------------------------------------------------
# parse_error / error_raw / real http_status_code (C)
# ---------------------------------------------------------------------------


def test_a_body_that_fails_to_parse_is_parse_error_with_the_verbatim_error():
    async def _bad_json(query):
        raise json.JSONDecodeError("Expecting value", "<html>Bad gateway</html>", 0)

    reg = ProviderRegistry()
    reg.register(_provider("garbled", _bad_json))
    engine = _isolated_engine(reg)
    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"])))

    log = result.query_log[0]
    assert log.status == "parse_error", (
        "an unusable BODY is a parse failure, not an http_error"
    )
    assert log.error_raw and "Expecting value" in log.error_raw
    assert log.pages_fetched == 0


def test_an_http_failure_carries_its_real_status_code():
    async def _boom(query):
        request = httpx.Request("GET", "https://example.org")
        raise httpx.HTTPStatusError(
            "server melted",
            request=request,
            response=httpx.Response(503, request=request),
        )

    reg = ProviderRegistry()
    reg.register(_provider("melting", _boom))
    engine = _isolated_engine(reg)
    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"])))

    log = result.query_log[0]
    assert log.status == "http_error"
    assert log.http_status_code == 503, (
        "a real status existed; the log must carry it instead of null"
    )
    assert log.error_raw and "server melted" in log.error_raw


def test_a_plain_list_success_does_not_fabricate_a_200():
    async def _ok(query):
        return [_material("plain", 1)]

    reg = ProviderRegistry()
    reg.register(_provider("plain", _ok))
    engine = _isolated_engine(reg)
    result = asyncio.run(engine.search(MaterialSearchQuery(elements=["Fe"])))

    log = result.query_log[0]
    assert log.status == "success"
    assert log.http_status_code is None, (
        "the provider reported no wire status; writing 200 was fiction"
    )


# ---------------------------------------------------------------------------
# Partial fan-out cache TTL (C: one blip pinned a 3-of-42 result for a day)
# ---------------------------------------------------------------------------


def test_a_partial_fanout_is_cached_briefly_not_for_a_day():
    async def _ok(query):
        return [_material("good", 1)]

    async def _fail(query):
        raise ConnectionError("blip")

    reg = ProviderRegistry()
    reg.register(_provider("good", _ok))
    reg.register(_provider("flaky", _fail))
    engine = _isolated_engine(reg)
    q = MaterialSearchQuery(elements=["Fe"])
    result = asyncio.run(engine.search(q))

    assert result.complete is False
    cached = engine._cache._query_cache[q.query_hash()]
    assert cached.ttl == PARTIAL_RESULT_TTL_SECONDS, (
        "a 1-of-2 result must not sit under the 24h TTL"
    )


def test_a_complete_fanout_keeps_the_full_ttl():
    async def _ok(query):
        return [_material("good", 1)]

    reg = ProviderRegistry()
    reg.register(_provider("good", _ok))
    engine = _isolated_engine(reg)
    q = MaterialSearchQuery(elements=["Fe"])
    result = asyncio.run(engine.search(q))

    assert result.complete is True
    cached = engine._cache._query_cache[q.query_hash()]
    assert cached.ttl == 86400


# ---------------------------------------------------------------------------
# Tool surface
# ---------------------------------------------------------------------------


def test_tool_output_surfaces_complete_and_per_provider_accounting():
    from unittest.mock import AsyncMock, patch

    import app.tools.search_engine.tools as tools_mod
    from app.tools.search_engine.result import ProviderQueryLog, SearchResult
    from app.tools.search_engine.tools import _materials_search_factory

    q = MaterialSearchQuery(elements=["Cu"])
    fake = SearchResult(
        materials=[],
        total_count=0,
        query=q,
        query_log=[
            ProviderQueryLog(
                provider_id="p1", provider_name="p1",
                endpoint_url="https://example/p1", query_description="",
                started_at=0.0, completed_at=0.0, latency_ms=1.0,
                status="success", result_count=2,
                pages_fetched=3, truncated=True, available=40,
            ),
            ProviderQueryLog(
                provider_id="p2", provider_name="p2",
                endpoint_url="https://example/p2", query_description="",
                started_at=0.0, completed_at=0.0, latency_ms=0.0,
                status="circuit_open", pages_fetched=0,
            ),
        ],
        complete=False,
    )
    factory = _materials_search_factory(ProviderRegistry())
    with patch.object(tools_mod.SearchEngine, "search", new=AsyncMock(return_value=fake)):
        out = factory(elements=["Cu"])

    assert out["complete"] is False
    by_id = {p["provider_id"]: p for p in out["providers_queried"]}
    assert by_id["p1"]["truncated"] is True
    assert by_id["p1"]["pages_fetched"] == 3
    assert by_id["p1"]["available"] == 40
    assert out["providers_summary"]["circuit_open"] == 1, (
        "the counter finally has a producer"
    )
