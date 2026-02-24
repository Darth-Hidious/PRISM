"""Materials discovery master skill: end-to-end pipeline."""

from app.skills.base import Skill, SkillStep


def _materials_discovery(**kwargs) -> dict:
    """Chain acquire → predict → visualize → report."""
    elements = kwargs["elements"]
    properties = kwargs.get("properties")
    sources = kwargs.get("sources")
    max_results = kwargs.get("max_results")
    title = kwargs.get("title", f"Materials Discovery: {', '.join(elements)}")

    dataset_name = kwargs.get(
        "dataset_name",
        "_".join(e.lower() for e in elements) + "_discovery",
    )

    results = {}

    # Step 1: Acquire
    from app.skills.acquisition import _acquire_materials

    acq = _acquire_materials(
        elements=elements,
        sources=sources,
        max_results=max_results,
        dataset_name=dataset_name,
    )
    results["acquisition"] = acq
    if "error" in acq:
        return {"error": f"Acquisition failed: {acq['error']}", "results": results}

    # Step 2: Predict (graceful — continues on failure)
    from app.skills.prediction import _predict_properties

    try:
        pred = _predict_properties(
            dataset_name=dataset_name,
            properties=properties,
        )
        results["prediction"] = pred
    except Exception as e:
        results["prediction"] = {"error": str(e)}

    # Step 3: Visualize (graceful)
    from app.skills.visualization import _visualize_dataset

    try:
        viz = _visualize_dataset(dataset_name=dataset_name)
        results["visualization"] = viz
    except Exception as e:
        results["visualization"] = {"error": str(e)}

    # Step 4: Report
    from app.skills.reporting import _generate_report

    try:
        report = _generate_report(dataset_name=dataset_name, title=title)
        results["report"] = report
    except Exception as e:
        results["report"] = {"error": str(e)}

    return {
        "dataset_name": dataset_name,
        "title": title,
        "results": results,
    }


DISCOVER_SKILL = Skill(
    name="materials_discovery",
    description=(
        "End-to-end materials discovery pipeline: acquire data from "
        "multiple sources, predict properties with ML, generate "
        "visualizations, and compile a report. Gracefully continues "
        "if individual steps fail."
    ),
    steps=[
        SkillStep("acquire", "Collect materials data", "acquire_materials"),
        SkillStep(
            "predict",
            "Predict properties with ML",
            "predict_properties",
            optional=True,
        ),
        SkillStep(
            "visualize",
            "Generate plots",
            "visualize_dataset",
            optional=True,
        ),
        SkillStep("report", "Compile report", "generate_report"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "elements": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Chemical elements to search for",
            },
            "properties": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Properties to predict (auto-detected if omitted)",
            },
            "sources": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Data sources: optimade, mp",
            },
            "max_results": {
                "type": "integer",
                "description": "Max results per source",
            },
            "title": {
                "type": "string",
                "description": "Report title",
            },
        },
        "required": ["elements"],
    },
    func=_materials_discovery,
    category="discovery",
)
