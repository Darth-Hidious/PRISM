"""OPTIMADE federation provider -- wraps OptimadeClient for a single endpoint."""

from __future__ import annotations

import logging
import time

from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
from app.tools.search_engine.providers.endpoint import ProviderEndpoint
from app.tools.search_engine.query import MaterialSearchQuery
from app.tools.search_engine.resilience.retries import with_transient_retry
from app.tools.search_engine.result import (
    ExtractionProvenance,
    Material,
    MaterialIdentity,
    PropertyValue,
    ProviderPage,
)
from app.tools.search_engine.translator import (
    QueryTranslator,
    optimade_reduced_formula,
)

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

    def describe_query(self, query: MaterialSearchQuery) -> str:
        """The OPTIMADE filter string for /structures.

        ``search()`` builds its wire filter by calling THIS method, so the
        recorded description and the dispatched filter share one code path
        and cannot drift (the base-class contract's intent, made literal).
        """
        return QueryTranslator.to_optimade(query)

    # Loop protection for `links.next` chains, not a policy cap: a server
    # cycling its next links must not spin forever. Hitting it is reported
    # as truncation, never absorbed into success.
    MAX_PAGES = 100

    async def search(self, query: MaterialSearchQuery) -> ProviderPage:
        """Query this OPTIMADE endpoint via async httpx (not OptimadeClient).

        The OptimadeClient library uses synchronous HTTP which blocks the
        event loop. We use httpx directly for a clean async path with
        proper timeouts.

        Follows ``links.next`` until the requested limit is satisfied or the
        server has no more pages. A provider whose server-side page cap is
        smaller than the requested limit (e.g. 20 rows against ``limit:
        1000``) therefore returns everything up to the limit instead of the
        first page dressed up as the whole answer. The returned
        :class:`ProviderPage` carries the accounting: pages walked, the
        server's own total (``meta.data_returned``), the real HTTP status,
        and ``truncated=True`` whenever more matching data existed but fewer
        than requested rows are returned.
        """
        import httpx

        filter_string = self.describe_query(query)
        base_url = self._endpoint.base_url
        if not base_url:
            return ProviderPage(materials=[], pages_fetched=0)

        # Build the structures URL. OPTIMADE spec requires /v1/structures.
        # Some base_urls already include /v1 (e.g. from discovery), others
        # don't (e.g. "https://optimade.materialsproject.org"). Handle both.
        base = base_url.rstrip("/")
        if base.endswith("/v1"):
            url = f"{base}/structures"
        else:
            url = f"{base}/v1/structures"
        limit = min(query.limit, self._endpoint.behavior.max_results or query.limit)
        params = {}
        if filter_string:
            params["filter"] = filter_string
        params["page_limit"] = str(limit)

        timeout = self._endpoint.behavior.timeout_ms / 1000
        headers = {"Accept": "application/json"}

        # S4: bounded transient retry (resilience/retries.py). Recovers a
        # single transient 429/503/connection-reset with one backed-off retry;
        # never retries 400/404/500/timeouts (those raise immediately). The
        # factory re-builds the coroutine each attempt (an awaitable is
        # one-shot). httpx.AsyncClient is per-attempt so a reset connection is
        # replaced, not reused.
        async def _get_page(page_url: str, page_params: dict | None) -> tuple[dict, int]:
            async def _do_get() -> tuple[dict, int]:
                async with httpx.AsyncClient(
                    timeout=timeout,
                    headers=headers,
                    follow_redirects=True,
                ) as client:
                    resp = await client.get(page_url, params=page_params)
                    resp.raise_for_status()
                    return resp.json(), resp.status_code

            try:
                return await with_transient_retry(_do_get, provider_id=self.id)
            except httpx.TimeoutException:
                logger.warning("OPTIMADE timeout for %s (%.1fs)", self.id, timeout)
                raise
            except httpx.HTTPStatusError as e:
                logger.warning(
                    "OPTIMADE HTTP %d for %s: %s",
                    e.response.status_code,
                    self.id,
                    str(e)[:200],
                )
                raise
            except Exception as e:
                logger.warning("OPTIMADE query failed for %s: %s", self.id, e)
                raise

        materials: list[Material] = []
        pages_fetched = 0
        available: int | None = None
        http_status: int | None = None
        note: str | None = None
        next_url: str | None = None

        while True:
            try:
                if next_url is None:
                    data, http_status = await _get_page(url, params)
                else:
                    # links.next is a full URL per the JSON:API base of the
                    # OPTIMADE spec; parameters are already baked into it.
                    data, http_status = await _get_page(next_url, None)
            except Exception as e:
                if pages_fetched == 0:
                    raise  # nothing fetched: the whole query failed
                # A mid-pagination failure is a PARTIAL result, not a flavour
                # of success: keep what landed, say why it stops here.
                note = (
                    f"pagination stopped after {pages_fetched} page(s): "
                    f"{type(e).__name__}: {str(e)[:200]}"
                )
                logger.warning("OPTIMADE %s %s", self.id, note)
                break

            pages_fetched += 1

            # Check for OPTIMADE error responses
            errors = data.get("errors", [])
            entries = data.get("data", [])
            if errors and not entries:
                err = errors[0]
                if isinstance(err, dict):
                    err = err.get("detail", err.get("title", str(err)))
                if pages_fetched == 1:
                    raise RuntimeError(
                        f"Provider '{self.id}' returned error: {str(err)[:200]}"
                    )
                note = (
                    f"pagination stopped after {pages_fetched - 1} page(s): "
                    f"provider returned error: {str(err)[:200]}"
                )
                break

            meta_total = (data.get("meta") or {}).get("data_returned")
            if isinstance(meta_total, int):
                available = meta_total

            if isinstance(entries, list):
                for entry in entries:
                    try:
                        m = self._parse_entry(entry)
                        if m:
                            materials.append(m)
                    except Exception as e:
                        logger.debug("Failed to parse entry: %s", e)

            if len(materials) >= limit:
                break
            raw_next = (data.get("links") or {}).get("next")
            if isinstance(raw_next, dict):
                raw_next = raw_next.get("href")
            if not raw_next or not isinstance(raw_next, str):
                break
            if pages_fetched >= self.MAX_PAGES:
                note = (
                    f"pagination stopped at the {self.MAX_PAGES}-page safety "
                    "cap with a next link still present"
                )
                logger.warning("OPTIMADE %s %s", self.id, note)
                break
            next_url = raw_next

        returned = materials[:limit]
        # Truncated means "less than what was asked for despite more
        # existing": a mid-chain failure, the safety cap, or a server that
        # stopped serving next links while its own total says more matched.
        truncated = len(returned) < limit and (
            note is not None
            or (available is not None and available > len(returned))
        )
        return ProviderPage(
            materials=returned,
            pages_fetched=pages_fetched,
            truncated=truncated,
            available=available,
            http_status_code=http_status,
            note=note,
        )

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
                    err = errors[0]
                    if isinstance(err, dict):
                        err = err.get("detail", err.get("title", str(err)))
                    err = str(err)[:200]
                    raise RuntimeError(f"Provider '{self.id}' returned an error: {err}")

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
        # Identity formula: prefer the spec's canonical `chemical_formula_reduced`
        # (alphabetical, GCD-reduced) so "TiO2" from one provider and "O2Ti"
        # from another key identically. Canonicalise through the same tested
        # helper the wire filter uses; a formula it cannot model (hydrates,
        # parentheses) is used verbatim rather than mangled. The DISPLAY
        # formula above keeps the provider's descriptive spelling.
        identity_source = attrs.get("chemical_formula_reduced") or formula
        identity_formula = (
            optimade_reduced_formula(identity_source) or identity_source
        )
        elements = attrs.get("elements", [])
        nelements = attrs.get("nelements") or len(elements)

        source = f"optimade:{self.id}"
        extraction = ExtractionProvenance(
            extractor_id="optimade_jsonapi",
            kind="structured_api",
        )

        # Symmetry, from the fields the OPTIMADE spec actually defines.
        # `space_group_symbol` is NOT in the specification and no live
        # provider returns it (live-probed MP OPTIMADE 2026-08: it returns
        # space_group_it_number / space_group_symbol_hall /
        # space_group_symbol_hermann_mauguin); reading it made symmetry None
        # for essentially every federation hit.
        sg_number = attrs.get("space_group_it_number")
        sg_symbol = (
            attrs.get("space_group_symbol_hermann_mauguin")
            or attrs.get("space_group_symbol_hermann_mauguin_extended")
            or attrs.get("space_group_symbol_hall")
        )
        space_group = None
        sg_display = sg_symbol or (str(sg_number) if sg_number is not None else None)
        if sg_display:
            space_group = PropertyValue(
                value=sg_display,
                source=source,
                extraction=extraction,
            )
        # Identity discriminator: prefer the International Tables NUMBER --
        # it has no notation variants ("P42/mnm" vs "P4_2/mnm"), so entries
        # from different providers key identically. When symmetry is absent
        # the attribute is OMITTED (never a sentinel): the identity plugin
        # then refuses to fuse this record with anything (IdentityNotFusable).
        identity_sg = (
            str(sg_number) if sg_number is not None else sg_symbol
        )
        identity_attrs = {"formula": identity_formula}
        if identity_sg:
            identity_attrs["space_group"] = str(identity_sg)

        lattice = None
        lv_val = attrs.get("lattice_vectors")
        if lv_val:
            lattice = PropertyValue(
                value=lv_val,
                source=source,
                extraction=extraction,
            )

        # Provider-specific fields (prefixed with _)
        extra = {}
        for key, val in attrs.items():
            if key.startswith("_") and val is not None:
                try:
                    extra[key] = PropertyValue(
                        value=val,
                        source=source,
                        extraction=extraction,
                    )
                except Exception:
                    logger.debug("Skipping unparseable field %s for %s", key, entry_id)

        return Material(
            id=entry_id,
            formula=formula,
            elements=sorted(elements),
            n_elements=nelements,
            sources=[self.id],
            identity=MaterialIdentity(
                domain="crystal",
                representation="formula_space_group",
                attributes=identity_attrs,
            ),
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
