"""Data tools: OPTIMADE search and Materials Project queries."""
import csv
import json
import os
from datetime import datetime
from typing import List, Optional
from app.tools.base import Tool, ToolRegistry


def _search_materials(**kwargs) -> dict:
    """Search materials via the PRISM federated search engine."""
    import asyncio
    from app.search import SearchEngine, MaterialSearchQuery, PropertyRange
    from app.search.providers.registry import build_registry

    try:
        elements = kwargs.get("elements")
        # Auto-infer n_elements from elements list when not explicitly set
        n_elements = None
        if kwargs.get("n_elements_min") or kwargs.get("n_elements_max"):
            n_elements = PropertyRange(min=kwargs.get("n_elements_min"), max=kwargs.get("n_elements_max"))
        elif elements and not kwargs.get("formula"):
            # Constrain to exactly these elements (avoid Fe-C returning Fe-C-Lu)
            n_elements = PropertyRange(min=len(elements), max=len(elements))

        query = MaterialSearchQuery(
            elements=elements,
            formula=kwargs.get("formula"),
            n_elements=n_elements,
            band_gap=PropertyRange(min=kwargs.get("band_gap_min"), max=kwargs.get("band_gap_max")) if kwargs.get("band_gap_min") is not None or kwargs.get("band_gap_max") is not None else None,
            space_group=kwargs.get("space_group"),
            crystal_system=kwargs.get("crystal_system"),
            providers=kwargs.get("providers"),
            limit=kwargs.get("limit", 20),
        )
        registry = build_registry()
        engine = SearchEngine(registry=registry)
        result = asyncio.run(engine.search(query))

        materials = []
        for m in result.materials:
            entry = {
                "id": m.id, "formula": m.formula, "elements": m.elements,
                "sources": m.sources,
            }
            if m.band_gap:
                entry["band_gap"] = {"value": m.band_gap.value, "unit": m.band_gap.unit, "source": m.band_gap.source}
            if m.space_group:
                entry["space_group"] = m.space_group.value
            if m.formation_energy:
                entry["formation_energy"] = {"value": m.formation_energy.value, "unit": m.formation_energy.unit}
            materials.append(entry)

        return {
            "results": materials,
            "count": result.total_count,
            "search_time_ms": result.search_time_ms,
            "cached": result.cached,
            "warnings": result.warnings,
            "providers_queried": [{"id": log.provider_id, "status": log.status, "results": log.result_count} for log in result.query_log],
        }
    except Exception as e:
        return {"error": str(e)}


def _query_materials_project(**kwargs) -> dict:
    formula = kwargs.get("formula")
    material_id = kwargs.get("material_id")
    properties = kwargs.get("properties", ["material_id", "formula_pretty", "band_gap", "formation_energy_per_atom", "energy_above_hull", "is_metal"])
    try:
        from mp_api.client import MPRester
        import os
        api_key = os.getenv("MP_API_KEY")
        if not api_key:
            return {"error": "MP_API_KEY not set. Configure with: prism configure --mp-api-key YOUR_KEY"}
        with MPRester(api_key) as mpr:
            if material_id:
                docs = mpr.materials.summary.search(material_ids=[material_id], fields=properties)
            elif formula:
                docs = mpr.materials.summary.search(formula=formula, fields=properties)
            else:
                return {"error": "Provide either 'formula' or 'material_id'"}
            results = []
            for doc in docs[:20]:
                entry = {}
                for prop in properties:
                    val = getattr(doc, prop, None)
                    if val is not None:
                        entry[prop] = val if isinstance(val, (str, int, float, bool)) else str(val)
                results.append(entry)
            return {"results": results, "count": len(results)}
    except Exception as e:
        return {"error": str(e)}


