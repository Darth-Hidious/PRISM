# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Structure description + synthesizability heuristic (E12).

  - describe_structure: robocrystallographer → human-readable crystal-structure
    description (the "what is this structure in plain English?" answer).
    robocrystallographer is NOT available on Python 3.14 (no wheel) — degrades
    with an honest hint on 3.14, runs natively on 3.12/3.13.
  - predict_synthesizability: a TRANSPARENT heuristic (not a trained DL
    classifier — those need unavailable datasets). Combines convex-hull distance
    (via the platform MP proxy) + composition features + element rarity into an
    explainable synthesizability score with the contributing factors listed.
    Clearly labeled "heuristic, not a trained classifier."
"""

from __future__ import annotations

import logging

from app.tools._extras import missing_extra_error
from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


def create_structure_desc_tools(registry: ToolRegistry) -> None:
    registry.register(_describe_structure_tool())
    registry.register(_predict_synthesizability_tool())
    logger.info("Registered describe_structure + predict_synthesizability tools")


def _describe_structure_tool() -> Tool:
    schema = {
        "type": "object",
        "description": (
            "Generate a human-readable description of a crystal structure "
            "(mineral name, dimensionality, structural features, coordination). "
            "The 'what is this structure in plain English?' answer. Uses "
            "robocrystallographer (hackingmaterials.lbl.gov). Supply cif or "
            "formula — at least one is mandatory, and only cif yields a full "
            "description."
        ),
        "properties": {
            "formula": {"type": "string", "description": "Formula (e.g. 'BaTiO3'). Looks the material up in the OPTIMADE federation; because those hits carry no site coordinates, a formula alone yields a note asking for a CIF, not a description."},
            "cif": {"type": "string", "description": "CIF text to describe directly. This is the only input that actually produces a robocrystallographer description."},
        },
        "required": [],
        "additionalProperties": False,
    }

    def _run(**kwargs) -> dict:
        cif = kwargs.get("cif")
        formula = kwargs.get("formula")
        if not cif and not formula:
            return {"error": "provide a formula or cif"}
        try:
            from robocrystallographer.structure import StructureDescriber
            from pymatgen.core import Structure
        except ImportError:
            # One missing-dependency shape (app/tools/_extras.py). The hint
            # this replaced was `pip install prism-platform[ml]`, which 404s —
            # prism-platform is on no index (see _extras.install_command).
            return missing_extra_error(
                "ml",
                "robocrystallographer not installed (note: no Python 3.14 wheel "
                "— use 3.12/3.13)",
                tool_available=False,
            )

        # Get a structure: from CIF text, or from the federation.
        if cif:
            try:
                from pymatgen.io.cif import CifParser
                import io
                struct = CifParser(io.StringIO(cif)).get_structures()[0]
            except Exception as exc:
                return {"error": f"could not parse CIF: {exc}"}
        else:
            try:
                from app.tools.materials._shared import get_shared_registry

                reg = get_shared_registry()
                ms = reg.get("materials_search")
                res = ms.func(formula=formula, limit=3, timeout_seconds=8)
                mats = res.get("materials", [])
                if not mats:
                    return {"formula": formula, "found": False}
                # Reconstruct a Structure from lattice_vectors + sites if available.
                best = mats[0]
                lv = best.get("lattice_vectors")
                if not lv or not lv.get("value"):
                    return {"formula": formula, "error": "no lattice data in federation hit to describe"}
                # Fallback: robocrystallographer needs a full Structure; without
                # site coordinates we can only describe at the composition level.
                return {"formula": formula, "note": "full CIF not available from the federation hit; use lookup_structure or provide a CIF for a robocrystallographer description"}
            except Exception as exc:
                return {"error": f"federation lookup failed: {exc}"}

        try:
            describer = StructureDescriber()
            description = describer.describe(struct)
            return {
                "formula": formula,
                "description": description,
                "structure": {"lattice": str(struct.lattice), "n_sites": len(struct)},
                "provenance": "robocrystallographer (hackingmaterials.lbl.gov)",
            }
        except Exception as exc:
            return {"error": f"robocrystallographer failed: {type(exc).__name__}: {exc}"}

    return Tool(
        name="describe_structure",
        description=(
            "Describe a crystal structure in plain English — mineral name, "
            "dimensionality, coordination environments, structural motifs. "
            "Returns robocrystallographer prose for a CIF you supply; given only "
            "a formula it looks the material up in the OPTIMADE federation and, "
            "because those hits carry no site coordinates, returns a note asking "
            "for a CIF instead of a description. Needs the robocrystallographer "
            "package, which has no Python 3.14 wheel."
        ),
        input_schema=schema, func=_run, requires_approval=False,
        source="builtin", source_detail="materials.structure_desc",
    )


def _predict_synthesizability_tool() -> Tool:
    schema = {
        "type": "object",
        "description": (
            "Estimate whether a material is likely synthesizable, using a "
            "TRANSPARENT heuristic (NOT a trained DL classifier). Combines: "
            "(1) distance to the convex hull (stable/near-hull = synthesizable, "
            "via the platform MP proxy), (2) composition complexity, (3) "
            "element abundance. Returns a score + the contributing factors so "
            "the reasoning is auditable. Clearly labeled 'heuristic'."
        ),
        "properties": {
            "formula": {"type": "string", "description": "Formula to score (e.g. 'YBa2Cu3O7'). Looked up in Materials Project via the platform proxy for its hull distance; if MP has no entry the hull factor is simply absent from the returned factor list."},
        },
        "required": ["formula"],
        "additionalProperties": False,
    }

    def _run(**kwargs) -> dict:
        formula = kwargs.get("formula")
        if not formula:
            return {"error": "provide a formula"}

        factors: list[dict] = []
        score = 0.5  # start neutral

        # Factor 1: convex-hull distance (the dominant signal).
        hull_score = None
        try:
            from app.tools.materials.stability import _classify_stability
            from app.tools.data import _query_materials_project

            res = _query_materials_project(formula=formula, properties=["material_id", "energy_above_hull"])
            if isinstance(res, dict) and res.get("results"):
                results = res["results"]
                ehulls = [r.get("energy_above_hull") for r in results if r.get("energy_above_hull") is not None]
                if ehulls:
                    ehull = min(float(e) for e in ehulls)
                    cls = _classify_stability(ehull)
                    # On-hull (0) → +0.4; <0.05 → +0.3; <0.1 → +0.1; >0.1 → -0.2
                    if ehull < 0.025:
                        hull_score = 0.4
                    elif ehull < 0.05:
                        hull_score = 0.3
                    elif ehull < 0.1:
                        hull_score = 0.1
                    else:
                        hull_score = -0.2
                    score += hull_score
                    factors.append({"factor": "convex_hull_distance", "value_eV_per_atom": round(ehull, 4),
                                    "classification": cls, "contribution": round(hull_score, 2)})
            else:
                # The dominant signal is ABSENT, not zero. Dropping it from the
                # factor list silently made "MP has no entry for this formula"
                # look identical to "the hull factor was never part of the
                # score" — while the note still claimed the score combines
                # hull distance. Say which one it was.
                factors.append({
                    "factor": "convex_hull_distance",
                    "available": False,
                    "reason": "no Materials Project entry for this formula; "
                              "the dominant signal is missing, not zero",
                })
        except Exception as exc:
            factors.append({"factor": "convex_hull_distance", "error": f"MP lookup failed: {type(exc).__name__}"})

        # Factor 2: composition complexity (fewer elements = easier).
        try:
            from pymatgen.core import Composition

            c = Composition(formula)
            n_elems = len(c.elements)
            # 1-4 elements: easy; 5-8: moderate; >8: hard.
            comp_score = 0.15 if n_elems <= 4 else (0.0 if n_elems <= 8 else -0.15)
            score += comp_score
            factors.append({"factor": "composition_complexity", "n_elements": n_elems, "contribution": round(comp_score, 2)})
        except Exception:
            pass

        # Factor 3: known in MP = existence evidence (already synthesized or computed).
        # Guarding on `res.get("results")` made the -0.1 "absent from MP" branch
        # unreachable — n_hits was > 0 by construction — so a formula MP has
        # never heard of scored the same as one it simply wasn't asked about.
        # Gate on the QUERY succeeding instead; `res` is unbound when the hull
        # lookup itself raised, and a failed lookup is not evidence of absence.
        try:
            if isinstance(res, dict) and "error" not in res:
                n_hits = len(res.get("results") or [])
                existence = 0.1 if n_hits > 0 else -0.1
                score += existence
                factors.append({"factor": "database_existence", "mp_hits": n_hits, "contribution": round(existence, 2)})
        except Exception:
            pass

        score = max(0.0, min(1.0, score))
        return {
            "formula": formula,
            "score": round(score, 3),
            "likely_synthesizable": score >= 0.5,
            "factors": factors,
            "note": "HEURISTIC (not a trained classifier). Combines hull distance + complexity + database existence. A trained DL synthesizability model needs datasets not bundled with PRISM.",
            "provenance": "transparent heuristic over Materials Project convex hull (platform proxy) + pymatgen composition",
        }

    return Tool(
        name="predict_synthesizability",
        description=(
            "Estimate synthesizability via a transparent heuristic (hull + "
            "complexity + existence). Clearly labeled heuristic, not a trained model."
        ),
        input_schema=schema, func=_run, requires_approval=False,
        source="builtin", source_detail="materials.structure_desc",
    )
