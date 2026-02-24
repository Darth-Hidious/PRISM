"""Property selection tool: inspect datasets for predictable properties."""

from app.tools.base import Tool, ToolRegistry


def _list_predictable_properties(**kwargs) -> dict:
    """List numeric properties in a dataset that can be predicted."""
    dataset_name = kwargs.get("dataset_name")
    if not dataset_name:
        return {"error": "dataset_name is required"}

    from app.data.store import DataStore

    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found in DataStore"}

    # Same exclusion logic as prediction.py
    exclude = {
        "source_id", "provider", "formula", "elements", "space_group",
        "material_id", "formula_pretty", "is_metal",
    }

    has_formula = any(c in df.columns for c in ("formula", "formula_pretty"))

    predictable = []
    for col in df.columns:
        if col in exclude or col.startswith("predicted_"):
            continue
        if df[col].dtype in ("float64", "float32", "int64", "int32"):
            non_null = int(df[col].notna().sum())
            predictable.append({
                "property": col,
                "non_null_count": non_null,
                "coverage": round(non_null / len(df), 2) if len(df) > 0 else 0.0,
                "mean": round(float(df[col].dropna().mean()), 4) if non_null > 0 else None,
                "has_trained_model": False,
            })

    # Check existing predictions
    already_predicted = [
        c.replace("predicted_", "") for c in df.columns if c.startswith("predicted_")
    ]

    # Check existing models
    try:
        from app.ml.registry import ModelRegistry

        registry = ModelRegistry()
        models = registry.list_models()
        model_props = {m.get("property") for m in models}
        properties_with_models = []
        for p in predictable:
            if p["property"] in model_props:
                p["has_trained_model"] = True
                properties_with_models.append(p["property"])
    except Exception:
        properties_with_models = []

    return {
        "dataset_name": dataset_name,
        "total_rows": len(df),
        "has_formula_column": has_formula,
        "predictable_properties": predictable,
        "already_predicted": already_predicted,
        "properties_with_models": properties_with_models,
    }


def create_property_selection_tools(registry: ToolRegistry) -> None:
    """Register property selection tools."""
    registry.register(Tool(
        name="list_predictable_properties",
        description=(
            "List numeric properties in a dataset that can be predicted with ML. "
            "Shows coverage, existing models, and already-predicted columns."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "dataset_name": {
                    "type": "string",
                    "description": "Name of the dataset in DataStore",
                },
            },
            "required": ["dataset_name"],
        },
        func=_list_predictable_properties,
    ))
