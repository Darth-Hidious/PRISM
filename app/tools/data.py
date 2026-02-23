"""Data tools: OPTIMADE search and Materials Project queries."""
import json
from typing import List, Optional
from app.tools.base import Tool, ToolRegistry


def _search_optimade(**kwargs) -> dict:
    filter_string = kwargs["filter_string"]
    providers = kwargs.get("providers", None)
    max_results = kwargs.get("max_results", 10)
    try:
        from optimade.client import OptimadeClient
        from app.config.providers import FALLBACK_PROVIDERS
        if providers:
            base_urls = {p["id"]: p["base_url"] for p in FALLBACK_PROVIDERS if p["id"] in providers}
        else:
            base_urls = {p["id"]: p["base_url"] for p in FALLBACK_PROVIDERS}
        client = OptimadeClient(base_urls=base_urls, max_results_per_provider=max_results)
        raw = client.get(filter_string)
        results = []
        for provider_id, provider_data in raw.items():
            if isinstance(provider_data, dict):
                entries = provider_data.get("data", [])
            elif isinstance(provider_data, list):
                entries = provider_data
            else:
                continue
            for entry in entries[:max_results]:
                attrs = entry.get("attributes", {}) if isinstance(entry, dict) else {}
                results.append({"id": entry.get("id", ""), "provider": provider_id, "formula": attrs.get("chemical_formula_descriptive", ""), "elements": attrs.get("elements", []), "space_group": attrs.get("space_group_symbol", "")})
        return {"results": results, "count": len(results), "filter": filter_string}
    except Exception as e:
        return {"error": str(e), "filter": filter_string}


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


def create_data_tools(registry: ToolRegistry) -> None:
    registry.register(Tool(
        name="search_optimade",
        description="Search materials databases using OPTIMADE filter syntax. Queries 6 providers: Materials Project, OQMD, COD, AFLOW, JARVIS, Materials Cloud. Use OPTIMADE filter syntax like: elements HAS ALL \"Si\",\"O\" AND nelements=2",
        input_schema={"type": "object", "properties": {
            "filter_string": {"type": "string", "description": "OPTIMADE filter string"},
            "providers": {"type": "array", "items": {"type": "string"}, "description": "Optional provider IDs"},
            "max_results": {"type": "integer", "description": "Max results per provider. Default 10.", "default": 10}},
            "required": ["filter_string"]},
        func=_search_optimade))
    registry.register(Tool(
        name="query_materials_project",
        description="Query Materials Project for detailed material properties like band gap, formation energy, bulk modulus. Requires MP_API_KEY.",
        input_schema={"type": "object", "properties": {
            "formula": {"type": "string", "description": "Chemical formula, e.g. 'LiCoO2'"},
            "material_id": {"type": "string", "description": "Materials Project ID, e.g. 'mp-1234'"},
            "properties": {"type": "array", "items": {"type": "string"}, "description": "Properties to retrieve."}}},
        func=_query_materials_project))
