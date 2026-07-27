"""Materials selection skill: filter, sort, and select top candidates."""

from app.tools.skills.base import Skill, SkillStep


def _select_materials(**kwargs) -> dict:
    """Filter and rank materials from a dataset."""
    dataset_name = kwargs["dataset_name"]
    criteria = kwargs.get("criteria", {})
    sort_by = kwargs.get("sort_by")
    descending = bool(kwargs.get("descending", False))
    top_n = kwargs.get("top_n", 10)
    output_name = kwargs.get("output_name")

    from app.tools.data_collectors.store import DataStore

    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found"}

    n_before = len(df)

    # Apply criteria filters: {col}_min, {col}_max
    ignored_criteria = []
    for key, value in criteria.items():
        if key.endswith("_min"):
            col = key[:-4]
            if col in df.columns:
                df = df[df[col] >= value]
            else:
                ignored_criteria.append(key)
        elif key.endswith("_max"):
            col = key[:-4]
            if col in df.columns:
                df = df[df[col] <= value]
            else:
                ignored_criteria.append(key)
        else:
            ignored_criteria.append(key)
    # A dropped filter silently returns candidates that do not meet the
    # stated criteria — refuse instead.
    if ignored_criteria:
        return {
            "error": (
                f"Criteria {ignored_criteria} do not match any column in "
                f"'{dataset_name}' (expected '<column>_min' / '<column>_max'). "
                f"Available columns: {list(df.columns)}"
            )
        }

    if df.empty:
        return {"error": "No materials match the given criteria"}

    # Sort. A sort_by naming a column that isn't there used to be ignored,
    # so "top 10 by band_gap" quietly returned the first 10 rows in file
    # order — ranked-looking output with no ranking in it.
    if sort_by:
        if sort_by not in df.columns:
            return {
                "error": (
                    f"Cannot sort by '{sort_by}' — no such column in "
                    f"'{dataset_name}'. Available: {list(df.columns)}"
                )
            }
        df = df.sort_values(sort_by, ascending=not descending).reset_index(drop=True)

    n_matching = len(df)

    # Take top N
    selected = df.head(top_n)

    # Save selected subset
    if not output_name:
        output_name = f"{dataset_name}_selected"
    store.save(selected, output_name)

    return {
        "dataset_name": output_name,
        "selected_count": len(selected),
        # Was len(df) AFTER filtering, so "original" equalled "matching"
        # whenever top_n exceeded the match count.
        "original_count": n_before,
        "matching_count": n_matching,
        "sorted_by": sort_by,
        "sort_order": "descending" if descending else "ascending",
        "ranked": bool(sort_by),
        "columns": list(selected.columns),
    }


SELECT_SKILL = Skill(
    name="select_materials",
    description=(
        "Filter and rank materials from a dataset by criteria "
        "(min/max thresholds), sort by a property, and save the "
        "top N candidates as a new dataset. Use this when you need to "
        "narrow a dataset to the best candidates. Returns the new "
        "dataset name and selected/total counts."
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
                "description": (
                    "Column to rank by. Must exist in the dataset — an "
                    "unknown column is an error, not an unranked result."
                ),
            },
            "descending": {
                "type": "boolean",
                "description": (
                    "Rank highest-first (default false = lowest-first). "
                    "'Top N' is ambiguous, so state which end you want."
                ),
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
        "additionalProperties": False,
    },
    func=_select_materials,
    category="selection",
)
