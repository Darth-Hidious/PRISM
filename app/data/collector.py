"""Collect materials data from OPTIMADE and Materials Project."""
from typing import Dict, List, Optional
from app.config.providers import FALLBACK_PROVIDERS
from app.data.base_collector import DataCollector


class OPTIMADECollector(DataCollector):
    name = "optimade"

    def __init__(self, providers: Optional[List[Dict]] = None):
        self.providers = providers or FALLBACK_PROVIDERS

    def collect(self, filter_string: str, max_per_provider: int = 100, provider_ids: Optional[List[str]] = None) -> List[Dict]:
        try:
            from optimade.client import OptimadeClient
        except ImportError:
            return []
        base_urls = []
        provider_map = {}
        for p in self.providers:
            if provider_ids is None or p["id"] in provider_ids:
                base_urls.append(p["base_url"])
                provider_map[p["base_url"]] = p["id"]
        try:
            client = OptimadeClient(base_urls=base_urls, max_results_per_provider=max_per_provider)
            raw = client.get(filter_string)
        except Exception:
            return []
        # Response format: {endpoint: {filter: {url: {data: [entries]}}}}
        results = []
        for endpoint, filters in raw.items():
            if not isinstance(filters, dict):
                continue
            for filter_key, providers_data in filters.items():
                if not isinstance(providers_data, dict):
                    continue
                for provider_url, response in providers_data.items():
                    provider_id = provider_map.get(provider_url, provider_url)
                    entries = []
                    if isinstance(response, dict):
                        entries = response.get("data", [])
                    elif isinstance(response, list):
                        entries = response
                    for entry in entries:
                        if not isinstance(entry, dict):
                            continue
                        attrs = entry.get("attributes", {})
                        results.append({
                            "source_id": f"{provider_id}:{entry.get('id', '')}",
                            "provider": provider_id,
                            "formula": attrs.get("chemical_formula_descriptive", ""),
                            "elements": attrs.get("elements", []),
                            "nelements": attrs.get("nelements"),
                            "space_group": attrs.get("space_group_symbol", ""),
                            "lattice_vectors": attrs.get("lattice_vectors"),
                        })
        return results


class MPCollector(DataCollector):
    name = "mp"

    def collect(self, formula: str = None, elements: List[str] = None, max_results: int = 50) -> List[Dict]:
        import os
        api_key = os.getenv("MP_API_KEY")
        if not api_key:
            return []
        try:
            from mp_api.client import MPRester
            with MPRester(api_key) as mpr:
                kwargs = {"fields": ["material_id", "formula_pretty", "band_gap", "formation_energy_per_atom", "energy_above_hull", "density", "is_metal"]}
                if formula:
                    kwargs["formula"] = formula
                elif elements:
                    kwargs["elements"] = elements
                docs = mpr.materials.summary.search(**kwargs)
                results = []
                for doc in docs[:max_results]:
                    entry = {}
                    for field in kwargs["fields"]:
                        val = getattr(doc, field, None)
                        if val is not None:
                            entry[field] = val if isinstance(val, (str, int, float, bool)) else str(val)
                    results.append(entry)
                return results
        except Exception:
            return []
