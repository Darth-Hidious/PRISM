# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Free first-class materials-informatics tools (OPTIMADE redesign S8).

These close the gap to commercial materials-discovery platforms (Citrine,
ExoMatter) using ONLY open data (the OPTIMADE federation via materials_search)
— pure federation reshaping, no external libs, no paid APIs.

Each tool follows the PRISM-Alpha authoring contract (typed inputs/outputs,
units in field names, examples, honest errors, provenance). They compose the
S1-S7 search-engine improvements: honest partial results, bounded time, and
client-side property post-filtering with coverage reporting.

Tools:
  - screen_materials: property-range screening across the federation, ranked.
    (The "find me alloys with band_gap in X, bulk_modulus > Y" verb.)
  - compare_materials: side-by-side comparison of N candidates across a
    property set, with per-cell provenance. (The "find & compare compatible
    materials" verb from the product loop.)
  - lookup_structure: deep structure + properties lookup for a formula/
    composition from the federation in one typed call.
"""

from __future__ import annotations

import logging
from typing import Any

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


def _prop_summary(material: dict) -> dict[str, Any]:
    """Pull the named properties (with provenance) from a fused Material dict."""
    out: dict[str, Any] = {}
    for field in (
        "band_gap",
        "formation_energy",
        "energy_above_hull",
        "bulk_modulus",
        "debye_temperature",
        "space_group",
        "crystal_system",
    ):
        pv = material.get(field)
        if pv and isinstance(pv, dict) and pv.get("value") is not None:
            out[field] = {
                "value": pv.get("value"),
                "source": pv.get("source", ""),
                "unit": pv.get("unit"),
            }
    return out


def create_materials_informatics_tools(registry: ToolRegistry) -> None:
    """Register the free first-class materials-informatics tools.

    Idempotent — safe to call multiple times if bootstrap reloads.
    """
    registry.register(_screen_materials_tool())
    registry.register(_compare_materials_tool())
    registry.register(_lookup_structure_tool())
    logger.info("Registered materials-informatics tools (screen/compare/lookup)")


# ---------------------------------------------------------------------------
# screen_materials — property-range screening, ranked
# ---------------------------------------------------------------------------

_SCREEN_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Screen candidate materials across the federation by property ranges "
        "and rank them. Use this for 'find me alloys with band_gap in [X,Y] eV "
        "and bulk_modulus > Z GPa' queries. It runs materials_search (element/"
        "formula server-side), post-filters property ranges client-side, scores "
        "each candidate, and returns a ranked list. Coverage is reported honestly: "
        "you'll see how many providers were queried, how many succeeded, and "
        "whether property filters ran client-side."
    ),
    "properties": {
        "elements": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Element symbols that MUST be present (e.g. ['Cu','Cr']).",
        },
        "formula": {
            "type": "string",
            "description": "Reduced formula constraint (e.g. 'Cu3Au').",
        },
        "band_gap_eV": {
            "type": "object",
            "description": "Band gap range in eV.",
            "properties": {"min": {"type": "number"}, "max": {"type": "number"}},
        },
        "bulk_modulus_GPa": {
            "type": "object",
            "description": "Bulk modulus range in GPa.",
            "properties": {"min": {"type": "number"}, "max": {"type": "number"}},
        },
        "formation_energy_eV_per_atom": {
            "type": "object",
            "description": "Formation energy range in eV/atom.",
            "properties": {"min": {"type": "number"}, "max": {"type": "number"}},
        },
        "rank_by": {
            "type": "string",
            "enum": ["band_gap", "bulk_modulus", "formation_energy"],
            "description": (
                "Property to rank candidates by, best-first: descending for "
                "band_gap/bulk_modulus, ASCENDING for formation_energy "
                "(more negative = more stable)."
            ),
        },
        "limit": {
            "type": "integer",
            "minimum": 1,
            "maximum": 200,
            "default": 20,
            "description": "Max candidates to return after screening + ranking.",
        },
        "timeout_seconds": {
            "type": "number",
            "minimum": 1,
            "maximum": 30,
            "default": 12,
            "description": "Hard deadline for the underlying federated search (seconds).",
        },
    },
    "required": [],
    "additionalProperties": False,
}


def _screen_materials_tool() -> Tool:
    def _screen(**kwargs) -> dict:
        # Defer the import so the tool registers even if bootstrap hasn't built
        # the search engine yet; resolve it from the live registry at call time.
        from app.tools.search_engine.query import MaterialSearchQuery, PropertyRange

        rank_by = kwargs.pop("rank_by", None)
        limit = int(kwargs.pop("limit", 20))
        timeout = kwargs.pop("timeout_seconds", 12)
        # Map the unit-bearing field names to the query model's fields.
        query_kwargs: dict[str, Any] = {}
        if "elements" in kwargs:
            query_kwargs["elements"] = kwargs["elements"]
        if "formula" in kwargs:
            query_kwargs["formula"] = kwargs["formula"]
        for screen_field, query_field in (
            ("band_gap_eV", "band_gap"),
            ("bulk_modulus_GPa", "bulk_modulus"),
            ("formation_energy_eV_per_atom", "formation_energy"),
        ):
            r = kwargs.get(screen_field)
            if r:
                if isinstance(r, list):
                    r = {"min": r[0], "max": r[1]}
                query_kwargs[query_field] = PropertyRange(**r)
        if not query_kwargs:
            return {
                "error": "provide at least one filter (elements, formula, or a property range)",
            }
        query = MaterialSearchQuery(limit=max(limit * 3, 50), **query_kwargs)
        # Run the federated search via the registered materials_search tool so we
        # reuse its engine (cache, breakers, honest output). The registry is a
        # process-level singleton — NOT rebuilt per call (SCI-9/C3).
        try:
            from app.tools.materials._shared import get_shared_registry

            reg = get_shared_registry()
            ms = reg.get("materials_search")
            call_args: dict[str, Any] = query.model_dump(exclude_none=True, mode="json")
            call_args["timeout_seconds"] = timeout
            search_result = ms.func(**call_args)
        except Exception as exc:
            logger.exception("screen_materials search failed")
            return {"error": f"{type(exc).__name__}: {exc}"}

        materials = search_result.get("materials", [])
        warnings = list(search_result.get("warnings", []))
        rank_coverage: dict[str, Any] | None = None
        # Rank by the requested property; missing props sort last. Direction is
        # property-aware: formation_energy is an energy where MORE NEGATIVE =
        # MORE STABLE, so stability ranking sorts ASCENDING (a descending sort
        # would surface the LEAST stable candidates first — SCI-8 fix).
        if rank_by:
            prop_key = {
                "band_gap": "band_gap",
                "bulk_modulus": "bulk_modulus",
                "formation_energy": "formation_energy",
            }.get(rank_by, rank_by)
            ascending = rank_by == "formation_energy"
            missing = float("inf") if ascending else float("-inf")

            def _rank_val(m):
                pv = m.get(prop_key)
                try:
                    return float(pv.get("value")) if pv and pv.get("value") is not None else missing
                except (TypeError, ValueError):
                    return missing

            materials = sorted(materials, key=_rank_val, reverse=not ascending)

            # A rank the data cannot support is not a rank. `_rank_val` sends
            # every candidate lacking the property to the SAME sentinel, so a
            # pool where nobody reports it comes back in the federation's own
            # order under a `ranked_by` label that says otherwise. Measured
            # live: elements=['Cu','O'], rank_by='band_gap' returned three
            # candidates, all with `properties: {}` and `ranked_by:
            # "band_gap"`. Report the coverage, and say so when it is zero.
            ranked_slice = materials[:limit]
            with_value = sum(1 for m in ranked_slice if _rank_val(m) != missing)
            rank_coverage = {
                "property": rank_by,
                "candidates_with_value": with_value,
                "candidates_without_value": len(ranked_slice) - with_value,
            }
            if ranked_slice and with_value == 0:
                warnings.append(
                    f"ranked_by={rank_by!r} but none of the {len(ranked_slice)} "
                    "returned candidates reports that property — the order is "
                    "the federation's, not a ranking"
                )

        candidates = [
            {
                "formula": m.get("formula"),
                "id": m.get("id"),
                "elements": m.get("elements"),
                "n_elements": m.get("n_elements"),
                "properties": _prop_summary(m),
                "sources": m.get("sources", []),
            }
            for m in materials[:limit]
        ]
        return {
            "candidates": candidates,
            "count": len(candidates),
            "screened_from": search_result.get("count", 0),
            "ranked_by": rank_by,
            "rank_coverage": rank_coverage,
            "coverage": search_result.get("coverage", {}),
            "providers_summary": search_result.get("providers_summary", {}),
            "warnings": warnings,
        }

    return Tool(
        name="screen_materials",
        description=(
            "Screen candidate materials across the federation by property ranges "
            "and rank them. The free, federation-backed equivalent of a "
            "materials-discovery screening pass. Use for 'find alloys with "
            "band_gap in [X,Y] and bulk_modulus > Z'. Returns ranked candidates "
            "with per-property provenance and honest coverage."
        ),
        input_schema=_SCREEN_SCHEMA,
        func=_screen,
        requires_approval=False,
        source="builtin",
        source_detail="materials.screening",
        examples=[
            {
                "input": {
                    "elements": ["Cu", "Cr"],
                    "band_gap_eV": {"min": 0.0, "max": 2.0},
                    "rank_by": "band_gap",
                    "limit": 10,
                },
                "output_note": "top-10 Cu-Cr compounds by band_gap, screened client-side",
            }
        ],
    )


# ---------------------------------------------------------------------------
# compare_materials — side-by-side comparison with provenance
# ---------------------------------------------------------------------------

_COMPARE_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Compare N candidate materials side-by-side across a property set. "
        "Pass formulas or material ids (from a prior materials_search / "
        "screen_materials); the tool looks each up in the federation and "
        "builds a typed comparison table with per-cell provenance so you can "
        "cite which source said what. The 'find & compare compatible materials' "
        "verb."
    ),
    "properties": {
        "materials": {
            "type": "array",
            "items": {"type": "string"},
            "description": (
                "Formulas or material ids to compare (e.g. ['Cu3Au','Cu3Sn', "
                "'mp-123']). Up to 10."
            ),
        },
        "properties": {
            "type": "array",
            "items": {
                "type": "string",
                "enum": [
                    "band_gap",
                    "formation_energy",
                    "energy_above_hull",
                    "bulk_modulus",
                    "debye_temperature",
                    "space_group",
                    "crystal_system",
                    "n_elements",
                ],
            },
            "description": "Properties to compare (default: all available).",
        },
    },
    "required": ["materials"],
    "additionalProperties": False,
}


def _compare_materials_tool() -> Tool:
    def _compare(**kwargs) -> dict:
        formulas = kwargs.get("materials", [])
        if not formulas or len(formulas) > 10:
            return {"error": "provide 1-10 materials (formulas or ids) to compare"}
        wanted = kwargs.get("properties") or [
            "band_gap",
            "formation_energy",
            "energy_above_hull",
            "bulk_modulus",
            "space_group",
            "crystal_system",
        ]
        try:
            from app.tools.materials._shared import get_shared_registry

            reg = get_shared_registry()
            ms = reg.get("materials_search")
        except Exception as exc:
            return {"error": f"search engine unavailable: {exc}"}

        rows = []
        for f in formulas:
            # Look up by formula.
            res = ms.func(formula=f, limit=3, timeout_seconds=8)
            mats = res.get("materials", [])
            if not mats:
                rows.append({"query": f, "found": False})
                continue
            best = mats[0]
            props = _prop_summary(best)
            row = {
                "query": f,
                "found": True,
                "formula": best.get("formula"),
                "id": best.get("id"),
                "sources": best.get("sources", []),
            }
            for p in wanted:
                row[p] = props.get(p) if p in props else best.get(p)
            rows.append(row)

        # Build a compact comparison matrix: property -> {formula: value}.
        matrix: dict[str, dict[str, Any]] = {}
        for p in wanted:
            matrix[p] = {}
            for row in rows:
                if row.get("found"):
                    val = row.get(p)
                    matrix[p][row["formula"]] = val.get("value") if isinstance(val, dict) else val
        return {
            "comparison": rows,
            "matrix": matrix,
            "properties_compared": wanted,
            "note": "values carry per-cell provenance in the `comparison` rows",
        }

    return Tool(
        name="compare_materials",
        description=(
            "Compare up to 10 candidate materials side-by-side across a property "
            "set, with per-cell provider provenance. Use after "
            "screen_materials / materials_search to decide between finalists."
        ),
        input_schema=_COMPARE_SCHEMA,
        func=_compare,
        requires_approval=False,
        source="builtin",
        source_detail="materials.screening",
    )


# ---------------------------------------------------------------------------
# lookup_structure — deep structure + properties for a formula
# ---------------------------------------------------------------------------

_LOOKUP_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Look up the relaxed structure and available computed properties for a "
        "formula or composition from the federation in one typed call. Returns "
        "the structure (lattice vectors, space group), band gap, formation "
        "energy, stability, and elastic properties where available — each with "
        "its source. Closes the 'I found a material, now give me its crystal "
        "structure + properties' step that otherwise needs multiple calls."
    ),
    "properties": {
        "formula": {
            "type": "string",
            "description": "Reduced chemical formula (e.g. 'Si', 'Cu2O', 'Gd2Ti2O7').",
        },
        "elements": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Alternative: look up by required elements (formula preferred).",
        },
    },
    "required": [],
    "additionalProperties": False,
}


def _lookup_structure_tool() -> Tool:
    def _lookup(**kwargs) -> dict:
        formula = kwargs.get("formula")
        elements = kwargs.get("elements")
        if not formula and not elements:
            return {"error": "provide a formula or elements to look up"}
        try:
            from app.tools.materials._shared import get_shared_registry

            reg = get_shared_registry()
            ms = reg.get("materials_search")
        except Exception as exc:
            return {"error": f"search engine unavailable: {exc}"}

        call_args: dict[str, Any] = {"limit": 5, "timeout_seconds": 8}
        if formula:
            call_args["formula"] = formula
        if elements:
            call_args["elements"] = elements
        res = ms.func(**call_args)
        mats = res.get("materials", [])
        if not mats:
            return {
                "formula": formula,
                "found": False,
                "providers_summary": res.get("providers_summary", {}),
                "note": "no structures found in the federation for this query",
            }
        # Best hit = the one with the most populated properties.
        best = max(
            mats,
            key=lambda m: sum(
                1 for f in ("band_gap", "formation_energy", "bulk_modulus", "space_group")
                if m.get(f) and isinstance(m.get(f), dict) and m.get(f).get("value") is not None
            ),
        )
        return {
            "formula": best.get("formula"),
            "found": True,
            "id": best.get("id"),
            "elements": best.get("elements"),
            "n_elements": best.get("n_elements"),
            "structure": {
                "space_group": best.get("space_group"),
                "crystal_system": best.get("crystal_system"),
                "lattice_vectors": best.get("lattice_vectors"),
            },
            "properties": _prop_summary(best),
            "extra_properties": best.get("extra_properties", {}),
            "sources": best.get("sources", []),
            "other_hits": [
                {"formula": m.get("formula"), "id": m.get("id")}
                for m in mats
                if m.get("id") != best.get("id")
            ],
            "providers_summary": res.get("providers_summary", {}),
        }

    return Tool(
        name="lookup_structure",
        description=(
            "Look up the crystal structure + computed properties for a formula "
            "from the federation in one typed call (space group, lattice, band "
            "gap, formation energy, etc.), each with source provenance."
        ),
        input_schema=_LOOKUP_SCHEMA,
        func=_lookup,
        requires_approval=False,
        source="builtin",
        source_detail="materials.screening",
    )
