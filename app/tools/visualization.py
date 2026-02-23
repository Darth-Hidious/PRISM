"""Visualization tools for materials data."""
from app.tools.base import Tool, ToolRegistry


def _plot_materials_comparison(**kwargs) -> dict:
    materials = kwargs["materials"]
    prop_x = kwargs["property_x"]
    prop_y = kwargs["property_y"]
    output_path = kwargs.get("output_path", "comparison.png")
    title = kwargs.get("title", f"{prop_x} vs {prop_y}")
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
        x_vals = [m.get(prop_x, 0) for m in materials]
        y_vals = [m.get(prop_y, 0) for m in materials]
        labels = [m.get("name", m.get("formula", f"M{i}")) for i, m in enumerate(materials)]
        fig, ax = plt.subplots(figsize=(8, 6))
        ax.scatter(x_vals, y_vals, s=60, alpha=0.7)
        for i, label in enumerate(labels):
            ax.annotate(label, (x_vals[i], y_vals[i]), fontsize=8, ha="left")
        ax.set_xlabel(prop_x)
        ax.set_ylabel(prop_y)
        ax.set_title(title)
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)
        return {"success": True, "path": output_path}
    except ImportError:
        return {"error": "matplotlib not installed. Install with: pip install matplotlib"}
    except Exception as e:
        return {"error": str(e)}


def _plot_property_distribution(**kwargs) -> dict:
    values = kwargs["values"]
    prop_name = kwargs.get("property_name", "property")
    output_path = kwargs.get("output_path", "distribution.png")
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
        fig, ax = plt.subplots(figsize=(8, 5))
        ax.hist(values, bins=min(30, max(5, len(values) // 3)), alpha=0.7, edgecolor="black")
        ax.set_xlabel(prop_name)
        ax.set_ylabel("Count")
        ax.set_title(f"Distribution of {prop_name}")
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)
        return {"success": True, "path": output_path}
    except ImportError:
        return {"error": "matplotlib not installed. Install with: pip install matplotlib"}
    except Exception as e:
        return {"error": str(e)}


def create_visualization_tools(registry: ToolRegistry) -> None:
    registry.register(Tool(
        name="plot_materials_comparison",
        description="Create a scatter plot comparing materials on two properties. Saves as PNG.",
        input_schema={"type": "object", "properties": {
            "materials": {"type": "array", "items": {"type": "object"}, "description": "List of material dicts with property values and name/formula"},
            "property_x": {"type": "string"}, "property_y": {"type": "string"},
            "output_path": {"type": "string"}, "title": {"type": "string"}},
            "required": ["materials", "property_x", "property_y"]},
        func=_plot_materials_comparison))
    registry.register(Tool(
        name="plot_property_distribution",
        description="Create a histogram showing the distribution of a material property.",
        input_schema={"type": "object", "properties": {
            "values": {"type": "array", "items": {"type": "number"}, "description": "Numeric values to plot"},
            "property_name": {"type": "string"}, "output_path": {"type": "string"}},
            "required": ["values"]},
        func=_plot_property_distribution))
