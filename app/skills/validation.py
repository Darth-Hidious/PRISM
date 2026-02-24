"""Validation skill: detect outliers, check constraints, score completeness."""

from app.skills.base import Skill, SkillStep


def _validate_dataset(**kwargs) -> dict:
    """Run rule-based validation on a stored dataset."""
    dataset_name = kwargs.get("dataset_name")
    z_threshold = kwargs.get("z_threshold", 3.0)

    if not dataset_name:
        return {"error": "dataset_name is required"}

    from app.data.store import DataStore

    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found in DataStore"}

    from app.validation.rules import validate_dataset

    results = validate_dataset(df, z_threshold=z_threshold)

    # Build human-readable summary
    n_outliers = len(results["outliers"])
    n_violations = len(results["constraint_violations"])
    completeness = results["completeness"]["overall_completeness"]
    below_50 = results["completeness"]["columns_below_50pct"]

    parts = []
    if n_outliers:
        cols = {f["column"] for f in results["outliers"]}
        parts.append(f"{n_outliers} outlier(s) in {', '.join(sorted(cols))}")
    if n_violations:
        cols = {f["column"] for f in results["constraint_violations"]}
        parts.append(f"{n_violations} constraint violation(s) in {', '.join(sorted(cols))}")
    if below_50:
        parts.append(f"columns below 50% completeness: {', '.join(below_50)}")

    summary = "; ".join(parts) if parts else "Dataset is clean — no issues found."

    return {
        "dataset_name": dataset_name,
        "outliers": results["outliers"],
        "constraint_violations": results["constraint_violations"],
        "completeness": results["completeness"],
        "total_findings": results["total_findings"],
        "summary": summary,
    }


VALIDATE_SKILL = Skill(
    name="validate_dataset",
    description="Validate a dataset: detect outliers, check physical constraints, score completeness.",
    steps=[
        SkillStep("load_dataset", "Load dataset from DataStore", "internal"),
        SkillStep("detect_outliers", "Flag statistical outliers", "internal"),
        SkillStep("check_constraints", "Check physical constraints", "internal"),
        SkillStep("score_completeness", "Score data completeness", "internal"),
        SkillStep("summarize", "Summarize validation findings", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "dataset_name": {
                "type": "string",
                "description": "Name of the dataset in DataStore",
            },
            "z_threshold": {
                "type": "number",
                "description": "Z-score threshold for outlier detection (default: 3.0)",
            },
        },
        "required": ["dataset_name"],
    },
    func=_validate_dataset,
    category="validation",
)
