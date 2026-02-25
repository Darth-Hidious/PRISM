"""Materials Project native API provider — wraps MPRester."""
from __future__ import annotations

import logging
import os

from app.search.providers.base import Provider, ProviderCapabilities
from app.search.providers.endpoint import ProviderEndpoint
from app.search.query import MaterialSearchQuery
from app.search.result import Material, PropertyValue
from app.search.translator import QueryTranslator

logger = logging.getLogger(__name__)


class MaterialsProjectProvider(Provider):
    """Materials Project native API via MPRester."""

    def __init__(self, endpoint: ProviderEndpoint):
        self._endpoint = endpoint
        self.id = endpoint.id
        self.name = endpoint.name
        self.capabilities = ProviderCapabilities(
            filterable_fields=set(endpoint.capabilities.filterable_fields),
            returned_properties=set(endpoint.capabilities.returned_properties),
        )

    async def search(self, query: MaterialSearchQuery) -> list[Material]:
        api_key = os.environ.get(
            self._endpoint.auth.auth_env_var or "MP_API_KEY", ""
        )
        if not api_key:
            logger.warning("MP_API_KEY not set — skipping Materials Project native query")
            return []

        try:
            from mp_api.client import MPRester
            kwargs = QueryTranslator.to_mp_kwargs(query)
            kwargs.setdefault("fields", [
                "material_id", "formula_pretty", "elements", "nelements",
                "band_gap", "formation_energy_per_atom", "energy_above_hull",
                "symmetry",
            ])

            with MPRester(api_key) as mpr:
                docs = mpr.materials.summary.search(
                    num_chunks=1, chunk_size=min(query.limit, 100),
                    **kwargs,
                )

            return [self._parse_doc(self._doc_to_dict(d)) for d in docs]
        except Exception as e:
            logger.warning("MP native query failed: %s", e)
            raise

    def _doc_to_dict(self, doc) -> dict:
        """Convert MPRester doc object to plain dict."""
        if hasattr(doc, "dict"):
            return doc.dict()
        if hasattr(doc, "model_dump"):
            return doc.model_dump()
        return dict(doc)

    def _parse_doc(self, doc: dict) -> Material:
        """Parse an MPRester result document into a Material."""
        source = "mp_native"
        mid = str(doc.get("material_id", ""))
        formula = doc.get("formula_pretty", "")
        elements = sorted(doc.get("elements", []))
        nelements = doc.get("nelements", len(elements))

        band_gap = None
        if doc.get("band_gap") is not None:
            band_gap = PropertyValue(value=doc["band_gap"], source=source, method="DFT-PBE", unit="eV")

        formation_energy = None
        if doc.get("formation_energy_per_atom") is not None:
            formation_energy = PropertyValue(
                value=doc["formation_energy_per_atom"], source=source, method="DFT-PBE", unit="eV/atom",
            )

        energy_above_hull = None
        if doc.get("energy_above_hull") is not None:
            energy_above_hull = PropertyValue(
                value=doc["energy_above_hull"], source=source, method="DFT-PBE", unit="eV/atom",
            )

        space_group = None
        sym = doc.get("symmetry")
        if isinstance(sym, dict) and sym.get("symbol"):
            space_group = PropertyValue(value=sym["symbol"], source=source)

        return Material(
            id=mid,
            formula=formula,
            elements=elements,
            n_elements=nelements,
            sources=["mp_native"],
            band_gap=band_gap,
            formation_energy=formation_energy,
            energy_above_hull=energy_above_hull,
            space_group=space_group,
        )
