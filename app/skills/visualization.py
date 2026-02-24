"""Dataset visualization skill."""

from pathlib import Path

from app.config.preferences import UserPreferences
from app.skills.base import Skill, SkillStep


def _visualize_dataset(**kwargs) -> dict:
    """Generate plots for numeric columns in a dataset."""
    dataset_name = kwargs["dataset_name"]
    properties = kwargs.get("properties")
    chart_types = kwargs.get("chart_types", ["distribution", "comparison"])
    output_dir = kwargs.get("output_dir")

    prefs = UserPreferences.load()
    output_dir = output_dir or prefs.output_dir

    from app.data.store import DataStore

    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found"}

    # Determine numeric columns
    exclude = {"source_id", "provider", "elements", "space_group",
               "material_id", "is_metal"}
    if properties:
        numeric_cols = [p for p in properties if p in df.columns]
    else:
        numeric_cols = [
            c for c in df.columns
            if df[c].dtype in ("float64", "float32", "int64", "int32")
            and c not in exclude
        ]

    if not numeric_cols:
        return {"error": "No numeric columns found to visualize"}

    out = Path(output_dir)
    out.mkdir(parents=True, exist_ok=True)

    from app.tools.visualization import (
        _plot_materials_comparison,
        _plot_property_distribution,
    )

    plots = []

    # Distribution plots
    if "distribution" in chart_types:
        for col in numeric_cols:
            values = df[col].dropna().tolist()
            if not values:
                continue
            path = str(out / f"{dataset_name}_{col}_dist.png")
            result = _plot_property_distribution(
                values=values, property_name=col, output_path=path
            )
            if result.get("success"):
                plots.append(result["path"])

    # Comparison plots (pairwise)
    if "comparison" in chart_types and len(numeric_cols) >= 2:
        for i, col_x in enumerate(numeric_cols):
            for col_y in numeric_cols[i + 1 :]:
                formula_col = None
                for cand in ("formula", "formula_pretty"):
                    if cand in df.columns:
                        formula_col = cand
                        break
                materials = []
                for _, row in df.iterrows():
                    m = {col_x: row.get(col_x), col_y: row.get(col_y)}
                    if formula_col:
                        m["formula"] = row.get(formula_col, "")
                    materials.append(m)
                path = str(out / f"{dataset_name}_{col_x}_vs_{col_y}.png")
                result = _plot_materials_comparison(
                    materials=materials,
                    property_x=col_x,
                    property_y=col_y,
                    output_path=path,
                )
                if result.get("success"):
                    plots.append(result["path"])

    return {
        "dataset_name": dataset_name,
        "plots": plots,
        "columns_plotted": numeric_cols,
    }


VISUALIZE_SKILL = Skill(
    name="visualize_dataset",
    description=(
        "Generate distribution histograms and comparison scatter plots "
        "for numeric columns in a dataset. Auto-detects plottable columns."
    ),
    steps=[
        SkillStep("load_dataset", "Load dataset from DataStore", "internal"),
        SkillStep(
            "distributions",
            "Plot property distributions",
            "plot_property_distribution",
        ),
        SkillStep(
            "comparisons",
            "Plot pairwise property comparisons",
            "plot_materials_comparison",
            optional=True,
        ),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "dataset_name": {
                "type": "string",
                "description": "Name of the dataset in DataStore",
            },
            "properties": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Columns to plot (auto-detected if omitted)",
            },
            "chart_types": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Chart types: distribution, comparison (default both)",
            },
            "output_dir": {
                "type": "string",
                "description": "Directory for output plots (default from preferences)",
            },
        },
        "required": ["dataset_name"],
    },
    func=_visualize_dataset,
    category="visualization",
)
