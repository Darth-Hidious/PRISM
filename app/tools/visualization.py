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


def _plot_correlation_matrix(**kwargs) -> dict:
    """Plot correlation heatmap for numeric columns in a dataset."""
    dataset_name = kwargs.get("dataset_name")
    if not dataset_name:
        return {"error": "dataset_name is required"}
    columns = kwargs.get("columns")
    output_path = kwargs.get("output_path")

    from app.data.store import DataStore

    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found in DataStore"}

    numeric = df.select_dtypes(include=["float64", "float32", "int64", "int32"])
    if columns:
        numeric = numeric[[c for c in columns if c in numeric.columns]]

    if numeric.shape[1] < 2:
        return {"error": "Need at least 2 numeric columns for correlation matrix"}

    corr = numeric.corr()

    if not output_path:
        output_path = f"{dataset_name}_correlation.png"

    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        fig, ax = plt.subplots(figsize=(max(8, numeric.shape[1]), max(6, numeric.shape[1] * 0.8)))
        im = ax.imshow(corr.values, cmap="RdBu_r", vmin=-1, vmax=1, aspect="auto")
        ax.set_xticks(range(len(corr.columns)))
        ax.set_yticks(range(len(corr.columns)))
        ax.set_xticklabels(corr.columns, rotation=45, ha="right", fontsize=8)
        ax.set_yticklabels(corr.columns, fontsize=8)
        fig.colorbar(im)
        ax.set_title(f"Correlation Matrix \u2014 {dataset_name}")
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)
    except ImportError:
        return {"error": "matplotlib not installed. Install with: pip install matplotlib"}
    except Exception as e:
        return {"error": str(e)}

    # Return top correlations (excluding self-correlation)
    pairs = []
    for i in range(len(corr.columns)):
        for j in range(i + 1, len(corr.columns)):
            pairs.append({
                "property_a": corr.columns[i],
                "property_b": corr.columns[j],
                "correlation": round(float(corr.iloc[i, j]), 4),
            })
    pairs.sort(key=lambda p: abs(p["correlation"]), reverse=True)

    return {
        "success": True,
        "path": output_path,
        "dataset_name": dataset_name,
        "n_properties": len(corr.columns),
        "top_correlations": pairs[:10],
    }


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
    registry.register(Tool(
        name="plot_correlation_matrix",
        description="Plot a correlation heatmap for numeric properties in a dataset.",
        input_schema={"type": "object", "properties": {
            "dataset_name": {"type": "string", "description": "Dataset name in DataStore"},
            "columns": {"type": "array", "items": {"type": "string"}, "description": "Columns to include (all numeric if omitted)"},
            "output_path": {"type": "string", "description": "Output PNG path"}},
            "required": ["dataset_name"]},
        func=_plot_correlation_matrix))
