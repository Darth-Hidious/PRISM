"""Tests for capability discovery tool."""
import pytest


class TestDiscoverCapabilities:
    """Test the unified capability discovery."""

    def test_returns_all_sections(self):
        from app.tools.capabilities import discover_capabilities
        caps = discover_capabilities()
        assert "search_providers" in caps
        assert "datasets" in caps
        assert "trained_models" in caps
        assert "pretrained_models" in caps
        assert "feature_backend" in caps
        assert "calphad" in caps
        assert "simulation" in caps
        assert "lab_subscriptions" in caps
        assert "plugins" in caps

    def test_search_providers_list(self):
        from app.tools.capabilities import discover_capabilities
        caps = discover_capabilities()
        providers = caps["search_providers"]
        assert isinstance(providers, list)
        # At minimum should have OPTIMADE providers from cache or overrides
        # (may be empty in CI without network)

    def test_feature_backend_known(self):
        from app.tools.capabilities import discover_capabilities
        caps = discover_capabilities()
        assert caps["feature_backend"] in ("matminer", "basic", "unknown")

    def test_calphad_section(self):
        from app.tools.capabilities import discover_capabilities
        caps = discover_capabilities()
        assert "available" in caps["calphad"]
        assert "databases" in caps["calphad"]
        assert isinstance(caps["calphad"]["databases"], list)

    def test_simulation_section(self):
        from app.tools.capabilities import discover_capabilities
        caps = discover_capabilities()
        assert "available" in caps["simulation"]

    def test_pretrained_models_list(self):
        from app.tools.capabilities import discover_capabilities
        caps = discover_capabilities()
        pretrained = caps["pretrained_models"]
        assert isinstance(pretrained, list)
        names = [m["name"] for m in pretrained]
        assert "m3gnet-eform" in names
        assert "megnet-bandgap" in names

    def test_lab_subscriptions_empty(self):
        from app.tools.capabilities import discover_capabilities
        caps = discover_capabilities()
        assert isinstance(caps["lab_subscriptions"], list)


class TestCapabilitiesSummary:
    """Test the text summary for system prompt injection."""

    def test_summary_is_string(self):
        from app.tools.capabilities import capabilities_summary
        summary = capabilities_summary()
        assert isinstance(summary, str)
        assert len(summary) > 0

    def test_summary_contains_feature_backend(self):
        from app.tools.capabilities import capabilities_summary
        summary = capabilities_summary()
        assert "Feature backend:" in summary

    def test_summary_contains_calphad(self):
        from app.tools.capabilities import capabilities_summary
        summary = capabilities_summary()
        assert "CALPHAD:" in summary

    def test_summary_contains_simulation(self):
        from app.tools.capabilities import capabilities_summary
        summary = capabilities_summary()
        assert "Simulation" in summary


class TestCapabilitiesToolRegistered:
    """Test that the tool is available in the registry."""

    def test_in_bootstrap(self):
        from app.plugins.bootstrap import build_full_registry
        tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in tool_reg.list_tools()}
        assert "discover_capabilities" in names

    def test_tool_executes(self):
        from app.plugins.bootstrap import build_full_registry
        tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        tool = tool_reg.get("discover_capabilities")
        result = tool.execute()
        assert "search_providers" in result
        assert "calphad" in result


class TestSystemPromptInjection:
    """Test that capabilities get injected into the system prompt."""

    def test_inject_capabilities(self):
        from app.agent.core import AgentCore
        result = AgentCore._inject_capabilities("Base prompt here.")
        assert "Base prompt here." in result
        assert "AVAILABLE RESOURCES" in result
        assert "Feature backend:" in result
