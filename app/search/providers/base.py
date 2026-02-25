"""Provider ABC and capabilities model."""
from __future__ import annotations

from abc import ABC, abstractmethod

from pydantic import BaseModel

from app.search.query import MaterialSearchQuery
from app.search.result import Material


class ProviderCapabilities(BaseModel):
    """What this provider can filter on and return."""
    filterable_fields: set[str] = set()
    returned_properties: set[str] = set()
    provider_specific_fields: list[str] = []
    supports_pagination: bool = True
    max_results: int | None = None

    def can_handle(self, query: MaterialSearchQuery) -> bool:
        """Check if this provider can handle the query's filters."""
        field_map = {
            "elements": "elements", "elements_any": "elements",
            "exclude_elements": "elements", "formula": "formula",
            "n_elements": "nelements", "space_group": "space_group",
            "crystal_system": "crystal_system",
            "band_gap": "band_gap", "formation_energy": "formation_energy",
            "energy_above_hull": "energy_above_hull",
            "bulk_modulus": "bulk_modulus", "debye_temperature": "debye_temperature",
        }
        query_data = query.model_dump(exclude_none=True)
        for field_name in query_data:
            if field_name in ("providers", "limit"):
                continue
            cap_name = field_map.get(field_name)
            if cap_name and cap_name not in self.filterable_fields:
                return False
        return True


class Provider(ABC):
    """Interface every data source implements."""

    id: str
    name: str
    capabilities: ProviderCapabilities

    @abstractmethod
    async def search(self, query: MaterialSearchQuery) -> list[Material]:
        """Execute search, return normalized materials."""
        ...

    async def health_check(self) -> bool:
        """Ping the provider. Default: return True."""
        return True
