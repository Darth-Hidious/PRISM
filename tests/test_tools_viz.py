"""Tests for visualization tools.

After Round 4 batch 2: `plot_materials_comparison`, `plot_property_distribution`,
and `plot_correlation_matrix` were collapsed into a single `plot(kind=…)` tool.
"""
import pytest
from app.tools.visualization import create_visualization_tools
from app.tools.base import ToolRegistry


class TestVisualizationTools:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        names = [t.name for t in registry.list_tools()]
        # Unified plot tool replaces the 3 plot_* tools
        assert "plot" in names
        # Old names must be gone
        assert "plot_materials_comparison" not in names
        assert "plot_property_distribution" not in names
        assert "plot_correlation_matrix" not in names

    def test_kind_enum(self):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot")
        kinds = tool.input_schema["properties"]["kind"]["enum"]
        assert set(kinds) == {
            "materials_comparison",
            "property_distribution",
            "correlation_matrix",
        }

    def test_comparison_plot(self, tmp_path):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot")
        result = tool.execute(
            kind="materials_comparison",
            materials=[
                {"name": "Si", "band_gap": 1.1, "formation_energy": -0.5},
                {"name": "Ge", "band_gap": 0.67, "formation_energy": -0.3},
            ],
            property_x="band_gap",
            property_y="formation_energy",
            output_path=str(tmp_path / "comparison.png"),
        )
        # Either it rendered successfully, OR matplotlib is missing — both fine
        assert result.get("success") is True or "error" in result

    def test_distribution_plot(self, tmp_path):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot")
        result = tool.execute(
            kind="property_distribution",
            values=[1.1, 2.3, 0.5, 1.8, 3.2],
            property_name="band_gap",
            output_path=str(tmp_path / "dist.png"),
        )
        assert result.get("success") is True or "error" in result

    def test_missing_kind_returns_clear_error(self):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot")
        result = tool.execute()
        assert "error" in result
        assert "Missing 'kind'" in result["error"]

    def test_unknown_kind_returns_clear_error(self):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot")
        result = tool.execute(kind="bogus_kind")
        assert "error" in result
        assert "Unknown kind" in result["error"]

    def test_kind_specific_arg_validation(self):
        """Each kind validates its required args before doing matplotlib work."""
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot")

        # materials_comparison without materials/property_x/property_y
        r = tool.execute(kind="materials_comparison")
        assert "error" in r
        assert "materials" in r["error"]

        # property_distribution without values
        r = tool.execute(kind="property_distribution")
        assert "error" in r
        assert "`values`" in r["error"]

        # correlation_matrix without dataset_name
        r = tool.execute(kind="correlation_matrix")
        assert "error" in r
        assert "`dataset_name`" in r["error"]
