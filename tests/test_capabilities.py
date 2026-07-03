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
        assert "marc27_platform" in caps

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

    def test_platform_capabilities_merge(self, monkeypatch):
        from app.tools import capabilities as mod

        class _Resp:
            def __init__(self, payload):
                self._payload = payload

            def json(self):
                return self._payload

        class _Base:
            def get(self, path):
                if path == "/agent/capabilities":
                    return _Resp({"services": {"knowledge": {"endpoint_count": 7}}})
                if path == "/knowledge/capabilities":
                    return _Resp({"query": {"semantic_search": {"method": "POST"}}})
                if path == "/projects/project-123/llm/models":
                    return _Resp([
                        {"id": "gemini-3.1-flash-preview", "provider": "google"},
                        {"id": "claude-sonnet-4-6", "provider": "anthropic"},
                    ])
                raise AssertionError(f"unexpected path: {path}")

        class _Knowledge:
            def graph_stats(self):
                return {"nodes": 12, "edges": 34, "entity_types": 5}

            def embedding_stats(self):
                return {"embeddings": 56}

        class _Client:
            def __init__(self):
                self._base = _Base()
                self.knowledge = _Knowledge()

        monkeypatch.setattr(mod, "_get_platform_client", lambda: _Client())
        monkeypatch.setattr(
            mod,
            "_load_project_context",
            lambda: {"project_id": "project-123", "project_name": "Sandbox"},
        )

        caps = mod.discover_capabilities()
        platform = caps["marc27_platform"]
        assert platform["connected"] is True
        assert platform["project_id"] == "project-123"
        assert platform["hosted_model_count"] == 2
        assert platform["hosted_model_providers"] == ["anthropic", "google"]
        assert platform["agent_capabilities"]["services"]["knowledge"]["endpoint_count"] == 7
        assert platform["knowledge_capabilities"]["query"]["semantic_search"]["method"] == "POST"

    def test_platform_capabilities_fallback_when_unavailable(self, monkeypatch):
        from app.tools import capabilities as mod

        monkeypatch.setattr(mod, "_get_platform_client", lambda: None)

        caps = mod.discover_capabilities()
        platform = caps["marc27_platform"]
        assert platform["connected"] is False
        assert platform["hosted_model_count"] == 0
        assert isinstance(platform["hosted_models"], list)


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

    def test_summary_mentions_platform_models_when_connected(self, monkeypatch):
        from app.tools import capabilities as mod

        monkeypatch.setattr(
            mod,
            "discover_capabilities",
            lambda: {
                "search_providers": [],
                "datasets": [],
                "trained_models": [],
                "pretrained_models": [],
                "feature_backend": "matminer",
                "calphad": {"available": False, "databases": []},
                "simulation": {"available": False},
                "lab_subscriptions": [],
                "plugins": [],
                "marc27_platform": {
                    "connected": True,
                    "hosted_model_count": 519,
                    "hosted_model_providers": ["anthropic", "google", "openai", "openrouter"],
                },
            },
        )

        summary = mod.capabilities_summary()
        assert "Platform LLM models: 519 discovered" in summary


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
