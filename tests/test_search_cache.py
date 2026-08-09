from app.tools.search_engine.query import MaterialSearchQuery
from app.tools.search_engine.result import Material, SearchResult


def _make_result(formula="Fe2O3", count=1):
    m = Material(id="mp-1", formula=formula, elements=["Fe", "O"], n_elements=2, sources=["mp"])
    return SearchResult(
        materials=[m] * count, total_count=count,
        query=MaterialSearchQuery(elements=["Fe", "O"]),
        query_log=[], warnings=[],
    )


def test_cache_put_get():
    from app.tools.search_engine.cache.engine import SearchCache
    cache = SearchCache()
    q = MaterialSearchQuery(elements=["Fe", "O"])
    r = _make_result()
    cache.put(q, r)
    hit = cache.get(q)
    assert hit is not None
    assert hit.total_count == 1


def test_cache_miss():
    from app.tools.search_engine.cache.engine import SearchCache
    cache = SearchCache()
    q = MaterialSearchQuery(elements=["Si"])
    assert cache.get(q) is None


def test_cache_material_index():
    from app.tools.search_engine.cache.engine import SearchCache
    cache = SearchCache()
    q = MaterialSearchQuery(elements=["Fe", "O"])
    cache.put(q, _make_result())
    m = cache.get_material("mp-1")
    assert m is not None
    assert m.formula == "Fe2O3"


def test_cache_get_all_materials():
    from app.tools.search_engine.cache.engine import SearchCache
    cache = SearchCache()
    cache.put(MaterialSearchQuery(elements=["Fe"]), _make_result("Fe2O3"))
    cache.put(MaterialSearchQuery(elements=["Si"]), _make_result("SiO2"))
    all_m = cache.get_all_materials()
    assert len(all_m) >= 1


def test_cache_stats():
    from app.tools.search_engine.cache.engine import SearchCache
    cache = SearchCache()
    cache.put(MaterialSearchQuery(elements=["Fe"]), _make_result())
    s = cache.stats()
    assert s["query_count"] == 1
    assert s["material_count"] >= 1


def test_cache_clear():
    from app.tools.search_engine.cache.engine import SearchCache
    cache = SearchCache()
    cache.put(MaterialSearchQuery(elements=["Fe"]), _make_result())
    cache.clear()
    assert cache.stats()["query_count"] == 0


def test_cache_disk_persist(tmp_path):
    from app.tools.search_engine.cache.engine import SearchCache
    cache = SearchCache(disk_dir=tmp_path)
    q = MaterialSearchQuery(elements=["Fe", "O"])
    cache.put(q, _make_result())
    cache.flush_to_disk()
    # New cache loads from disk
    cache2 = SearchCache(disk_dir=tmp_path)
    cache2.load_from_disk()
    hit = cache2.get(q)
    assert hit is not None


def _provider(pid: str, *, raises: bool = False, materials=None):
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class P(Provider):
        id = pid
        name = pid
        capabilities = ProviderCapabilities(filterable_fields={"elements"})

        async def search(self, query):
            if raises:
                raise ConnectionError("offline mode: external DNS/network access blocked")
            return list(materials or [])

    return P()


def _engine(prov):
    from app.tools.search_engine.cache.engine import SearchCache
    from app.tools.search_engine.engine import SearchEngine
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.resilience.circuit_breaker import HealthManager

    reg = ProviderRegistry()
    reg.register(prov)
    return SearchEngine(
        registry=reg,
        cache=SearchCache(disk_dir=None),
        health_manager=HealthManager(persist_path=None),
    )


def test_a_search_nobody_answered_is_not_cached_but_a_real_empty_answer_is():
    """`put` was unconditional, so a failed search poisoned the next 24 hours.

    Every provider failing — exactly what hard offline produces — still stored
    an empty `SearchResult` under a 24h TTL, keyed by `query_hash()`, which
    covers the query parameters and nothing about the network. A later ONLINE
    search of the same query then short-circuited at the cache check and
    returned zero materials without contacting anyone.

    Worse than the circuit-breaker case fixed alongside it: 24 hours rather
    than a 300s cooldown, and ONE failed search rather than two.

    The distinction that matters: a provider replying "no matches" IS real
    knowledge and must still be cached. What must not be cached is an answer
    nobody gave.
    """
    import asyncio

    from app.tools.search_engine.query import MaterialSearchQuery
    from app.tools.search_engine.result import Material

    query = MaterialSearchQuery(elements=["Fe"], limit=5)

    # Nobody answered -> must NOT be cached.
    engine = _engine(_provider("dead", raises=True))
    first = asyncio.run(engine.search(query))
    second = asyncio.run(engine.search(query))
    assert first.materials == []
    assert second.cached is False, (
        "a search in which every provider failed was cached — the next 24h of "
        "identical queries would return it without contacting anyone"
    )

    # A provider answered "no matches" -> that IS knowledge, cache it.
    engine = _engine(_provider("quiet"))
    asyncio.run(engine.search(query))
    assert asyncio.run(engine.search(query)).cached is True

    # A provider answered with a hit -> cached.
    hit = Material(id="h-1", formula="Fe2O3", elements=["Fe", "O"], n_elements=2, sources=["h"])
    engine = _engine(_provider("hit", materials=[hit]))
    asyncio.run(engine.search(query))
    assert asyncio.run(engine.search(query)).cached is True
