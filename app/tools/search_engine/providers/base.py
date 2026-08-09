"""Provider ABC and capabilities model."""
from __future__ import annotations

from abc import ABC, abstractmethod

from pydantic import BaseModel

from app.tools.search_engine.query import MaterialSearchQuery
from app.tools.search_engine.result import Material, ProviderPage


class ProviderCapabilities(BaseModel):
    """What this provider can filter on and return."""
    filterable_fields: set[str] = set()
    returned_properties: set[str] = set()
    provider_specific_fields: list[str] = []
    supports_pagination: bool = True
    max_results: int | None = None

    def can_handle(self, query: MaterialSearchQuery) -> bool:
        """Check if this provider can handle the query's SERVER-SIDE filters.

        S7: property-range filters (band_gap, formation_energy, bulk_modulus,
        debye_temperature, energy_above_hull) are NOT server-side gates here —
        most OPTIMADE providers can't filter on them, but many RETURN them, so
        the engine fetches by the strongest server-side filter (elements/formula)
        and post-filters property ranges client-side (see engine
        _post_filter_client_side). Excluding a provider for an unadvertised
        property filter would silently drop coverage; instead we keep the
        provider and narrow locally.
        """
        # The fields a provider MUST support server-side to be eligible.
        # Property-range fields are intentionally absent — handled client-side.
        server_side_fields = {
            "elements": "elements",
            "elements_any": "elements",
            "exclude_elements": "elements",
            "formula": "formula",
            "n_elements": "nelements",
            "space_group": "space_group",
            "crystal_system": "crystal_system",
        }
        query_data = query.model_dump(exclude_none=True)
        for field_name in query_data:
            if field_name in ("providers", "limit"):
                continue
            cap_name = server_side_fields.get(field_name)
            if cap_name and cap_name not in self.filterable_fields:
                return False
        return True


class Provider(ABC):
    """Interface every data source implements."""

    id: str
    name: str
    capabilities: ProviderCapabilities

    @abstractmethod
    async def search(self, query: MaterialSearchQuery) -> list[Material] | ProviderPage:
        """Execute search, return normalized materials.

        Return a :class:`ProviderPage` when the provider can account for its
        own completeness (pages walked, truncation, the server's total) so
        the engine logs measured values. A plain ``list[Material]`` keeps
        working and is logged with single-page defaults and no fabricated
        HTTP status.
        """
        ...

    def describe_query(self, query: MaterialSearchQuery) -> str:
        """Describe the query this provider INTENDS to issue for ``query``.

        Recorded as ``ProviderQueryLog.query_description``. Contract: this is
        the provider's own pre-dispatch translation of the engine query -- a
        logical/intended query, explicitly NOT a captured wire transcript. It
        is computed before ``search()`` runs, so it exists even for a query
        that later fails, is cancelled, or is served without any transport.

        Design choice (of the two honest options): we narrow what the field
        CLAIMS rather than capture what ``search()`` actually sent. A wire
        capture would force every adapter to thread transport internals back
        out through timeouts and cancellation, and could still say nothing
        for queries that die before send. Instead the field claims exactly
        what it carries, and adapters must derive the description from the
        SAME code path their ``search()`` dispatch uses (see
        OptimadeProvider.describe_query and
        MaterialsProjectProvider.describe_query) so intent and dispatch
        cannot drift.

        The default is the engine-level query as JSON: honest but generic.
        Providers should override this with their native query syntax.
        """
        return query.model_dump_json(exclude_none=True)

    async def health_check(self) -> bool:
        """Ping the provider. Default: return True."""
        return True
