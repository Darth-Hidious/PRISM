"""Visualization tools for materials data.

The unified `plot` tool replaces the prior `plot_materials_comparison`,
`plot_property_distribution`, and `plot_correlation_matrix` tools with
a single entry point dispatched by `kind`. Each kind owns its own
required-args contract, validated up front before any matplotlib work.
"""
from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# Per-kind handlers
# ---------------------------------------------------------------------------

def _kind_materials_comparison(**kw) -> dict:
    materials = kw.get("materials")
    prop_x = kw.get("property_x")
    prop_y = kw.get("property_y")
    if not materials or not prop_x or not prop_y:
        return {"error": "kind='materials_comparison' requires `materials`, `property_x`, `property_y`"}

    output_path = kw.get("output_path", "comparison.png")
    title = kw.get("title", f"{prop_x} vs {prop_y}")
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
        return {"success": True, "path": output_path, "kind": "materials_comparison"}
    except ImportError:
        return {"error": "matplotlib not installed"}
    except Exception as e:
        return {"error": str(e)}


def _kind_property_distribution(**kw) -> dict:
    values = kw.get("values")
    if values is None:
        return {"error": "kind='property_distribution' requires `values` list"}

    prop_name = kw.get("property_name", "property")
    output_path = kw.get("output_path", "distribution.png")
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
        return {"success": True, "path": output_path, "kind": "property_distribution"}
    except ImportError:
        return {"error": "matplotlib not installed"}
    except Exception as e:
        return {"error": str(e)}


def _kind_correlation_matrix(**kw) -> dict:
    dataset_name = kw.get("dataset_name")
    if not dataset_name:
        return {"error": "kind='correlation_matrix' requires `dataset_name`"}

    from app.tools.data_collectors.store import DataStore
    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found in DataStore"}

    columns = kw.get("columns")
    output_path = kw.get("output_path") or f"{dataset_name}_correlation.png"

    numeric = df.select_dtypes(include=["float64", "float32", "int64", "int32"])
    if columns:
        numeric = numeric[[c for c in columns if c in numeric.columns]]

    if numeric.shape[1] < 2:
        return {"error": "Need at least 2 numeric columns for correlation matrix"}

    corr = numeric.corr()

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
        ax.set_title(f"Correlation Matrix — {dataset_name}")
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)
    except ImportError:
        return {"error": "matplotlib not installed"}
    except Exception as e:
        return {"error": str(e)}

    # Top correlations excluding self-correlation
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
        "kind": "correlation_matrix",
        "dataset_name": dataset_name,
        "n_properties": len(corr.columns),
        "top_correlations": pairs[:10],
    }


_DISPATCH = {
    "materials_comparison":  _kind_materials_comparison,
    "property_distribution": _kind_property_distribution,
    "correlation_matrix":    _kind_correlation_matrix,
}


# Internal aliases preserved for tests / skills that import the
# private helpers directly. Public surface is the unified `plot` tool.
_plot_materials_comparison = _kind_materials_comparison
_plot_property_distribution = _kind_property_distribution
_plot_correlation_matrix = _kind_correlation_matrix


def _plot(**kwargs) -> dict:
    kind = kwargs.pop("kind", None)
    if not kind:
        return {
            "error": f"Missing 'kind'. Valid: {list(_DISPATCH.keys())}",
            "hint": (
                "plot(kind='property_distribution', values=[...]) or "
                "plot(kind='correlation_matrix', dataset_name='alloys')"
            ),
        }
    handler = _DISPATCH.get(kind)
    if not handler:
        return {"error": f"Unknown kind '{kind}'. Valid: {list(_DISPATCH.keys())}"}
    try:
        return handler(**kwargs)
    except Exception as e:
        return {"error": str(e), "kind": kind}


# ---------------------------------------------------------------------------
# Description + schema
# ---------------------------------------------------------------------------

_DESCRIPTION = (
    "Generate a PNG plot of materials data. ONE tool, three kinds:\n"
    "  • kind='materials_comparison' — scatter plot of property_x vs "
    "property_y across a list of materials. Requires `materials` (list of "
    "dicts), `property_x`, `property_y`. Use when comparing N materials "
    "on two properties.\n"
    "  • kind='property_distribution' — histogram of one property's "
    "values. Requires `values` (list of numbers). Optional `property_name` "
    "for labelling. Use to see the spread of one property.\n"
    "  • kind='correlation_matrix' — heatmap of pairwise correlations "
    "across numeric columns in a stored dataset. Requires `dataset_name` "
    "(must already be in the DataStore — use import_dataset first). "
    "Optional `columns` to restrict to specific fields.\n"
    "All kinds output a PNG; pass `output_path` to override the default. "
    "Returns {success, path}. NOT for crystal-structure visualization "
    "(no 3D viewer here) and NOT for interactive plots."
)


_SCHEMA = {
    "type": "object",
    "properties": {
        "kind": {
            "type": "string",
            "enum": list(_DISPATCH.keys()),
            "description": "Which chart type to render.",
        },
        # materials_comparison
        "materials": {
            "type": "array",
            "items": {"type": "object"},
            "description": "List of material dicts for kind='materials_comparison'.",
        },
        "property_x": {
            "type": "string",
            "description": "X-axis property name for kind='materials_comparison'.",
        },
        "property_y": {
            "type": "string",
            "description": "Y-axis property name for kind='materials_comparison'.",
        },
        # property_distribution
        "values": {
            "type": "array",
            "items": {"type": "number"},
            "description": "Numeric values for kind='property_distribution'.",
        },
        "property_name": {
            "type": "string",
            "description": "Property label for kind='property_distribution'.",
        },
        # correlation_matrix
        "dataset_name": {
            "type": "string",
            "description": "DataStore dataset name for kind='correlation_matrix'.",
        },
        "columns": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Restrict kind='correlation_matrix' to these columns.",
        },
        # shared
        "output_path": {
            "type": "string",
            "description": "Output PNG path. Default depends on kind.",
        },
        "title": {
            "type": "string",
            "description": "Plot title (kind='materials_comparison').",
        },
    },
    "required": ["kind"],
    "additionalProperties": False,
}


def create_visualization_tools(registry: ToolRegistry) -> None:
    """Register the unified `plot` tool (replaces 3 prior tools).

    Function name preserved (`create_visualization_tools`) for
    bootstrap.py backward-compatibility.
    """
    registry.register(Tool(
        name="plot",
        description=_DESCRIPTION,
        input_schema=_SCHEMA,
        func=_plot,
    ))
