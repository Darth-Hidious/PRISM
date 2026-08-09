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
from app.tools.search_engine.translator import (
    QueryTranslator,
    optimade_reduced_formula,
)

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
        """The formula the keyless platform-proxy path issues.

        The proxy supports ONLY explicit formula pulls. It used to narrow an
        elements-only query to ``query.elements[0]`` -- elements=["Ni","Al"]
        became formula="Ni" and returned pure nickel labelled success, a
        different question answered as if it were the asked one. Elements-only
        queries are now refused honestly (see ``_proxy_refusal``). Shared by
        ``_search_via_platform_proxy`` (dispatch) and ``describe_query``
        (audit) so the recorded description can never drift from what the
        proxy path actually requests.
        """
        return query.formula or ""

    @staticmethod
    def _proxy_refusal(query: MaterialSearchQuery) -> str | None:
        """Why the keyless proxy path refuses this query, or None if servable.

        Shared by ``search()`` (which raises it) and ``describe_query`` (which
        records it), so refusal and audit cannot drift.
        """
        if query.formula:
            return None
        if query.elements or query.elements_any:
            return (
                "cannot serve elements-only queries via the platform-proxy "
                "path (it supports formula pulls only; substituting the first "
                "element would answer a different question). Set MP_API_KEY "
                "for native element filtering."
            )
        return None

    def describe_query(self, query: MaterialSearchQuery) -> str:
        """The intended query of the path ``search()`` will take (same branch).

        - Local MP_API_KEY present: the MPRester summary-search kwargs
          (QueryTranslator.to_mp_kwargs) -- what that path passes to MP.
        - Keyless (the default install): the platform proxy issues ONLY an
          explicit formula pull, so that is what gets recorded -- or the
          refusal that ``search()`` will raise. Reporting to_mp_kwargs here
          would advertise constraints (band_gap ranges, element sets) that
          are never sent -- the exact lie this method exists to remove.
        """
        if os.environ.get("MP_API_KEY", ""):
            return str(QueryTranslator.to_mp_kwargs(query))
        refusal = self._proxy_refusal(query)
        if refusal:
            return f"platform_proxy: {refusal}"
        formula = self._proxy_formula(query)
        if not formula:
            return "platform_proxy: no formula -- no request will be issued"
        return f'platform_proxy formula="{formula}" (formula pull; proxy path)'

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
                    "symmetry", "bulk_modulus",
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
        # Refuse queries this path cannot faithfully represent BEFORE anything
        # else: substituting a narrower question (the old elements[0]
        # degradation) is a wrong answer presented as a right one.
        refusal = self._proxy_refusal(query)
        if refusal:
            raise RuntimeError(f"MP platform proxy: {refusal}")

        try:
            from app.tools.data import _query_materials_project
        except ImportError as exc:
            # A broken install must surface as a provider failure, not as
            # "queried MP, found nothing".
            raise RuntimeError(f"MP platform proxy unavailable (import failed): {exc}") from exc

        # The proxy takes only an explicit formula -- _proxy_formula is the
        # same helper describe_query reports with. No formula (and no refusal
        # above) means there is nothing this path could ask.
        formula = self._proxy_formula(query)
        if not formula:
            return []

        res = _query_materials_project(
            formula=formula,
            properties=[
                "material_id", "formula_pretty", "elements", "nelements",
                "band_gap", "formation_energy_per_atom", "energy_above_hull",
                "symmetry", "bulk_modulus",
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

        # MP summary `bulk_modulus` is a dict of Voigt/Reuss/VRH averages in
        # GPa; VRH is the conventional single number (and what the k_vrh
        # search filter constrains).
        bulk_modulus = None
        bm = doc.get("bulk_modulus")
        bm_value = bm.get("vrh") if isinstance(bm, dict) else bm
        if isinstance(bm_value, (int, float)):
            bulk_modulus = PropertyValue(
                value=bm_value,
                source=source,
                method="DFT-PBE",
                unit="GPa",
                extraction=extraction,
            )

        space_group = None
        sym = doc.get("symmetry") if isinstance(doc.get("symmetry"), dict) else {}
        if sym.get("symbol"):
            space_group = PropertyValue(
                value=sym["symbol"],
                source=source,
                extraction=extraction,
            )
        # Identity discriminator: prefer the International Tables NUMBER (no
        # notation variants, keys identically with OPTIMADE's
        # space_group_it_number), else the symbol. When symmetry is absent the
        # attribute is OMITTED -- never an "unknown" sentinel, which used to
        # merge every polymorph of a formula into one fabricated record.
        # The identity formula is canonicalised the same way the OPTIMADE
        # adapter does it, so MP's "TiO2" (formula_pretty) and the federation's
        # "O2Ti" (chemical_formula_reduced) key identically.
        identity_attrs = {"formula": optimade_reduced_formula(formula) or formula}
        identity_sg = sym.get("number") or sym.get("symbol")
        if identity_sg:
            identity_attrs["space_group"] = str(identity_sg)

        return Material(
            id=mid,
            formula=formula,
            elements=elements,
            n_elements=nelements,
            sources=["mp_native"],
            identity=MaterialIdentity(
                domain="crystal",
                representation="formula_space_group",
                attributes=identity_attrs,
            ),
            band_gap=band_gap,
            formation_energy=formation_energy,
            energy_above_hull=energy_above_hull,
            bulk_modulus=bulk_modulus,
            space_group=space_group,
        )
