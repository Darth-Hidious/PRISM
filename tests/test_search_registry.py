"""Tests for ProviderRegistry — loading, routing, custom registration."""


def test_registry_loads_from_json():
    from app.search.providers.registry import ProviderRegistry
    reg = ProviderRegistry.from_registry_json()
    providers = reg.get_all()
    assert len(providers) > 0


def test_registry_get_capable():
    from app.search.providers.registry import ProviderRegistry
    from app.search.query import MaterialSearchQuery, PropertyRange
    reg = ProviderRegistry.from_registry_json()
    # Simple elements query — many providers can handle
    q1 = MaterialSearchQuery(elements=["Fe"])
    capable = reg.get_capable(q1)
    assert len(capable) > 0
    # Band gap query — only MP native can filter on it
    q2 = MaterialSearchQuery(band_gap=PropertyRange(min=1.0))
    capable2 = reg.get_capable(q2)
    ids = {p.id for p in capable2}
    assert "mp_native" in ids


def test_registry_register_custom():
    from app.search.providers.registry import ProviderRegistry
    from app.search.providers.base import Provider, ProviderCapabilities
    from app.search.query import MaterialSearchQuery
    from app.search.result import Material

    class FakeProvider(Provider):
        id = "fake"
        name = "Fake"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query: MaterialSearchQuery) -> list[Material]:
            return []

    reg = ProviderRegistry()
    reg.register(FakeProvider())
    assert "fake" in {p.id for p in reg.get_all()}
