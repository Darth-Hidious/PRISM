"""Tests for unified PluginRegistry with provider + agent registries."""


def test_plugin_registry_has_provider_registry():
    from app.plugins.registry import PluginRegistry

    reg = PluginRegistry()
    assert hasattr(reg, "provider_registry")


def test_plugin_can_register_provider():
    from app.plugins.registry import PluginRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class FakeProvider(Provider):
        id = "fake"
        name = "Fake"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query): return []

    reg = PluginRegistry()
    reg.provider_registry.register(FakeProvider())
    assert "fake" in {p.id for p in reg.provider_registry.get_all()}


