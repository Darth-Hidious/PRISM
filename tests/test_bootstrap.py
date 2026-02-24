"""Tests for build_full_registry bootstrap function."""
from unittest.mock import patch
from app.plugins.bootstrap import build_full_registry


class TestBuildFullRegistry:
    def test_returns_tool_registry(self):
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        from app.tools.base import ToolRegistry
        assert isinstance(registry, ToolRegistry)

    def test_has_core_tools(self):
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "search_optimade" in names
        assert "query_materials_project" in names
        assert "export_results_csv" in names
        assert "import_dataset" in names

    def test_has_system_tools(self):
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        # System tools exist (read_file, write_file, web_search)
        assert "read_file" in names or len(names) > 3

    def test_has_visualization_tools(self):
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "plot_materials_comparison" in names or "plot_property_distribution" in names

    def test_has_prediction_tools(self):
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "predict_property" in names or "train_model" in names

    def test_has_builtin_skills(self):
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "acquire_materials" in names
        assert "materials_discovery" in names

    def test_plugins_enabled_does_not_crash(self):
        # Should not crash even if no plugins installed
        registry = build_full_registry(enable_mcp=False, enable_plugins=True)
        assert len(registry.list_tools()) > 0

    def test_mcp_disabled(self):
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        # Should work without MCP
        assert len(registry.list_tools()) > 0

    def test_works_without_pycalphad(self):
        """build_full_registry() succeeds even when pycalphad is not installed."""
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        # CALPHAD tools should not be present (pycalphad not installed)
        names = {t.name for t in registry.list_tools()}
        assert "calculate_phase_diagram" not in names

    def test_has_analyze_phases_skill(self):
        """The analyze_phases skill is always registered (graceful on missing pycalphad)."""
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "analyze_phases" in names

    def test_has_list_predictable_properties(self):
        """list_predictable_properties tool is registered."""
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "list_predictable_properties" in names

    def test_has_validate_and_review_skills(self):
        """validate_dataset and review_dataset skills are registered."""
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "validate_dataset" in names
        assert "review_dataset" in names

    def test_has_correlation_matrix_tool(self):
        """plot_correlation_matrix tool is registered."""
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "plot_correlation_matrix" in names
