"""`materials_search` must never answer "nothing" when it means "I could
not look".

A broken install (missing `provider_overrides.json`, missing
`app.tools.memory`) made `build_registry()` raise, `bootstrap` fall back to
an empty `ProviderRegistry()`, and this tool answer
`{"materials": [], "count": 0, "providers_queried": []}` with exit 0. The
agent — and the user reading it — cannot tell that from "no material
matched your filters".
"""

from __future__ import annotations

from unittest.mock import AsyncMock, patch

from app.tools.search_engine.providers.registry import ProviderRegistry
from app.tools.search_engine.query import MaterialSearchQuery
from app.tools.search_engine.result import SearchResult
from app.tools.search_engine.tools import _materials_search_factory
import app.tools.search_engine.tools as tools_mod


def _empty_result(query):
    return SearchResult(
        materials=[],
        total_count=0,
        query=query,
        query_log=[],
        warnings=["No providers available for this query"],
    )


def test_an_empty_registry_is_reported_as_a_fault_not_as_no_results():
    """The exact shape a broken install produced."""
    factory = _materials_search_factory(ProviderRegistry())
    q = MaterialSearchQuery(elements=["Cu"])
    with patch.object(
        tools_mod.SearchEngine, "search", new=AsyncMock(return_value=_empty_result(q))
    ):
        out = factory(elements=["Cu"])

    assert out.get("error"), "an unqueryable federation must be an error"
    assert out["error_type"] == "NoProvidersQueried"
    # No empty list for a caller to mistake for an answer.
    assert "materials" not in out
    assert "count" not in out
    # And the message must say what is actually wrong, not just "failed".
    assert "provider_overrides.json" in out["error"]


def test_a_registry_with_providers_says_so_when_none_could_serve_the_query():
    """Distinct failure, distinct message: the install is fine, the
    capability match or the circuit breakers ruled everything out."""
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class Dummy(Provider):
        id = "dummy"
        name = "dummy"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        async def search(self, query):  # pragma: no cover - never called
            return []

    reg = ProviderRegistry()
    reg.register(Dummy())
    factory = _materials_search_factory(reg)
    q = MaterialSearchQuery(elements=["Cu"])
    with patch.object(
        tools_mod.SearchEngine, "search", new=AsyncMock(return_value=_empty_result(q))
    ):
        out = factory(elements=["Cu"])

    assert out["error_type"] == "NoProvidersQueried"
    assert "1 registered providers" in out["error"]
    assert "provider_overrides.json" not in out["error"]


def test_a_provider_that_answered_with_nothing_is_still_a_real_empty_result():
    """The honest empty answer must survive: a provider WAS queried and it
    genuinely returned no matches. Turning that into an error would be the
    opposite over-correction."""
    from tests.test_search_engine import _make_log  # reuse the existing helper

    q = MaterialSearchQuery(elements=["Og"])
    result = SearchResult(
        materials=[],
        total_count=0,
        query=q,
        query_log=[_make_log("real-prov", "success")],
        warnings=[],
    )
    factory = _materials_search_factory(ProviderRegistry())
    with patch.object(
        tools_mod.SearchEngine, "search", new=AsyncMock(return_value=result)
    ):
        out = factory(elements=["Og"])

    assert "error" not in out
    assert out["materials"] == []
    assert out["count"] == 0
    assert out["providers_summary"]["succeeded"] == 1
