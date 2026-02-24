"""Multi-source materials data acquisition skill."""

from app.config.preferences import UserPreferences
from app.skills.base import Skill, SkillStep


def _acquire_materials(**kwargs) -> dict:
    """Acquire materials from multiple sources, normalize, and store."""
    elements = kwargs.get("elements", [])
    filter_string = kwargs.get("filter_string")
    sources = kwargs.get("sources")
    max_results = kwargs.get("max_results")
    dataset_name = kwargs.get("dataset_name", "acquired_materials")

    prefs = UserPreferences.load()
    sources = sources or prefs.default_providers
    max_results = max_results or prefs.max_results_per_source

    if not filter_string and elements:
        elem_filter = " AND ".join(
            f'elements HAS "{e}"' for e in elements
        )
        filter_string = elem_filter

    all_records = []

    # Build a collector registry with built-in collectors
    from app.data.base_collector import CollectorRegistry
    from app.data.collector import OPTIMADECollector, MPCollector

    collector_reg = CollectorRegistry()
    try:
        collector_reg.register(OPTIMADECollector())
    except Exception:
        pass
    try:
        collector_reg.register(MPCollector())
    except Exception:
        pass

    # Collect from each requested source
    for src in sources:
        collector = None
        try:
            collector = collector_reg.get(src)
        except KeyError:
            continue
        try:
            if src == "optimade" and filter_string:
                records = collector.collect(
                    filter_string=filter_string, max_per_provider=max_results
                )
            elif src == "mp":
                records = collector.collect(
                    elements=elements if elements else None,
                    max_results=max_results,
                )
            else:
                records = collector.collect()
            all_records.extend(records)
        except Exception:
            pass

    if not all_records:
        return {"error": "No records collected from any source", "sources": sources}

    # Normalize and deduplicate
    from app.data.normalizer import normalize_records

    df = normalize_records(all_records)

    # Save to DataStore
    from app.data.store import DataStore

    store = DataStore()
    store.save(df, dataset_name)

    # Optional CSV export
    if prefs.output_format in ("csv", "both"):
        from pathlib import Path

        csv_dir = Path(prefs.output_dir)
        csv_dir.mkdir(parents=True, exist_ok=True)
        df.to_csv(csv_dir / f"{dataset_name}.csv", index=False)

    return {
        "dataset_name": dataset_name,
        "total_records": len(df),
        "columns": list(df.columns),
        "sources_queried": sources,
    }


ACQUIRE_SKILL = Skill(
    name="acquire_materials",
    description=(
        "Search and collect materials data from multiple sources "
        "(OPTIMADE, Materials Project), normalize records, and save "
        "as a named dataset for downstream analysis."
    ),
    steps=[
        SkillStep("collect_optimade", "Query OPTIMADE providers", "search_optimade"),
        SkillStep(
            "collect_mp",
            "Query Materials Project",
            "query_materials_project",
            optional=True,
        ),
        SkillStep("normalize", "Merge and deduplicate records", "internal"),
        SkillStep("store", "Save dataset to DataStore", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "elements": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Chemical elements to search for, e.g. ['W', 'Rh']",
            },
            "filter_string": {
                "type": "string",
                "description": "OPTIMADE filter string (auto-generated from elements if omitted)",
            },
            "sources": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Data sources to query: optimade, mp (default from preferences)",
            },
            "max_results": {
                "type": "integer",
                "description": "Max results per source (default from preferences)",
            },
            "dataset_name": {
                "type": "string",
                "description": "Name for the saved dataset (default: acquired_materials)",
            },
        },
        "required": ["elements"],
    },
    func=_acquire_materials,
    category="acquisition",
)
