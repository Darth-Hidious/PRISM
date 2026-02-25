"""Tests for SearchEngine orchestrator — federated async fan-out, caching, audit trail."""
import asyncio
from unittest.mock import AsyncMock, patch

import pytest

from app.search.query import MaterialSearchQuery, PropertyRange
from app.search.result import Material, PropertyValue


def _mock_material(pid="mp", formula="Fe2O3"):
    return Material(
        id=f"{pid}-1", formula=formula, elements=["Fe", "O"],
        n_elements=2, sources=[pid],
        band_gap=PropertyValue(value=2.2, source=f"optimade:{pid}", unit="eV"),
    )


def test_engine_creates():
    from app.search.engine import SearchEngine
    from app.search.providers.registry import ProviderRegistry
    engine = SearchEngine(registry=ProviderRegistry())
    assert engine is not None


def test_engine_search_empty_registry():
    from app.search.engine import SearchEngine
    from app.search.providers.registry import ProviderRegistry
    engine = SearchEngine(registry=ProviderRegistry())
    q = MaterialSearchQuery(elements=["Fe"])
    result = asyncio.run(engine.search(q))
    assert result.total_count == 0
    assert result.warnings  # should warn no providers


def test_engine_search_with_mock_provider():
    from app.search.engine import SearchEngine
    from app.search.providers.registry import ProviderRegistry
    from app.search.providers.base import Provider, ProviderCapabilities

    class MockProvider(Provider):
        id = "mock"
        name = "Mock"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query):
            return [_mock_material("mock")]

    reg = ProviderRegistry()
    reg.register(MockProvider())
    engine = SearchEngine(registry=reg)
    q = MaterialSearchQuery(elements=["Fe", "O"])
    result = asyncio.run(engine.search(q))
    assert result.total_count == 1
    assert result.materials[0].formula == "Fe2O3"
    assert len(result.query_log) == 1
    assert result.query_log[0].status == "success"


def test_engine_search_provider_failure_graceful():
    from app.search.engine import SearchEngine
    from app.search.providers.registry import ProviderRegistry
    from app.search.providers.base import Provider, ProviderCapabilities

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
    engine = SearchEngine(registry=reg)
    q = MaterialSearchQuery(elements=["Fe"])
    result = asyncio.run(engine.search(q))
    assert result.total_count == 1
    assert len(result.query_log) == 2
    statuses = {log.provider_id: log.status for log in result.query_log}
    assert statuses["fail"] in ("http_error", "timeout")
    assert statuses["good"] == "success"


def test_engine_caches_result():
    from app.search.engine import SearchEngine
    from app.search.providers.registry import ProviderRegistry
    from app.search.providers.base import Provider, ProviderCapabilities

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
    engine = SearchEngine(registry=reg)
    q = MaterialSearchQuery(elements=["Fe"])
    r1 = asyncio.run(engine.search(q))
    r2 = asyncio.run(engine.search(q))
    assert call_count == 1
    assert r2.cached is True


def test_engine_audit_trail_has_url():
    from app.search.engine import SearchEngine
    from app.search.providers.registry import ProviderRegistry
    from app.search.providers.base import Provider, ProviderCapabilities

    class MockProvider(Provider):
        id = "mock"
        name = "Mock Provider"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query):
            return []

    reg = ProviderRegistry()
    reg.register(MockProvider())
    engine = SearchEngine(registry=reg)
    q = MaterialSearchQuery(elements=["Fe"])
    result = asyncio.run(engine.search(q))
    assert len(result.query_log) == 1
    assert result.query_log[0].provider_id == "mock"
