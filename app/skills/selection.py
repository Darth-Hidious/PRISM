"""Materials selection skill: filter, sort, and select top candidates."""

from app.skills.base import Skill, SkillStep


def _select_materials(**kwargs) -> dict:
    """Filter and rank materials from a dataset."""
    dataset_name = kwargs["dataset_name"]
    criteria = kwargs.get("criteria", {})
    sort_by = kwargs.get("sort_by")
    top_n = kwargs.get("top_n", 10)
    output_name = kwargs.get("output_name")

    from app.data.store import DataStore

    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found"}

    # Apply criteria filters: {col}_min, {col}_max
    for key, value in criteria.items():
        if key.endswith("_min"):
            col = key[:-4]
            if col in df.columns:
                df = df[df[col] >= value]
        elif key.endswith("_max"):
            col = key[:-4]
            if col in df.columns:
                df = df[df[col] <= value]

    if df.empty:
        return {"error": "No materials match the given criteria"}

    # Sort
    if sort_by and sort_by in df.columns:
        df = df.sort_values(sort_by, ascending=True).reset_index(drop=True)

    # Take top N
    selected = df.head(top_n)

    # Save selected subset
    if not output_name:
        output_name = f"{dataset_name}_selected"
    store.save(selected, output_name)

    return {
        "dataset_name": output_name,
        "selected_count": len(selected),
        "original_count": len(df),
        "columns": list(selected.columns),
    }


SELECT_SKILL = Skill(
    name="select_materials",
    description=(
        "Filter and rank materials from a dataset by criteria "
        "(min/max thresholds), sort by a property, and save the "
        "top N candidates as a new dataset."
    ),
    steps=[
        SkillStep("load_dataset", "Load dataset from DataStore", "internal"),
        SkillStep("filter", "Apply min/max criteria filters", "internal"),
        SkillStep("sort", "Sort by specified property", "internal"),
        SkillStep("select_top", "Take top N candidates", "internal"),
        SkillStep("save", "Save selected subset to DataStore", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "dataset_name": {
                "type": "string",
                "description": "Name of the dataset in DataStore",
            },
            "criteria": {
                "type": "object",
                "description": "Filter criteria: keys like 'band_gap_min', 'band_gap_max' with numeric values",
            },
            "sort_by": {
                "type": "string",
                "description": "Column to sort results by",
            },
            "top_n": {
                "type": "integer",
                "description": "Number of top candidates to select (default: 10)",
            },
            "output_name": {
                "type": "string",
                "description": "Name for the output dataset (default: <input>_selected)",
            },
        },
        "required": ["dataset_name"],
    },
    func=_select_materials,
    category="selection",
)
