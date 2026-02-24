"""Review skill: rule-based validation + structured LLM review prompt."""

from app.skills.base import Skill, SkillStep


def _review_dataset(**kwargs) -> dict:
    """Review dataset quality: run validations, build findings and review prompt."""
    dataset_name = kwargs.get("dataset_name")
    include_llm_prompt = kwargs.get("include_llm_prompt", True)

    if not dataset_name:
        return {"error": "dataset_name is required"}

    from app.data.store import DataStore

    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found in DataStore"}

    from app.validation.rules import validate_dataset

    results = validate_dataset(df)

    # Build structured findings with severity
    findings: list[dict] = []

    for v in results["constraint_violations"]:
        findings.append({
            "severity": "critical",
            "type": "constraint_violation",
            "message": f"{v['column']} = {v['value']} violates {v['constraint']} (row {v['row']})",
        })

    for o in results["outliers"]:
        findings.append({
            "severity": "warning",
            "type": "outlier",
            "message": f"{o['column']} = {o['value']} is an outlier (z={o['z_score']}, row {o['row']})",
        })

    below_50 = results["completeness"]["columns_below_50pct"]
    for col in below_50:
        pct = results["completeness"]["column_completeness"].get(col, 0)
        findings.append({
            "severity": "info",
            "type": "low_completeness",
            "message": f"{col} has only {pct:.0%} non-null values",
        })

    # Severity counts
    severity_counts = {"critical": 0, "warning": 0, "info": 0}
    for f in findings:
        severity_counts[f["severity"]] += 1

    # Quality score: 1.0 - (critical*0.1 + warning*0.02), clamped [0, 1]
    quality_score = 1.0 - (severity_counts["critical"] * 0.1 + severity_counts["warning"] * 0.02)
    quality_score = max(0.0, min(1.0, quality_score))

    result = {
        "dataset_name": dataset_name,
        "findings": findings,
        "severity_counts": severity_counts,
        "quality_score": round(quality_score, 2),
    }

    # Build review prompt for the agent LLM
    if include_llm_prompt:
        n_outliers = len(results["outliers"])
        n_violations = len(results["constraint_violations"])
        completeness = results["completeness"]["overall_completeness"]

        formatted_findings = "\n".join(
            f"  - [{f['severity'].upper()}] {f['message']}" for f in findings
        ) if findings else "  No issues found."

        result["review_prompt"] = (
            f"Review this materials dataset:\n"
            f"- {len(df)} materials, {len(df.columns)} properties\n"
            f"- {n_outliers} outliers detected, {n_violations} constraint violations\n"
            f"- Overall completeness: {completeness:.0%}\n"
            f"\n"
            f"Key findings:\n"
            f"{formatted_findings}\n"
            f"\n"
            f"Please assess:\n"
            f"1. Are the outliers genuine anomalies or measurement errors?\n"
            f"2. Do constraint violations indicate data quality issues?\n"
            f"3. Are there any concerning patterns in the data?\n"
            f"4. Overall data quality rating (excellent/good/fair/poor)"
        )

    return result


REVIEW_SKILL = Skill(
    name="review_dataset",
    description=(
        "Review dataset quality: run validations, generate structured findings "
        "and a review prompt for scientific assessment."
    ),
    steps=[
        SkillStep("validate", "Run rule-based validation", "validate_dataset"),
        SkillStep("build_review", "Build structured review findings", "internal"),
        SkillStep("generate_prompt", "Generate review prompt for LLM assessment", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "dataset_name": {
                "type": "string",
                "description": "Name of the dataset in DataStore",
            },
            "include_llm_prompt": {
                "type": "boolean",
                "description": "Include a review prompt for LLM assessment (default: true)",
            },
        },
        "required": ["dataset_name"],
    },
    func=_review_dataset,
    category="review",
)