def _import_dataset(**kwargs) -> dict:
    """Import a local file (CSV, JSON, Parquet) into the PRISM DataStore."""
    import pandas as pd
    from pathlib import Path
    from app.data.store import DataStore

    file_path = kwargs["file_path"]
    dataset_name = kwargs.get("dataset_name")
    file_format = kwargs.get("file_format")

    p = Path(file_path)
    if not p.exists():
        return {"error": f"File not found: {file_path}"}

    fmt = file_format or p.suffix.lstrip(".")
    try:
        if fmt in ("csv", "tsv"):
            df = pd.read_csv(p, sep="\t" if fmt == "tsv" else ",")
        elif fmt == "json":
            df = pd.read_json(p)
        elif fmt in ("parquet", "pq"):
            df = pd.read_parquet(p)
        else:
            return {"error": f"Unsupported format: {fmt}. Use csv, json, or parquet."}
    except Exception as e:
        return {"error": f"Failed to read file: {e}"}

    name = dataset_name or p.stem
    store = DataStore()
    store.save(df, name)

    return {
        "dataset_name": name,
        "rows": len(df),
        "columns": list(df.columns),
        "source": str(p),
    }


def _export_results_csv(**kwargs) -> dict:
    results = kwargs.get("results", [])
    filename = kwargs.get("filename")
    if not results:
        return {"error": "No results to export."}
    if not filename:
        filename = f"prism_export_{datetime.now().strftime('%Y%m%d_%H%M%S')}.csv"
    try:
        fieldnames = list(results[0].keys())
        with open(filename, "w", newline="") as f:
            writer = csv.DictWriter(f, fieldnames=fieldnames, extrasaction="ignore")
            writer.writeheader()
            writer.writerows(results)
        return {"filename": filename, "rows": len(results), "columns": fieldnames}
    except Exception as e:
        return {"error": str(e)}


def create_data_tools(registry: ToolRegistry) -> None:
    registry.register(Tool(
        name="search_materials",
        description="Search materials databases via PRISM's federated search engine. Queries 20+ providers (NOMAD, MP, OQMD, COD, Alexandria, GNoME, etc.) in parallel with automatic cross-provider fusion.",
        input_schema={"type": "object", "properties": {
            "elements": {"type": "array", "items": {"type": "string"}, "description": "Must contain ALL these elements (e.g., ['Fe', 'O'])"},
            "formula": {"type": "string", "description": "Chemical formula (e.g., 'SiO2')"},
            "n_elements_min": {"type": "integer", "description": "Minimum number of elements"},
            "n_elements_max": {"type": "integer", "description": "Maximum number of elements"},
            "band_gap_min": {"type": "number", "description": "Minimum band gap in eV"},
            "band_gap_max": {"type": "number", "description": "Maximum band gap in eV"},
            "space_group": {"type": "string", "description": "Space group symbol (e.g., 'Fm-3m')"},
            "crystal_system": {"type": "string", "enum": ["cubic", "hexagonal", "tetragonal", "orthorhombic", "monoclinic", "triclinic", "trigonal"], "description": "Crystal system"},
            "providers": {"type": "array", "items": {"type": "string"}, "description": "Optional: only query these provider IDs"},
            "limit": {"type": "integer", "description": "Max results (default 20)", "default": 20}}},
        func=_search_materials))
    registry.register(Tool(
        name="query_materials_project",
        description="Query Materials Project for detailed material properties like band gap, formation energy, bulk modulus. Requires MP_API_KEY.",
        input_schema={"type": "object", "properties": {
            "formula": {"type": "string", "description": "Chemical formula, e.g. 'LiCoO2'"},
            "material_id": {"type": "string", "description": "Materials Project ID, e.g. 'mp-1234'"},
            "properties": {"type": "array", "items": {"type": "string"}, "description": "Properties to retrieve."}}},
        func=_query_materials_project))
    registry.register(Tool(
        name="export_results_csv",
        description="Export a list of result dictionaries to a CSV file. Use after gathering data to save results for the user.",
        input_schema={"type": "object", "properties": {
            "results": {"type": "array", "items": {"type": "object"}, "description": "List of result dictionaries to export"},
            "filename": {"type": "string", "description": "Output CSV filename. Auto-generated if omitted."}},
            "required": ["results"]},
        func=_export_results_csv))
    registry.register(Tool(
        name="import_dataset",
        description="Import a local CSV, JSON, or Parquet file into the PRISM DataStore as a named dataset.",
        input_schema={"type": "object", "properties": {
            "file_path": {"type": "string", "description": "Path to the file to import"},
            "dataset_name": {"type": "string", "description": "Name for the dataset (defaults to filename stem)"},
            "file_format": {"type": "string", "description": "File format override (csv, json, parquet). Auto-detected if omitted."}},
            "required": ["file_path"]},
        func=_import_dataset))
