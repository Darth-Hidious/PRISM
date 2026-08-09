# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""MCP tool wrapper for the federated SearchEngine.

The infrastructure (`SearchEngine`, providers, fusion, cache, circuit
breakers) was already in `app/tools/search_engine/` but was never
exposed as a tool the in-PRISM agent could call. The agent saw 9
separate point tools (literature_search, patent_search, web_search,
semantic_search, knowledge_search, etc.) and had to pick correctly
per turn — the wrong choice meant missing data sources or hitting
the wrong DB.

This module exposes ONE federated tool: `materials_search`. It wraps
the existing `SearchEngine` and the existing `ProviderRegistry`. The
agent passes domain terms (elements, formula, property ranges, space
group, …); the engine fans out to every healthy provider, fuses
results across providers (formula + space-group identity key), and
returns one unified `SearchResult` with per-property provider
provenance.

Adding a new provider (ExoMatter, Citrine, MaterialsZone, …) is a
drop-in: implement `Provider` in `providers/`, register in
`provider_overrides.json`, and the same `materials_search` tool
fans the queries out to it without the agent's catalog changing.

This commit only *exposes* the federation — no new providers wired
yet. The existing `materials_project.py` and `optimade.py` providers
fan out automatically.
"""

from __future__ import annotations

import asyncio
import logging

from app.tools.base import Tool, ToolRegistry
from app.tools.search_engine.engine import SearchEngine
from app.tools.search_engine.providers.registry import ProviderRegistry
from app.tools.search_engine.query import MaterialSearchQuery, PropertyRange

logger = logging.getLogger(__name__)


_MATERIALS_SEARCH_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Search for materials across every healthy data provider in the "
        "federation (Materials Project, OPTIMADE federation members, "
        "and any user-installed providers like ExoMatter). Results are "
        "deduplicated across providers using formula + space group as "
        "identity key; conflicting property values from different "
        "sources are surfaced in `extra_properties` with provider tags "
        "so the agent can cite which source said what. Use this for "
        "any 'find candidate materials with property X' query."
    ),
    "properties": {
        "elements": {
            "type": "array",
            "items": {"type": "string"},
            "description": (
                "Element symbols that MUST be present in the material "
                "(e.g. ['Ni', 'Al', 'Cr']). Pass exactly as periodic-table "
                "symbols, case-sensitive."
            ),
        },
        "elements_any": {
            "type": "array",
            "items": {"type": "string"},
            "description": (
                "Material must contain AT LEAST ONE of these elements "
                "(union, not intersection). Use for 'something with a "
                "transition metal' style queries."
            ),
        },
        "exclude_elements": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Materials containing any of these elements are filtered out.",
        },
        "formula": {
            "type": "string",
            "description": (
                "Reduced chemical formula (e.g. 'Ni3Al'). Case-sensitive; "
                "stoichiometric subscripts inline with no spaces."
            ),
        },
        "n_elements": {
            "type": "object",
            "description": "Range of element count in the material.",
            "properties": {
                "min": {"type": "integer", "minimum": 1},
                "max": {"type": "integer", "minimum": 1},
            },
        },
        "band_gap": {
            "type": "object",
            "description": "Band gap range in eV.",
            "properties": {
                "min": {"type": "number"},
                "max": {"type": "number"},
            },
        },
        "formation_energy": {
            "type": "object",
            "description": "Formation energy per atom range in eV/atom.",
            "properties": {
                "min": {"type": "number"},
                "max": {"type": "number"},
            },
        },
        "energy_above_hull": {
            "type": "object",
            "description": (
                "Energy above convex hull (thermodynamic stability) in "
                "eV/atom. 0 = on the hull (most stable). Use [0, 0.05] "
                "for 'reasonably synthesizable' filtering."
            ),
            "properties": {
                "min": {"type": "number"},
                "max": {"type": "number"},
            },
        },
        "bulk_modulus": {
            "type": "object",
            "description": "Bulk modulus range in GPa.",
            "properties": {
                "min": {"type": "number"},
                "max": {"type": "number"},
            },
        },
        "debye_temperature": {
            "type": "object",
            "description": "Debye temperature range in Kelvin.",
            "properties": {
                "min": {"type": "number"},
                "max": {"type": "number"},
            },
        },
        "space_group": {
            "description": (
                "Space group symbol (e.g. 'Fm-3m', 'Pm-3m') or international "
                "number (1-230). Either accepted."
            ),
        },
        "crystal_system": {
            "type": "string",
            "enum": [
                "cubic",
                "hexagonal",
                "tetragonal",
                "orthorhombic",
                "monoclinic",
                "triclinic",
                "trigonal",
            ],
            "description": "Crystal system constraint.",
        },
        "providers": {
            "type": "array",
            "items": {"type": "string"},
            "description": (
                "Optional override: only query the named providers (e.g. "
                "['materials_project', 'oqmd']). Omit to query every healthy "
                "provider in the registry."
            ),
        },
        "limit": {
            "type": "integer",
            "minimum": 1,
            "maximum": 10000,
            "default": 100,
            "description": "Maximum number of unique materials to return after fusion.",
        },
        "timeout_seconds": {
            "type": "number",
            "minimum": 1,
            "maximum": 30,
            "default": 8,
            "description": (
                "Hard deadline for the whole federated search in seconds "
                "(default 8). The engine fans out to every healthy provider "
                "concurrently and returns PARTIAL results if this deadline "
                "fires — providers that didn't finish are reported as "
                "timed-out in providers_queried. Raise for exhaustive searches; "
                "lower for a fast best-effort pass."
            ),
        },
    },
    "required": [],
    "additionalProperties": False,
}


def _materials_search_factory(provider_registry: ProviderRegistry):
    """Build the closure that knows how to invoke SearchEngine.

    Held over a single SearchEngine instance per process so cache +
    circuit-breaker state are shared across calls. Cheap to keep alive
    because the engine itself is mostly references to the registry +
    cache backends.
    """
    engine = SearchEngine(provider_registry)

    def _materials_search(**kwargs) -> dict:
        # Pydantic-validate the inbound shape so a bad agent call fails
        # with a clear message before we hit any network.
        # S5: an optional timeout_seconds (default 8s, capped 1-30s in the
        # engine) lets the agent bound the whole fan-out. Popped before
        # query construction (it's not a search filter).
        timeout_seconds = kwargs.pop("timeout_seconds", None)
        query = MaterialSearchQuery(**_normalize_property_ranges(kwargs))

        # SearchEngine.search() is async; we're called from the tool server's
        # sync handler. Spin up a fresh loop per call (the engine spawns its
        # own tasks internally). The agent's Rust loop is NOT blocked by
        # Python's GIL — the tool server is a separate subprocess — but the
        # agent does await this call's result, so the engine's hard deadline
        # (S2/S5) is what guarantees the agent gets an answer within ~timeout.
        try:
            loop = asyncio.new_event_loop()
            try:
                result = loop.run_until_complete(
                    engine.search(query, timeout_seconds=timeout_seconds)
                )
            finally:
                loop.close()
        except Exception as exc:
            logger.exception("materials_search failed")
            return {
                "error": str(exc),
                "error_type": type(exc).__name__,
                "query": query.model_dump(exclude_none=True, mode="json"),
            }

        # S1: HONEST output. The old shape marked every provider ok:true
        # (getattr(log,"ok",True) — ProviderQueryLog has no `ok` field) and
        # dropped status/error/warnings, so the agent couldn't tell which
        # providers actually failed. Now we surface the full per-provider log
        # plus a one-glance summary + the engine's warnings array.
        providers_queried = []
        # `offline_blocked` is counted apart from `failed`. It fell into the
        # `else` and was reported as a provider failure — the one rolled-up
        # number a calling agent reads, contradicting the whole point of
        # distinguishing a policy refusal from provider health.
        summary = {
            "succeeded": 0,
            "failed": 0,
            "skipped": 0,
            "circuit_open": 0,
            "offline_blocked": 0,
        }
        for log in result.query_log:
            ok = log.status == "success"
            providers_queried.append(
                {
                    "provider": log.provider_name,
                    "provider_id": log.provider_id,
                    "endpoint": log.endpoint_url,
                    "status": log.status,
                    "ok": ok,
                    "latency_ms": round(log.latency_ms, 1),
                    "result_count": log.result_count,
                    # The provider's own total of matching records, when it
                    # reports one: result_count < available means this row
                    # is a slice of what exists.
                    "available": log.available,
                    "pages_fetched": log.pages_fetched,
                    # Partial is a third state: a truncated success returned
                    # less than what was asked for despite more existing.
                    "truncated": log.truncated,
                    "http_status": log.http_status_code,
                    "error": log.error_message,
                }
            )
            if log.status == "success":
                summary["succeeded"] += 1
            elif log.status == "skipped":
                summary["skipped"] += 1
            elif log.status == "offline_blocked":
                summary["offline_blocked"] += 1
            elif log.status == "circuit_open":
                summary["circuit_open"] += 1
            else:
                summary["failed"] += 1

        # An empty provider list is NOT a successful empty search. It means
        # nothing was asked, so nothing can be concluded — and
        # `{"materials": [], "count": 0}` is indistinguishable from "nothing
        # matched your filters". That exact shape is what a broken install
        # returned for months: the wheel omitted
        # `provider_overrides.json`, `build_registry()` raised
        # FileNotFoundError, bootstrap swallowed it into an empty registry,
        # and `materials_search` answered "no materials" with exit 0.
        #
        # `materials`/`count` are deliberately absent from this branch: a
        # caller that reads them before checking `error` must not find an
        # empty list to believe.
        if not providers_queried:
            known = len(provider_registry.get_all())
            if known == 0:
                reason = (
                    "no materials data providers are registered — provider "
                    "discovery produced an empty registry. This is an "
                    "installation or connectivity fault, not an empty result: "
                    "check that app/tools/search_engine/providers/"
                    "provider_overrides.json is present in the installed "
                    "package, and that the OPTIMADE provider index is "
                    "reachable."
                )
            else:
                reason = (
                    f"none of the {known} registered providers can serve this "
                    "query — every one of them was filtered out by capability "
                    "matching or an open circuit breaker. Nothing was queried, "
                    "so nothing was ruled out."
                )
            logger.error("materials_search queried no providers: %s", reason)
            return {
                "error": reason,
                "error_type": "NoProvidersQueried",
                "providers_queried": [],
                "providers_summary": summary,
                "warnings": result.warnings,
                "query": query.model_dump(exclude_none=True, mode="json"),
                "query_hash": query.query_hash(),
            }

        return {
            "materials": [m.model_dump(mode="json") for m in result.materials],
            "count": len(result.materials),
            # Partial is a third state: False whenever any consulted provider
            # failed, timed out, sat behind an open circuit, was offline-
            # blocked, or returned truncated data. A caller citing this
            # result as exhaustive must check it.
            "complete": result.complete,
            "providers_queried": providers_queried,
            "providers_summary": summary,
            "warnings": result.warnings,
            # S7: honest coverage — which filters ran server-side vs client-side.
            "coverage": result.coverage,
            "query_hash": query.query_hash(),
        }

    return _materials_search


def _normalize_property_ranges(kwargs: dict) -> dict:
    """Allow the agent to pass property ranges as either:
        bulk_modulus: {"min": 180, "max": 220}
        bulk_modulus: [180, 220]

    Many models lean toward arrays; accepting both is a small kindness
    that prevents one class of "I tried calling but it 422'd" loops.
    """
    range_fields = {
        "n_elements",
        "band_gap",
        "formation_energy",
        "energy_above_hull",
        "bulk_modulus",
        "debye_temperature",
    }
    out = dict(kwargs)
    # Normalize elements: accept comma-separated string or list
    if isinstance(out.get("elements"), str):
        out["elements"] = [e.strip() for e in out["elements"].split(",") if e.strip()]
    for field in range_fields:
        v = out.get(field)
        if isinstance(v, list) and len(v) == 2:
            out[field] = PropertyRange(min=v[0], max=v[1])
        elif isinstance(v, dict):
            out[field] = PropertyRange(**v)
    return out


def create_search_engine_tools(
    registry: ToolRegistry,
    provider_registry: ProviderRegistry,
) -> None:
    """Register `materials_search` as the federated MCP tool surface.

    Idempotent — safe to call multiple times if bootstrap reloads.
    """
    registry.register(
        Tool(
            name="materials_search",
            description=(
                "Federated search across every healthy materials database "
                "provider (Materials Project, OPTIMADE consortium members, "
                "user-installed providers). Returns deduplicated unified "
                "Material records with per-property provider provenance. "
                "Use this for 'find candidate materials with property X' "
                "queries instead of picking a per-DB tool."
            ),
            input_schema=_MATERIALS_SEARCH_SCHEMA,
            func=_materials_search_factory(provider_registry),
            requires_approval=False,
            source="builtin",
            source_detail="search_engine.federated",
        )
    )
    logger.info("Registered materials_search tool (federated provider registry)")
