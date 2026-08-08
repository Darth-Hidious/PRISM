# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Phase-stability screening — convex-hull distance via Materials Project.

SCOPE (honest): this looks up the Materials Project 0 K DFT convex hull
(energy_above_hull) for a compound — a thermodynamic SCREENING signal for
"is this phase stable/synthesizable at 0 K?". It is NOT a CALPHAD
phase-equilibria calculation: no temperature dependence, no phase fractions,
no Gibbs-energy minimization (that is what Thermo-Calc-class databases
compute). MP requests route through the platform's server-brokered MP key —
no local MP_API_KEY needed.

`phase_stability`: given a composition/formula, returns its distance to the
convex hull (eV/atom), whether it's thermodynamically stable (on the hull),
its formation energy, and the competing phases. Routes MP requests through the
platform proxy (crates/api/src/routes/data_proxy.rs) which injects the server-
side key — no local MP_API_KEY needed.
"""

from __future__ import annotations

import logging
from typing import Any

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


def create_phase_stability_tool(registry: ToolRegistry) -> None:
    """Register the phase_stability tool."""
    registry.register(_phase_stability_tool())
    logger.info("Registered phase_stability tool")


_PHASE_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Check the thermodynamic phase stability of a material: its distance "
        "to the convex hull (energy_above_hull in eV/atom), whether it's stable "
        "(on the hull), its formation energy, and competing phases. Routes "
        "Materials Project data through the platform's server-brokered key — "
        "no local MP_API_KEY needed. This is 0 K DFT convex-hull screening, "
        "NOT CALPHAD phase equilibria (no temperature dependence or phase "
        "fractions). Use after screening to gate candidates: on-hull or "
        "near-hull (< 0.05 eV/atom) materials are likely synthesizable. "
        "Supply formula or material_id — one of the two is mandatory, and "
        "either alone is enough."
    ),
    "properties": {
        "formula": {
            "type": "string",
            "description": "Reduced formula (e.g. 'Cu2O', 'BaTiO3', 'Gd2Ti2O7').",
        },
        "material_id": {
            "type": "string",
            "description": "Optional MP material_id (e.g. 'mp-19770') for a direct lookup.",
        },
    },
    "required": [],
    "additionalProperties": False,
}


def _phase_stability_tool() -> Tool:
    def _stability(**kwargs) -> dict:
        formula = kwargs.get("formula")
        material_id = kwargs.get("material_id")
        if not formula and not material_id:
            return {"error": "provide a formula or material_id"}

        # Route through the platform MP proxy (server-side key) — no local
        # MP_API_KEY needed. _query_materials_project has a 3-tier fallback:
        # local key → platform proxy (keyless, needs prism login) → OPTIMADE hint.
        try:
            from app.tools.data import _query_materials_project

            props = [
                "material_id",
                "formula_pretty",
                "energy_above_hull",
                "formation_energy_per_atom",
                "theoretical",
                "density",
            ]
            res = _query_materials_project(
                formula=formula, material_id=material_id, properties=props
            )
        except Exception as exc:
            logger.exception("phase_stability MP query failed")
            return {"error": f"Materials Project lookup failed: {type(exc).__name__}: {exc}"}

        # The proxy returns {results: [...], source: "..."} on success, or
        # {error: "..."} if it fell through to the keyless hint.
        if isinstance(res, dict) and res.get("error"):
            return {
                "formula": formula,
                "error": res["error"],
                "hint": "Materials Project proxy unavailable — try materials_search for OPTIMADE-federated stability fields (_oqmd_stability, _alexandria_hull_distance)",
            }

        results = res.get("results", []) if isinstance(res, dict) else []
        source = res.get("source", "materials_project") if isinstance(res, dict) else "?"
        if not results:
            return {
                "formula": formula,
                "found": False,
                "note": "no Materials Project entry for this formula",
                "source": source,
            }

        # Best entry = the one closest to the hull (most stable polymorph).
        def _hull(e):
            v = e.get("energy_above_hull")
            return float(v) if v is not None else float("inf")

        best = min(results, key=_hull)
        ehull = best.get("energy_above_hull")
        form_e = best.get("formation_energy_per_atom")
        stable = ehull is not None and abs(float(ehull)) < 0.025  # on-hull tolerance

        return {
            "formula": best.get("formula_pretty", formula),
            "found": True,
            "material_id": best.get("material_id"),
            "energy_above_hull_eV_per_atom": round(float(ehull), 4) if ehull is not None else None,
            "formation_energy_eV_per_atom": round(float(form_e), 4) if form_e is not None else None,
            "stable": stable,
            "stability_class": _classify_stability(ehull),
            "theoretical": best.get("theoretical"),
            "density": best.get("density"),
            "other_polymorphs": len(results) - 1,
            "source": source,
            "provenance": f"Materials Project convex hull via {source}; energy_above_hull is the decomposition energy (0 = on the hull = stable)",
        }

    return Tool(
        name="phase_stability",
        description=(
            "Check a material's thermodynamic phase stability: distance to the "
            "convex hull (energy_above_hull eV/atom), stable-vs-unstable verdict, "
            "formation energy. 0 K DFT convex-hull screening (Materials Project, "
            "platform-brokered key — no local MP_API_KEY); not CALPHAD equilibria."
        ),
        input_schema=_PHASE_SCHEMA,
        func=_stability,
        requires_approval=False,
        source="builtin",
        source_detail="materials.stability",
        examples=[
            {
                "input": {"formula": "Cu2O"},
                "output_note": "Cu2O is on the hull (energy_above_hull=0, stable)",
            },
            {
                "input": {"formula": "BaTiO3"},
                "output_note": "ferroelectric perovskite — stable on the hull",
            },
        ],
    )


def _classify_stability(ehull: float | None) -> str:
    """Honest stability classification from energy_above_hull (eV/atom)."""
    if ehull is None:
        return "unknown"
    e = float(ehull)
    if e < 0.025:
        return "stable (on hull)"
    if e < 0.05:
        return "near-stable (likely synthesizable)"
    if e < 0.1:
        return "metastable"
    return "unstable (above hull)"
