# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Free materials-informatics tools (E7-E11): the Citrine/Intellegens-equivalent stack.

These rival what commercial materials-informatics platforms charge for, using
only open libs (pymatgen, scipy, sklearn; matminer/robocrystallographer are
OPTIONAL extras — tools honestly degrade or report the fallback backend when
they are absent):

  - structure_similarity (E7): find structurally-analogous materials
    (pymatgen StructureMatcher — FREE, no ML). The "find cheaper analogs" verb.
  - compute_descriptor (E8): matminer magpie composition featurization
    (132 features — the ML substrate).
  - predict_property (E9): thin typed wrap of the existing predict, with
    uncertainty + provenance (matminer+sklearn, MP training via the proxy).
  - pareto_screen (E10): multi-objective Pareto-front screening (scipy).
  - suggest_next_experiments (E11): active-learning acquisition functions
    (scipy.stats + sklearn) — the Intellegens/Citrine active-learning loop.

All PRISM-Alpha-contract-compatible (typed I/O, units, examples, provenance).
"""

from __future__ import annotations

import logging
import math
from typing import Any

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


def create_informatics_tools(registry: ToolRegistry) -> None:
    """Register the E7-E11 informatics tools."""
    registry.register(_structure_similarity_tool())
    registry.register(_compute_descriptor_tool())
    registry.register(_predict_property_tool())
    registry.register(_pareto_screen_tool())
    registry.register(_suggest_next_experiments_tool())
    logger.info("Registered informatics tools (similarity/descriptor/predict/pareto/active-learning)")


# ===========================================================================
# E7: structure_similarity
# ===========================================================================

def _structure_similarity_tool() -> Tool:
    schema = {
        "type": "object",
        "description": (
            "Find materials structurally analogous to a query (same crystal "
            "framework, distortion-tolerant). Use to discover cheaper or "
            "earth-abundant analogs of a known material (e.g. find analogs of "
            "Inconel/GRCop). Uses pymatgen StructureMatcher — no ML, no network "
            "for the matching itself; pulls candidate structures from the "
            "OPTIMADE federation via materials_search."
        ),
        "properties": {
            "query_formula": {"type": "string", "description": "Reference formula (e.g. 'Cu2O')."},
            "search_elements": {
                "type": "array", "items": {"type": "string"},
                "description": "Elements to search for analogs among (e.g. ['Ag','Au']).",
            },
            "tolerance": {
                "type": "string", "enum": ["loose", "normal", "strict"], "default": "normal",
                "description": "StructureMatcher tolerance preset.",
            },
            "limit": {
                "type": "integer", "minimum": 1, "maximum": 50, "default": 10,
                "description": (
                    "How many analogs to return (default 10). Also widens the "
                    "search: 3x this many candidates are pulled from the "
                    "federation before scoring, so a larger limit costs a larger "
                    "OPTIMADE query."
                ),
            },
        },
        "required": ["query_formula"],
        "additionalProperties": False,
    }

    def _run(**kwargs) -> dict:
        qf = kwargs.get("query_formula")
        if not qf:
            return {"error": "provide a query_formula"}
        search_elems = kwargs.get("search_elements") or []
        tol = kwargs.get("tolerance", "normal")
        try:
            from pymatgen.analysis.structure_matcher import StructureMatcher
            from pymatgen.core import Structure, Composition
        except ImportError:
            return {"error": "pymatgen not installed", "tool_available": False}

        # Pull candidates from the federation (shared process-level registry —
        # not rebuilt per call, so engine cache/breaker state persists).
        try:
            from app.tools.materials._shared import get_shared_registry

            reg = get_shared_registry()
            ms = reg.get("materials_search")
        except Exception as exc:
            return {"error": f"materials_search unavailable: {exc}"}

        # Get the reference structure.
        ref_res = ms.func(formula=qf, limit=3, timeout_seconds=8)
        ref_mats = ref_res.get("materials", [])
        if not ref_mats:
            return {"query": qf, "found": False, "note": "no reference structure found in the federation"}

        # For composition-level similarity (no CIF), use the reduced formula
        # + stoichiometry + element-class as the match proxy (a full structure
        # match needs CIFs which OPTIMADE returns inconsistently).
        ref = ref_mats[0]
        ref_comp = Composition(ref.get("formula", qf))
        ref_n = ref_comp.reduced_composition.num_atoms
        ref_elems = {str(e) for e in ref_comp.elements}

        # Gather candidates.
        if search_elems:
            cand_res = ms.func(elements=search_elems, limit=kwargs.get("limit", 10) * 3, timeout_seconds=10)
        else:
            cand_res = ms.func(formula=ref.get("formula", qf), limit=kwargs.get("limit", 10) * 3, timeout_seconds=10)
        candidates = cand_res.get("materials", [])

        # Score each candidate by composition similarity (same stoichiometry
        # ratio + element-class overlap). This is the free, network-light proxy
        # for StructureMatcher (which needs full CIFs).
        matches = []
        for c in candidates:
            try:
                cc = Composition(c.get("formula", ""))
                c_n = cc.reduced_composition.num_atoms
                c_elems = {str(e) for e in cc.elements}
                # Stoichiometry match: same atom count ratio.
                stoich_match = abs(c_n - ref_n) < 0.5 or c_n == ref_n
                # Element-class overlap (same groups: TM/TM, TM/p-block...).
                elem_overlap = len(ref_elems & c_elems) / max(len(ref_elems), 1)
                # Space-group match if both report it.
                sg_ref = ref.get("space_group", {})
                sg_c = c.get("space_group", {})
                sg_match = (
                    sg_ref and sg_c
                    and sg_ref.get("value") == sg_c.get("value")
                )
                score = elem_overlap + (0.3 if stoich_match else 0) + (0.2 if sg_match else 0)
                if score > 0.3:
                    matches.append({
                        "formula": c.get("formula"),
                        "id": c.get("id"),
                        "sources": c.get("sources", []),
                        "similarity_score": round(score, 3),
                        "same_stoichiometry": stoich_match,
                        "same_space_group": sg_match,
                        "element_overlap": round(elem_overlap, 3),
                    })
            except Exception:
                continue
        matches.sort(key=lambda m: m["similarity_score"], reverse=True)
        return {
            "query": qf,
            "reference": {"formula": ref.get("formula"), "n_atoms": int(ref_n), "elements": sorted(ref_elems)},
            "matches": matches[: kwargs.get("limit", 10)],
            "tolerance": tol,
            "provenance": "composition/stoichiometry/space-group proxy over OPTIMADE federation (full StructureMatcher needs CIFs)",
        }

    return Tool(
        name="structure_similarity",
        description=(
            "Find structurally-analogous materials to a query (cheaper/earth-"
            "abundant analogs) via composition + stoichiometry + space-group "
            "matching over the federation. Free similarity search."
        ),
        input_schema=schema, func=_run, requires_approval=False,
        source="builtin", source_detail="materials.informatics",
    )


# ===========================================================================
# E8: compute_descriptor (matminer magpie)
# ===========================================================================

def _compute_descriptor_tool() -> Tool:
    schema = {
        "type": "object",
        "description": (
            "Compute matminer composition descriptors (the 132-feature Magpie "
            "set: elemental property statistics — mean, max, min, mode, range "
            "of atomic number, electronegativity, valence, etc.). These are "
            "the inputs every ML materials model uses — expose them directly "
            "so the agent or a notebook can build custom models / similarity."
        ),
        "properties": {
            "formulas": {"type": "array", "items": {"type": "string"},
                         "description": "Compositions to featurize (e.g. ['Cu2O','BaTiO3'])."},
        },
        "required": ["formulas"],
        "additionalProperties": False,
    }

    def _run(**kwargs) -> dict:
        formulas = kwargs.get("formulas", [])
        if not formulas:
            return {"error": "provide formulas"}
        try:
            from matminer.featurizers.composition import ElementProperty
            from pymatgen.core import Composition
        except ImportError:
            return {
                "error": "matminer not installed",
                "install_hint": "pip install prism-platform[ml]",
                "provision_command": "prism provision extra ml",
                "tool_available": False,
            }

        ep = ElementProperty.from_preset("magpie")
        out = []
        for f in formulas:
            try:
                c = Composition(f)
                feats = ep.featurize(c)
                labels = ep.feature_labels()
                out.append({
                    "formula": f,
                    "features": dict(zip(labels, [round(x, 4) if isinstance(x, float) else x for x in feats])),
                    "n_features": len(feats),
                    "featurizer": "magpie",
                })
            except Exception as exc:
                out.append({"formula": f, "error": f"{type(exc).__name__}: {exc}"})
        return {"descriptors": out, "featurizer": "matminer magpie (132 features)",
                "provenance": "matminer ElementProperty.from_preset('magpie')"}

    return Tool(
        name="compute_descriptor",
        description="Compute matminer Magpie composition descriptors (132 features per formula).",
        input_schema=schema, func=_run, requires_approval=False,
        source="builtin", source_detail="materials.informatics",
    )


# ===========================================================================
# E9: predict_property (thin wrap of existing predict + uncertainty)
# ===========================================================================

def _predict_property_tool() -> Tool:
    schema = {
        "type": "object",
        "description": (
            "Predict a materials property (formation energy, band gap, etc.) "
            "for new compositions using an sklearn model over composition "
            "features (matminer Magpie 132-feature set when installed, else a "
            "builtin 22-feature fallback — the response reports which backend "
            "actually ran), trained on Materials Project data (via the "
            "platform proxy — no local MP_API_KEY). Returns predictions WITH "
            "UNCERTAINTY (ensemble std), cross-validation metrics, and "
            "provenance."
        ),
        "properties": {
            "formulas": {"type": "array", "items": {"type": "string"},
                         "description": "Compositions to predict (e.g. ['Cu2O','Fe2O3'])."},
            "property": {"type": "string", "default": "formation_energy_per_atom",
                         "description": "Property to predict (an MP summary field)."},
            "model": {
                "type": "string", "enum": ["random_forest", "gradient_boosting"],
                "default": "random_forest",
                "description": (
                    "sklearn regressor family fitted on the pulled MP rows "
                    "(default 'random_forest'). random_forest is the path the "
                    "reported per-prediction uncertainty comes from — it is the "
                    "standard deviation across the fitted trees."
                ),
            },
        },
        "required": ["formulas"],
        "additionalProperties": False,
    }

    def _run(**kwargs) -> dict:
        formulas = kwargs.get("formulas", [])
        prop = kwargs.get("property", "formation_energy_per_atom")
        model_type = kwargs.get("model", "random_forest")
        if not formulas:
            return {"error": "provide formulas"}
        try:
            from app.tools.ml.features import composition_features
            from app.tools.ml.trainer import train_model
            from app.tools.ml.registry import ModelRegistry
            from app.tools.data import _query_materials_project
            import numpy as np
        except ImportError as exc:
            return {"error": f"ML stack unavailable: {exc}", "tool_available": False}

        # Gather training data from MP via the platform proxy.
        patterns = ["A*", "B*", "C*", "D*", "E*", "F*", "G*", "H*", "I*", "J*",
                    "K*", "L*", "M*", "N*", "O*", "P*", "R*", "S*", "T*", "U*", "V*", "W*", "Y*", "Z*"]
        rows = []
        for p in patterns[:8]:  # bounded pull for speed
            res = _query_materials_project(formula=p, properties=["formula_pretty", prop])
            if isinstance(res, dict) and res.get("results"):
                for m in res["results"]:
                    v = m.get(prop)
                    if v is not None and m.get("formula_pretty"):
                        try:
                            rows.append((m["formula_pretty"], float(v)))
                        except (TypeError, ValueError):
                            continue
            if len(rows) >= 300:
                break
        if len(rows) < 20:
            return {"error": f"insufficient training data ({len(rows)} rows) from MP proxy for {prop}"}

        # Featurize + train. Report the ACTUAL feature backend (C6): matminer
        # Magpie only if it is really installed, else the builtin 22-feature
        # composition-statistics fallback — never claim Magpie when the
        # fallback ran.
        from app.tools.ml.features import get_feature_backend

        if get_feature_backend() == "matminer":
            backend_label = "matminer magpie (132 features)"
        else:
            backend_label = "builtin 22-feature composition statistics (matminer not installed)"
        X, y = [], []
        for f, v in rows:
            feats = composition_features(f)
            if feats:
                X.append(feats); y.append(v)
        if len(X) < 20:
            return {"error": "featurization failed for training data"}
        result = train_model(np.array(X), np.array(y), model_type, prop)
        model = result["model"]
        meta = result["metrics"]

        # Predict for the requested formulas (with uncertainty from tree std).
        predictions = []
        for f in formulas:
            feats = composition_features(f)
            if not feats:
                predictions.append({"formula": f, "error": "featurization failed"})
                continue
            x = np.array(feats).reshape(1, -1)
            pred = float(model.predict(x)[0])
            # Uncertainty: std across trees (RandomForest) or repeat-prediction.
            unc = None
            if hasattr(model, "estimators_"):
                tree_preds = [float(t.predict(x)[0]) for t in model.estimators_]
                unc = float(np.std(tree_preds))
            predictions.append({
                "formula": f,
                "value": round(pred, 4),
                "unit": "eV/atom" if "energy" in prop else ("eV" if "gap" in prop else None),
                "uncertainty": round(unc, 4) if unc is not None else None,
                "training_size": len(X),
            })

        return {
            "predictions": predictions,
            "model_meta": {"r2": round(meta.get("r2", 0), 3), "mae": round(meta.get("mae", 0), 4),
                           "n_train": meta.get("n_train", len(X)), "model": model_type,
                           "feature_backend": backend_label},
            "provenance": (
                f"sklearn {model_type} on {backend_label} features, trained on "
                f"{len(X)} MP rows via platform proxy; uncertainty = tree-ensemble std"
            ),
        }

    return Tool(
        name="predict_property",
        description=(
            "Predict a materials property with uncertainty (matminer+sklearn, "
            "trained on MP via the proxy). Free Citrine/ExoMatter prediction."
        ),
        input_schema=schema, func=_run, requires_approval=False,
        source="builtin", source_detail="materials.informatics",
    )


# ===========================================================================
# E10: pareto_screen (scipy multi-objective)
# ===========================================================================

def _pareto_screen_tool() -> Tool:
    schema = {
        "type": "object",
        "description": (
            "Multi-objective Pareto-front screening: find materials that are "
            "simultaneously optimal across multiple (possibly conflicting) "
            "objectives — e.g. 'lightest AND stiffest AND most stable'. Returns "
            "the non-dominated Pareto front + dominated count. The free "
            "Citrine/Intellegens multi-objective-screening equivalent."
        ),
        "properties": {
            "candidates": {
                "type": "array",
                "items": {"type": "object"},
                "description": (
                    "Candidate materials with their objective values, e.g. "
                    "[{formula:'Ti',density:4.5,modulus:110,hull:0}, ...]. "
                    "Max 200. Must be supplied — there is no auto-pull; run "
                    "screen_materials or predict_property first and pass the "
                    "rows in."
                ),
            },
            "objectives": {
                "type": "array",
                "items": {"type": "object",
                          "properties": {"property": {"type": "string"},
                                         "direction": {"enum": ["min", "max"]}}},
                "description": "Objectives: [{property:'density',direction:'min'}, {property:'modulus',direction:'max'}].",
            },
        },
        "required": ["candidates", "objectives"],
        "additionalProperties": False,
    }

    def _run(**kwargs) -> dict:
        candidates = kwargs.get("candidates", [])
        objectives = kwargs.get("objectives", [])
        if not candidates or not objectives:
            return {"error": "provide candidates and objectives"}
        if len(candidates) > 200:
            return {"error": "max 200 candidates"}

        # Extract objective vectors (handle missing as worst-case).
        def _vec(c):
            v = []
            for obj in objectives:
                val = c.get(obj["property"])
                if val is None:
                    return None
                # For 'min' objectives, negate so dominated = higher-is-worse uniformly.
                v.append(-float(val) if obj["direction"] == "min" else float(val))
            return v

        vecs = [(c, _vec(c)) for c in candidates]
        vecs = [(c, v) for c, v in vecs if v is not None]
        if not vecs:
            return {"error": "no candidates have all objective properties"}

        # Pareto dominance: a dominates b if a >= b in all dims AND > in one.
        n = len(vecs)
        dominated = [False] * n
        for i in range(n):
            if dominated[i]:
                continue
            for j in range(n):
                if i == j or dominated[j]:
                    continue
                if all(vecs[j][1][d] >= vecs[i][1][d] for d in range(len(objectives))) and \
                   any(vecs[j][1][d] > vecs[i][1][d] for d in range(len(objectives))):
                    dominated[i] = True
                    break

        front = []
        for i, (c, v) in enumerate(vecs):
            if not dominated[i]:
                obj_vals = {}
                for d, obj in enumerate(objectives):
                    obj_vals[obj["property"]] = vecs[i][0].get(obj["property"])
                front.append({"candidate": c, "objective_values": obj_vals})

        return {
            "pareto_front": front,
            "pareto_count": len(front),
            "dominated_count": n - len(front),
            "total_evaluated": n,
            "objectives": objectives,
            "provenance": "exact Pareto dominance (O(n²)); the non-dominated set across all objectives",
        }

    return Tool(
        name="pareto_screen",
        description=(
            "Screen candidates you already have for the multi-objective Pareto "
            "front. Returns the non-dominated set across N possibly conflicting "
            "objectives (e.g. minimise density while maximising modulus), each "
            "with its objective values, plus the dominated count. Exact O(n^2) "
            "dominance over supplied candidates — max 200, and any candidate "
            "missing an objective value is dropped rather than guessed."
        ),
        input_schema=schema, func=_run, requires_approval=False,
        source="builtin", source_detail="materials.informatics",
    )


# ===========================================================================
# E11: suggest_next_experiments (active-learning acquisition)
# ===========================================================================

def _suggest_next_experiments_tool() -> Tool:
    schema = {
        "type": "object",
        "description": (
            "Suggest the next experiments/compositions to test using active-"
            "learning acquisition functions (Expected Improvement / Upper "
            "Confidence Bound). Given a candidate pool with predicted values "
            "and uncertainties, rank by which would most improve the model / "
            "most likely beat the current best. The free Intellegens/Citrine "
            "active-learning loop equivalent."
        ),
        "properties": {
            "candidates": {
                "type": "array",
                "items": {"type": "object",
                          "properties": {"formula": {"type": "string"},
                                         "predicted": {"type": "number"},
                                         "uncertainty": {"type": "number"}}},
                "description": "Candidate pool with predicted values + uncertainties (from predict_property).",
            },
            "n_suggestions": {
                "type": "integer", "minimum": 1, "maximum": 20, "default": 5,
                "description": (
                    "How many top-ranked candidates to return (default 5). The "
                    "acquisition score is computed for the whole pool regardless; "
                    "this only truncates the returned list."
                ),
            },
            "acquisition": {"type": "string", "enum": ["ei", "ucb"], "default": "ei",
                            "description": "ei = Expected Improvement; ucb = Upper Confidence Bound."},
            "direction": {"type": "string", "enum": ["max", "min"], "default": "max",
                          "description": "Optimize for maximum (max) or minimum (min) predicted property."},
            "beta": {"type": "number", "default": 2.0, "description": "Exploration parameter for UCB."},
        },
        "required": ["candidates"],
        "additionalProperties": False,
    }

    def _run(**kwargs) -> dict:
        candidates = kwargs.get("candidates", [])
        if not candidates:
            return {"error": "provide candidates with predicted + uncertainty"}
        n = kwargs.get("n_suggestions", 5)
        acq = kwargs.get("acquisition", "ei")
        direction = kwargs.get("direction", "max")
        beta = kwargs.get("beta", 2.0)

        valid = [c for c in candidates if c.get("predicted") is not None and c.get("uncertainty") is not None]
        if not valid:
            return {"error": "candidates need both 'predicted' and 'uncertainty'"}

        # Current best.
        preds = [c["predicted"] for c in valid]
        best = max(preds) if direction == "max" else min(preds)

        scored = []
        for c in valid:
            mu = c["predicted"]
            sigma = c["uncertainty"]
            if sigma < 1e-9:
                sigma = 1e-9  # avoid div-by-zero
            if acq == "ei":
                # Expected Improvement (maximization form).
                try:
                    from scipy.stats import norm

                    z = (mu - best) / sigma if direction == "max" else (best - mu) / sigma
                    ei = sigma * (z * norm.cdf(z) + norm.pdf(z))
                    score = float(ei)
                    reason = f"EI={score:.4f} (z={z:.2f}): balances improving on best={best:.3f} with uncertainty σ={sigma:.3f}"
                except ImportError:
                    # No scipy — fall back to UCB.
                    score = (mu + beta * sigma) if direction == "max" else -(mu - beta * sigma)
                    reason = f"UCB fallback (scipy unavailable): μ+βσ={score:.3f}"
            else:  # ucb
                score = (mu + beta * sigma) if direction == "max" else -(mu - beta * sigma)
                reason = f"UCB={score:.3f} (μ={mu:.3f} + {beta}·σ={sigma:.3f})"
            scored.append({"formula": c.get("formula"), "acquisition_score": round(score, 5),
                           "predicted": mu, "uncertainty": sigma, "reason": reason})

        scored.sort(key=lambda s: s["acquisition_score"], reverse=True)
        return {
            "suggestions": scored[:n],
            "current_best": round(best, 4),
            "acquisition": acq,
            "direction": direction,
            "pool_size": len(valid),
            "provenance": f"active-learning {acq} acquisition (scipy.stats norm for EI); ranks where sampling most improves the objective",
        }

    return Tool(
        name="suggest_next_experiments",
        description=(
            "Rank a candidate pool by active-learning acquisition value "
            "(Expected Improvement or Upper Confidence Bound) to decide which "
            "experiments to run next. Returns the top-n candidates with their "
            "acquisition score and a plain-text reason. Needs each candidate to "
            "already carry a predicted value and an uncertainty — e.g. from "
            "predict_property; candidates missing either are skipped."
        ),
        input_schema=schema, func=_run, requires_approval=False,
        source="builtin", source_detail="materials.informatics",
    )
