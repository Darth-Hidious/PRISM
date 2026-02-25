def test_property_value_creation():
    from app.search.result import PropertyValue
    pv = PropertyValue(value=2.2, source="optimade:mp", method="DFT-PBE", unit="eV")
    assert pv.value == 2.2
    assert pv.source == "optimade:mp"


def test_material_creation():
    from app.search.result import Material
    m = Material(
        id="mp-1234",
        formula="Fe2O3",
        elements=["Fe", "O"],
        n_elements=2,
        sources=["mp"],
    )
    assert m.formula == "Fe2O3"
    assert m.sources == ["mp"]


def test_material_extra_properties():
    from app.search.result import Material, PropertyValue
    m = Material(
        id="mp-1234", formula="Fe2O3", elements=["Fe", "O"],
        n_elements=2, sources=["mp"],
        band_gap=PropertyValue(value=2.2, source="optimade:mp"),
        extra_properties={
            "band_gap:aflow": PropertyValue(value=2.0, source="optimade:aflow"),
        },
    )
    assert m.band_gap.value == 2.2
    assert m.extra_properties["band_gap:aflow"].value == 2.0


def test_provider_query_log():
    from app.search.result import ProviderQueryLog
    log = ProviderQueryLog(
        provider_id="mp", provider_name="Materials Project",
        endpoint_url="https://optimade.materialsproject.org/v1/structures",
        query_sent='elements HAS ALL "Fe","O"',
        started_at=1000.0, completed_at=1000.34, latency_ms=340.0,
        status="success", http_status_code=200, result_count=89,
    )
    assert log.status == "success"
    assert log.latency_ms == 340.0


def test_search_result():
    from app.search.result import SearchResult, Material, ProviderQueryLog
    from app.search.query import MaterialSearchQuery
    r = SearchResult(
        materials=[],
        total_count=0,
        query=MaterialSearchQuery(),
        query_log=[],
        warnings=[],
    )
    assert r.total_count == 0
    assert r.cached is False
