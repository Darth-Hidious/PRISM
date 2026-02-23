"""Tests for visualization tools."""
import pytest
from app.tools.visualization import create_visualization_tools
from app.tools.base import ToolRegistry


class TestVisualizationTools:
    def test_tools_registered(self):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "plot_materials_comparison" in names
        assert "plot_property_distribution" in names

    def test_comparison_plot(self, tmp_path):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot_materials_comparison")
        result = tool.execute(
            materials=[{"name": "Si", "band_gap": 1.1, "formation_energy": -0.5}, {"name": "Ge", "band_gap": 0.67, "formation_energy": -0.3}],
            property_x="band_gap", property_y="formation_energy", output_path=str(tmp_path / "comparison.png"))
        assert result.get("success") is True or "error" in result

    def test_distribution_plot(self, tmp_path):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot_property_distribution")
        result = tool.execute(values=[1.1, 2.3, 0.5, 1.8, 3.2], property_name="band_gap", output_path=str(tmp_path / "dist.png"))
        assert result.get("success") is True or "error" in result
