"""Tests for unified PluginRegistry with provider + agent registries."""


def test_plugin_registry_has_provider_registry():
    from app.plugins.registry import PluginRegistry

    reg = PluginRegistry()
    assert hasattr(reg, "provider_registry")


def test_plugin_registry_has_agent_registry():
    from app.plugins.registry import PluginRegistry

    reg = PluginRegistry()
    assert hasattr(reg, "agent_registry")


def test_plugin_can_register_provider():
    from app.plugins.registry import PluginRegistry
    from app.search.providers.base import Provider, ProviderCapabilities

    class FakeProvider(Provider):
        id = "fake"
        name = "Fake"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query): return []

    reg = PluginRegistry()
    reg.provider_registry.register(FakeProvider())
    assert "fake" in {p.id for p in reg.provider_registry.get_all()}


def test_plugin_can_register_agent_config():
    from app.plugins.registry import PluginRegistry
    from app.agent.agent_registry import AgentConfig

    reg = PluginRegistry()
    reg.agent_registry.register(AgentConfig(id="test", name="Test"))
    assert reg.agent_registry.get("test") is not None
