from app.search.query import MaterialSearchQuery
from app.search.result import Material, SearchResult


def _make_result(formula="Fe2O3", count=1):
    m = Material(id="mp-1", formula=formula, elements=["Fe", "O"], n_elements=2, sources=["mp"])
    return SearchResult(
        materials=[m] * count, total_count=count,
        query=MaterialSearchQuery(elements=["Fe", "O"]),
        query_log=[], warnings=[],
    )


def test_cache_put_get():
    from app.search.cache.engine import SearchCache
    cache = SearchCache()
    q = MaterialSearchQuery(elements=["Fe", "O"])
    r = _make_result()
    cache.put(q, r)
    hit = cache.get(q)
    assert hit is not None
    assert hit.total_count == 1


def test_cache_miss():
    from app.search.cache.engine import SearchCache
    cache = SearchCache()
    q = MaterialSearchQuery(elements=["Si"])
    assert cache.get(q) is None


def test_cache_material_index():
    from app.search.cache.engine import SearchCache
    cache = SearchCache()
    q = MaterialSearchQuery(elements=["Fe", "O"])
    cache.put(q, _make_result())
    m = cache.get_material("mp-1")
    assert m is not None
    assert m.formula == "Fe2O3"


def test_cache_get_all_materials():
    from app.search.cache.engine import SearchCache
    cache = SearchCache()
    cache.put(MaterialSearchQuery(elements=["Fe"]), _make_result("Fe2O3"))
    cache.put(MaterialSearchQuery(elements=["Si"]), _make_result("SiO2"))
    all_m = cache.get_all_materials()
    assert len(all_m) >= 1


def test_cache_stats():
    from app.search.cache.engine import SearchCache
    cache = SearchCache()
    cache.put(MaterialSearchQuery(elements=["Fe"]), _make_result())
    s = cache.stats()
    assert s["query_count"] == 1
    assert s["material_count"] >= 1


def test_cache_clear():
    from app.search.cache.engine import SearchCache
    cache = SearchCache()
    cache.put(MaterialSearchQuery(elements=["Fe"]), _make_result())
    cache.clear()
    assert cache.stats()["query_count"] == 0


def test_cache_disk_persist(tmp_path):
    from app.search.cache.engine import SearchCache
    cache = SearchCache(disk_dir=tmp_path)
    q = MaterialSearchQuery(elements=["Fe", "O"])
    cache.put(q, _make_result())
    cache.flush_to_disk()
    # New cache loads from disk
    cache2 = SearchCache(disk_dir=tmp_path)
    cache2.load_from_disk()
    hit = cache2.get(q)
    assert hit is not None
