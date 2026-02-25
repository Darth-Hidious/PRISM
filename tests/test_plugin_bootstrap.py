"""Tests for build_full_registry with unified registries."""


def test_build_full_registry_returns_three_registries():
    from app.plugins.bootstrap import build_full_registry

    result = build_full_registry(enable_mcp=False, enable_plugins=False)
    assert len(result) == 3  # tool_reg, provider_reg, agent_reg


def test_build_full_registry_loads_agent_configs():
    from app.plugins.bootstrap import build_full_registry

    _tools, _providers, agent_reg = build_full_registry(enable_mcp=False, enable_plugins=False)
    # catalog.json has phase_stability_agent
    config = agent_reg.get("phase_stability_agent")
    assert config is not None
    assert config.runtime == "local"


def test_build_full_registry_provider_registry_has_platform_providers():
    from app.plugins.bootstrap import build_full_registry

    _tools, provider_reg, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
    ids = {p.id for p in provider_reg.get_all()}
    assert "mp_native" in ids
