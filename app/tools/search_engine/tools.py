# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
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
    },
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
        query = MaterialSearchQuery(**_normalize_property_ranges(kwargs))

        # SearchEngine.search() is async; we're called from sync MCP.
        # Spin up a fresh loop per call (the engine spawns its own
        # tasks internally). This is fine for the tool-call cadence;
        # if we ever want to share an event loop, refactor the
        # registration to expose async tools natively.
        try:
            loop = asyncio.new_event_loop()
            try:
                result = loop.run_until_complete(engine.search(query))
            finally:
                loop.close()
        except Exception as exc:
            logger.exception("materials_search failed")
            return {
                "error": str(exc),
                "error_type": type(exc).__name__,
                "query": query.model_dump(exclude_none=True, mode="json"),
            }

        # Return a JSON-serialisable shape that keeps provenance — the
        # agent should be able to cite "Materials Project says X for
        # Inconel 718" vs "OPTIMADE-Alexandria says Y."
        return {
            "materials": [m.model_dump(mode="json") for m in result.materials],
            "count": len(result.materials),
            "providers_queried": [
                {
                    "provider": log.provider_name,
                    "endpoint": log.endpoint_url,
                    "latency_ms": log.latency_ms,
                    "ok": getattr(log, "ok", True),
                }
                for log in result.query_log
            ],
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
