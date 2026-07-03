"""Tests for build_full_registry bootstrap function."""
from unittest.mock import patch
from app.plugins.bootstrap import build_full_registry


class TestBuildFullRegistry:
    def test_returns_tool_registry(self):
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        from app.tools.base import ToolRegistry
        assert isinstance(registry, ToolRegistry)
        # Also verify we got 3 items from the full call

    def test_has_core_tools(self):
        """After Round 7: search_materials was removed (duplicate of
        materials_search); import_dataset and export_results_csv folded into
        the unified `dataset` Tool as actions."""
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "materials_search" in names  # canonical federated search
        assert "query_materials_project" in names  # MP-specific deep query
        assert "dataset" in names  # absorbs import + export
        # Old names must be gone
        assert "search_materials" not in names
        assert "import_dataset" not in names
        assert "export_results_csv" not in names

    def test_has_system_tools(self):
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        # System tools exist (file unified, show_scratchpad). web search
        # lives in app/tools/web.py now as web(action='search').
        assert "file" in names
        assert "show_scratchpad" in names

    def test_has_visualization_tools(self):
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "plot" in names  # unified tool replaced plot_materials_comparison + plot_property_distribution + plot_correlation_matrix

    def test_has_prediction_tools(self):
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "predict" in names or "train_model" in names  # unified `predict(target=...)` replaces predict_property + predict_structure

    def test_has_builtin_skills(self):
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "acquire_materials" in names
        assert "materials_discovery" in names

    def test_plugins_enabled_does_not_crash(self):
        # Should not crash even if no plugins installed
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=True)
        assert len(registry.list_tools()) > 0

    def test_mcp_disabled(self):
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        # Should work without MCP
        assert len(registry.list_tools()) > 0

    @patch("app.tools.simulation.calphad_bridge.check_calphad_available", return_value=False)
    def test_works_without_pycalphad(self, mock_check):
        """build_full_registry() succeeds even when pycalphad is not installed."""
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        # CALPHAD tools should not be present (pycalphad not installed)
        names = {t.name for t in registry.list_tools()}
        assert "calculate_phase_diagram" not in names

    def test_has_analyze_phases_skill(self):
        """The analyze_phases skill is always registered (graceful on missing pycalphad)."""
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "analyze_phases" in names

    def test_has_list_predictable_properties(self):
        """list_predictable_properties tool is registered."""
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "list_predictable_properties" in names

    def test_has_dataset_tool(self):
        """Round 6 collapse: validate_dataset / review_dataset / visualize_dataset
        Skills were collapsed into the unified `dataset(action=…)` Tool."""
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "dataset" in names
        # Old Skill names must be gone
        assert "validate_dataset" not in names
        assert "review_dataset" not in names
        assert "visualize_dataset" not in names
        # The unified tool advertises all five actions (Round 7 added
        # import + export by absorbing import_dataset + export_results_csv)
        ds = registry.get("dataset")
        actions = ds.input_schema["properties"]["action"]["enum"]
        assert set(actions) == {"validate", "review", "visualize", "import", "export"}

    def test_has_correlation_matrix_tool(self):
        """correlation_matrix is now a kind of the unified `plot` tool."""
        registry, _prov, _agents = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        assert "plot" in names
        plot_tool = registry.get("plot")
        assert "correlation_matrix" in plot_tool.input_schema["properties"]["kind"]["enum"]
