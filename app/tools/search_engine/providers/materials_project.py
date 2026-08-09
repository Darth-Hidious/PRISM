"""Materials Project native API provider — wraps MPRester."""

from __future__ import annotations

import logging
import os

from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
from app.tools.search_engine.providers.endpoint import ProviderEndpoint
from app.tools.search_engine.query import MaterialSearchQuery
from app.tools.search_engine.result import (
    ExtractionProvenance,
    Material,
    MaterialIdentity,
    PropertyValue,
)
from app.tools.search_engine.translator import QueryTranslator

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

    @staticmethod
    def _proxy_formula(query: MaterialSearchQuery) -> str:
        """The narrowed formula the keyless platform-proxy path issues.

        The proxy supports only formula pulls: an explicit formula wins,
        else the FIRST element is used as a broad pull, else nothing is
        issued at all. Shared by ``_search_via_platform_proxy`` (dispatch)
        and ``describe_query`` (audit) so the recorded description can never
        drift from what the proxy path actually requests.
        """
        if query.formula:
            return query.formula
        if query.elements:
            return query.elements[0]
        return ""

    def describe_query(self, query: MaterialSearchQuery) -> str:
        """The intended query of the path ``search()`` will take (same branch).

        - Local MP_API_KEY present: the MPRester summary-search kwargs
          (QueryTranslator.to_mp_kwargs) -- what that path passes to MP.
        - Keyless (the default install): the platform proxy issues ONLY the
          narrowed formula pull, so that is what gets recorded. Reporting
          to_mp_kwargs here would advertise constraints (band_gap ranges,
          full element sets) that are never sent -- the exact lie this
          method exists to remove.
        """
        if os.environ.get("MP_API_KEY", ""):
            return str(QueryTranslator.to_mp_kwargs(query))
        formula = self._proxy_formula(query)
        if not formula:
            return "platform_proxy: no formula/elements -- no request will be issued"
        return (
            f'platform_proxy formula="{formula}" '
            "(query narrowed to a formula pull; proxy path)"
        )

    async def search(self, query: MaterialSearchQuery) -> list[Material]:
        # E13 proxy fix: route MP requests through the platform proxy when there
        # is no LOCAL MP_API_KEY. The old code's _resolve_api_key tier-2 returned
        # the user's platform JWT and handed it to MPRester as if it were an MP
        # key — but a JWT is not a valid X-API-KEY, so MP native always failed
        # keylessly. Now: local key → MPRester (direct); no local key → platform
        # proxy (server-side key injection, via _query_materials_project).
        env_key = os.environ.get("MP_API_KEY", "")
        if env_key:
            return await self._search_via_mprester(query, env_key)
        return await self._search_via_platform_proxy(query)

    async def _search_via_mprester(self, query: MaterialSearchQuery, api_key: str) -> list[Material]:
        """Direct MPRester path (local MP_API_KEY present)."""
        try:
            from mp_api.client import MPRester

            kwargs = QueryTranslator.to_mp_kwargs(query)
            kwargs.setdefault(
                "fields",
                [
                    "material_id", "formula_pretty", "elements", "nelements",
                    "band_gap", "formation_energy_per_atom", "energy_above_hull",
                    "symmetry",
                ],
            )
            with MPRester(api_key) as mpr:
                docs = mpr.materials.summary.search(
                    num_chunks=1, chunk_size=min(query.limit, 100), **kwargs,
                )
            return [self._parse_doc(self._doc_to_dict(d)) for d in docs]
        except Exception as e:
            logger.warning("MP native (MPRester) query failed: %s", e)
            raise

    async def _search_via_platform_proxy(self, query: MaterialSearchQuery) -> list[Material]:
        """Platform-proxy path (no local key — server injects MP_API_KEY).

        Reuses the existing _query_materials_project helper (data.py) which has
        the same 3-tier fallback. This is the keyless path every PRISM user gets
        via `prism login`.
        """
        try:
            from app.tools.data import _query_materials_project
        except ImportError as exc:
            # A broken install must surface as a provider failure, not as
            # "queried MP, found nothing".
            raise RuntimeError(f"MP platform proxy unavailable (import failed): {exc}") from exc

        # The proxy takes only a formula; _proxy_formula narrows the query the
        # same way describe_query reports it (formula, else first element as a
        # broad pull -- the proxy returns up to 20 per call -- else nothing).
        formula = self._proxy_formula(query)
        if not formula:
            return []

        res = _query_materials_project(
            formula=formula,
            properties=[
                "material_id", "formula_pretty", "elements", "nelements",
                "band_gap", "formation_energy_per_atom", "energy_above_hull",
                "symmetry",
            ],
        )
        # C1 honesty fix: a proxy error (outage, auth failure, HTTP error) must
        # RAISE so the engine's circuit breaker + query_log record an honest
        # provider failure. Returning [] here would report status=success,
        # count=0 — an outage masquerading as "nothing found".
        if not isinstance(res, dict):
            raise RuntimeError(
                f"MP platform proxy returned unexpected payload: {type(res).__name__}"
            )
        if res.get("error"):
            raise RuntimeError(f"MP platform proxy error: {res['error']}")
        if not res.get("results"):
            return []

        materials = []
        for doc in res["results"][: query.limit]:
            try:
                m = self._parse_doc(doc)
                if m:
                    materials.append(m)
            except Exception:
                continue
        return materials

    def _resolve_api_key(self) -> str:
        """Resolve a LOCAL MP API key only (the proxy path doesn't need one).

        E13: this now returns ONLY the local env key. The platform JWT is no
        longer misused as an MP key — the proxy path (_search_via_platform_proxy)
        handles the keyless case. Kept for backward compat with any caller that
        checks it directly.
        """
        return os.environ.get(self._endpoint.auth.auth_env_var or "MP_API_KEY", "")

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
        extraction = ExtractionProvenance(
            extractor_id="materials_project_api",
            kind="structured_api",
        )
        mid = str(doc.get("material_id", ""))
        formula = doc.get("formula_pretty", "")
        elements = sorted(doc.get("elements", []))
        nelements = doc.get("nelements", len(elements))

        band_gap = None
        if doc.get("band_gap") is not None:
            band_gap = PropertyValue(
                value=doc["band_gap"],
                source=source,
                method="DFT-PBE",
                unit="eV",
                extraction=extraction,
            )

        formation_energy = None
        if doc.get("formation_energy_per_atom") is not None:
            formation_energy = PropertyValue(
                value=doc["formation_energy_per_atom"],
                source=source,
                method="DFT-PBE",
                unit="eV/atom",
                extraction=extraction,
            )

        energy_above_hull = None
        if doc.get("energy_above_hull") is not None:
            energy_above_hull = PropertyValue(
                value=doc["energy_above_hull"],
                source=source,
                method="DFT-PBE",
                unit="eV/atom",
                extraction=extraction,
            )

        space_group = None
        sym = doc.get("symmetry")
        if isinstance(sym, dict) and sym.get("symbol"):
            space_group = PropertyValue(
                value=sym["symbol"],
                source=source,
                extraction=extraction,
            )

        return Material(
            id=mid,
            formula=formula,
            elements=elements,
            n_elements=nelements,
            sources=["mp_native"],
            identity=MaterialIdentity(
                domain="crystal",
                representation="formula_space_group",
                attributes={
                    "formula": formula,
                    "space_group": str(space_group.value) if space_group else "unknown",
                },
            ),
            band_gap=band_gap,
            formation_energy=formation_energy,
            energy_above_hull=energy_above_hull,
            space_group=space_group,
        )
