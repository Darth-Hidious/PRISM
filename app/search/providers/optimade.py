"""OPTIMADE federation provider -- wraps OptimadeClient for a single endpoint."""
from __future__ import annotations

import logging
import time

from app.search.providers.base import Provider, ProviderCapabilities
from app.search.providers.endpoint import ProviderEndpoint
from app.search.query import MaterialSearchQuery
from app.search.result import Material, PropertyValue, ProviderQueryLog
from app.search.translator import QueryTranslator

logger = logging.getLogger(__name__)


class OptimadeProvider(Provider):
    """Single OPTIMADE endpoint provider."""

    def __init__(self, endpoint: ProviderEndpoint):
        self._endpoint = endpoint
        self.id = endpoint.id
        self.name = endpoint.name
        self.capabilities = ProviderCapabilities(
            filterable_fields=set(endpoint.capabilities.filterable_fields),
            returned_properties=set(endpoint.capabilities.returned_properties),
            provider_specific_fields=endpoint.capabilities.provider_specific_fields,
            supports_pagination=endpoint.capabilities.supports_pagination,
            max_results=endpoint.behavior.max_results,
        )

    async def search(self, query: MaterialSearchQuery) -> list[Material]:
        """Query this OPTIMADE endpoint and return normalized materials."""
        from optimade.client import OptimadeClient

        filter_string = QueryTranslator.to_optimade(query)
        base_url = self._endpoint.base_url
        if not base_url:
            return []

        try:
            client = OptimadeClient(
                base_urls=[base_url],
                max_results_per_provider=min(query.limit, self._endpoint.behavior.max_results),
                use_async=False,
            )
            results = client.structures.get(filter=filter_string) if filter_string else client.structures.get()
        except Exception as e:
            logger.warning("OPTIMADE query failed for %s: %s", self.id, e)
            raise

        return self._parse_response(results, filter_string)

    def _parse_response(self, results: dict, filter_string: str) -> list[Material]:
        """Parse the nested OptimadeClient response into Material objects.

        Raises ``RuntimeError`` when the OPTIMADE client captured an error
        from the provider (e.g. 404, 500).  This propagates up so the
        circuit breaker can record the failure.
        """
        materials = []
        endpoint_key = "structures"

        if endpoint_key not in results:
            return materials

        for _filter, providers_data in results[endpoint_key].items():
            for _url, response in providers_data.items():
                # Detect errors swallowed by OptimadeClient
                errors = response.get("errors", [])
                entries = response.get("data", [])

                if errors and not entries:
                    # Provider returned only errors — propagate as failure
                    raise RuntimeError(f"Provider {self.id} error: {errors[0][:200]}")

                if isinstance(entries, list):
                    for entry in entries:
                        try:
                            m = self._parse_entry(entry)
                            if m:
                                materials.append(m)
                        except Exception as e:
                            logger.debug("Failed to parse entry: %s", e)
        return materials

    def _parse_entry(self, entry: dict) -> Material | None:
        """Parse a single OPTIMADE JSON:API entry into a Material."""
        attrs = entry.get("attributes", {})
        entry_id = str(entry.get("id", ""))

        formula = (
            attrs.get("chemical_formula_descriptive")
            or attrs.get("chemical_formula_reduced")
            or attrs.get("chemical_formula_hill")
            or ""
        )
        elements = attrs.get("elements", [])
        nelements = attrs.get("nelements") or len(elements)

        source = f"optimade:{self.id}"

        space_group = None
        sg_val = attrs.get("space_group_symbol")
        if sg_val:
            space_group = PropertyValue(value=sg_val, source=source)

        lattice = None
        lv_val = attrs.get("lattice_vectors")
        if lv_val:
            lattice = PropertyValue(value=lv_val, source=source)

        # Provider-specific fields (prefixed with _)
        extra = {}
        for key, val in attrs.items():
            if key.startswith("_") and val is not None:
                try:
                    extra[key] = PropertyValue(value=val, source=source)
                except Exception:
                    logger.debug("Skipping unparseable field %s for %s", key, entry_id)

        return Material(
            id=entry_id,
            formula=formula,
            elements=sorted(elements),
            n_elements=nelements,
            sources=[self.id],
            space_group=space_group,
            lattice_vectors=lattice,
            extra_properties=extra,
            raw=attrs,
        )

    async def health_check(self) -> bool:
        """Check if this endpoint is responding."""
        import httpx
        try:
            url = f"{self._endpoint.base_url}/{self._endpoint.api_version}/info"
            async with httpx.AsyncClient(timeout=5.0) as client:
                resp = await client.get(url)
                return resp.status_code == 200
        except Exception:
            return False
