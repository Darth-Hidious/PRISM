# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Cluster expansion (icet) + canonical Monte Carlo (mchammer) as tools.

"Does this alloy stay a random solid solution at temperature?" was hand-written
by the model in a sandbox every run. Two campaigns built the cluster expansion
on a parent lattice of a_eff * sqrt(2) (about 4.02 A) when every relaxation
record satisfies a_eff^3 = 2 * V/atom, so the fcc conventional parameter is
(4 * V)^(1/3), about 3.58 A: the factor is the cube root of 2, not the square
root, and volume per atom was inflated 41 %. The second campaign then fitted
at 3.6 A and sampled at 4.01 A, and nothing checked.

So the parent lattice is taken ONCE, derived from volume, sanity-checked
against the constituents' reference states, carried from fit to sampling
inside a single ClusterExpansion object, re-derived from the sampling cell and
compared, and printed. icet / trainstation / mchammer are imported lazily so
registration never fails when they are absent.
"""

from __future__ import annotations

import hashlib
import json
import logging
import time
from pathlib import Path
from typing import Any

import numpy as np

from app.tools._provenance import attach, build, file_ref, utc_now_iso
from app.tools.base import Tool, ToolRegistry
from app.tools.evidence import EvidenceSource, stamp_evidence

logger = logging.getLogger(__name__)

_CE_DIR = Path.home() / ".prism" / "cluster_expansions"
_GATE_MEV = 15.0
# Atoms per conventional cell, and primitive-edge -> conventional-edge factor.
_Z = {"fcc": 4, "bcc": 2}
_PRIM_TO_CONV = {"fcc": 2 ** 0.5, "bcc": 2 / 3 ** 0.5}


def _calc_factory():
    """Energy calculator for the training set. Tests swap this for EMT."""
    from app.tools.simulation.mace.core.calculator import make_calc

    return make_calc(head="omat_pbe", dtype="float64")


def _atoms_from_cache_ref(cache_ref: str):
    """Hydrate ASE Atoms from a cache:// CIF, as LocalBackend does."""
    import io

    from ase.io import read as ase_read

    from app.tools.simulation.mace.cache.hashing import parse_cache_uri
    from app.tools.simulation.mace_bridge import get_mace_bridge

    key, _kind = parse_cache_uri(cache_ref)
    cif = get_mace_bridge().cache.read_structure_cif(key)
    if cif is None:
        raise ValueError(f"no cached structure for {cache_ref!r} — relax or structure_import first")
    return ase_read(io.StringIO(cif), format="cif")


def _stamp(out: dict[str, Any]) -> dict[str, Any]:
    # Computed by a cited method (icet / mchammer), never above screening.
    stamp_evidence(out, EvidenceSource.CITED_COMPUTATION)
    return out


def _reference_a(fractions: dict[str, float], phase: str) -> float | None:
    """Composition-weighted reference lattice constant over the constituents
    whose ASE reference state has the same symmetry as ``phase``."""
    from ase.data import atomic_numbers, reference_states

    refs = {el: reference_states[atomic_numbers[el]] or {} for el in fractions}
    pairs = [(fractions[el], r["a"]) for el, r in refs.items() if r.get("symmetry") == phase]
    if not pairs:
        return None
    return sum(f * a for f, a in pairs) / sum(f for f, _ in pairs)


def _decorate(structure, elements, fractions, rng, exact: bool):
    """Random decoration. ``exact`` places round(f*n) of each element (canonical
    MC needs the target composition); otherwise each site is drawn independently
    so the training set spans compositions around the target and the point
    term is not collinear with the constant."""
    n, p = len(structure), [fractions[e] for e in elements]
    if exact:
        counts = [int(round(f * n)) for f in p]
        counts[int(np.argmax(counts))] += n - sum(counts)
        syms = [e for e, c in zip(elements, counts) for _ in range(c)]
        rng.shuffle(syms)
    else:
        syms = list(rng.choice(elements, size=n, p=p))
    structure.set_chemical_symbols(syms)
    return structure


def _cluster_expansion_fit(**kw: Any) -> dict[str, Any]:
    t0 = time.monotonic()
    try:
        import icet
        from ase.build import bulk
        from icet import ClusterExpansion, ClusterSpace, StructureContainer
        from trainstation import CrossValidationEstimator

        counts, phase = kw["composition"]["atoms"], kw.get("phase", "fcc")
        n_structures, reps = int(kw.get("n_structures", 40)), int(kw.get("supercell", 2))
        cutoffs = [float(c) for c in kw.get("cutoffs_A", [5.0, 4.0])]
        elements = sorted(counts)
        fractions = {e: counts[e] / sum(counts.values()) for e in elements}

        if (kw.get("lattice_a_A") is None) == (kw.get("relaxed_cache_ref") is None):
            return {"error": "give exactly one of lattice_a_A or relaxed_cache_ref"}
        if kw.get("relaxed_cache_ref"):
            atoms = _atoms_from_cache_ref(kw["relaxed_cache_ref"])
            # The conventional cube holds Z atoms: a = (Z * V/atom)^(1/3).
            # Cube root — a_eff * sqrt(2) is the mistake this tool exists for.
            a = (_Z[phase] * atoms.get_volume() / len(atoms)) ** (1 / 3)
            source = "relaxed_cache_ref"
        else:
            a, source = float(kw["lattice_a_A"]), "lattice_a_A"
        # One printed value; monte_carlo_sro re-derives it from the sampling
        # cell at the same precision and must land on the same float.
        a = round(a, 9)

        a_ref = _reference_a(fractions, phase)
        if a_ref is not None and abs(a - a_ref) / a_ref > 0.10:
            return {
                "error": (f"parent lattice {a:.3f} A is {100 * abs(a - a_ref) / a_ref:.0f}% off the "
                          f"composition-weighted {phase} reference {a_ref:.3f} A; no fit attempted"),
                "lattice_a_A": a, "reference_a_A": a_ref,
                "hint": ("a nickel-base alloy near 4.0 A is aluminium's lattice constant; check the "
                         "a_eff conversion - the factor is the cube root of 2, not the square root"),
            }
        logger.info("cluster_expansion_fit: parent lattice a = %.6f A (%s)", a, source)

        rng = np.random.default_rng(int(kw.get("seed", 20260506)))
        primitive = bulk(max(elements, key=counts.get), phase, a=a)
        cs = ClusterSpace(primitive, cutoffs, chemical_symbols=elements)
        calc, container = _calc_factory(), StructureContainer(cs)
        for _ in range(n_structures):
            s = _decorate(primitive.repeat(reps), elements, fractions, rng, exact=False)
            s.calc = calc
            container.add_structure(s, properties={"energy": s.get_potential_energy() / len(s)})
        # validate() gives the k-fold CV error; train() then fits on all data.
        cve = CrossValidationEstimator(container.get_fit_data(key="energy"),
                                       fit_method="least-squares", n_splits=min(5, n_structures))
        cve.validate()
        cve.train()
        ce = ClusterExpansion(cs, cve.parameters)
        cv_mev, train_mev = float(cve.rmse_validation) * 1e3, float(cve.rmse_train_final) * 1e3

        sidecar = {
            "parent_lattice_a_A": a, "phase": phase, "elements": elements,
            "composition_fractions": fractions, "cutoffs_A": cutoffs,
            "n_structures": n_structures, "supercell": reps,
            "cv_rmse_meV_per_atom": cv_mev, "train_rmse_meV_per_atom": train_mev,
            "n_parameters": len(ce.parameters), "calculator": type(calc).__name__,
            "created_at": utc_now_iso(),
        }
        blob = json.dumps(sidecar, sort_keys=True)
        key = hashlib.sha256(blob.encode()).hexdigest()[:24]
        _CE_DIR.mkdir(parents=True, exist_ok=True)
        (_CE_DIR / f"{key}.json").write_text(blob)
        ce.write(str(_CE_DIR / f"{key}.ce"))

        out = {
            "ce_ref": f"ce://{key}", "parent_lattice_a_A": a, "lattice_source": source,
            "reference_a_A": a_ref, "phase": phase, "elements": elements,
            "n_structures": n_structures, "n_parameters": len(ce.parameters),
            "cv_rmse_meV_per_atom": cv_mev, "train_rmse_meV_per_atom": train_mev,
            "usable": cv_mev <= _GATE_MEV, "gate_meV_per_atom": _GATE_MEV,
            "calculator": type(calc).__name__, "wall_time_s": round(time.monotonic() - t0, 2),
        }
        if a_ref is None:
            out["reference_note"] = (
                f"no constituent has an ASE {phase} reference state; lattice sanity check skipped")
        attach(out, build(
            tool_name="cluster_expansion_fit", engine="icet", engine_version=icet.__version__,
            activity="cluster_expansion.fit", inputs=kw,
            units={"parent_lattice_a_A": "A", "reference_a_A": "A", "cv_rmse_meV_per_atom": "meV/atom",
                   "train_rmse_meV_per_atom": "meV/atom", "gate_meV_per_atom": "meV/atom",
                   "wall_time_s": "s"},
            reproduce=f"cluster_expansion_fit(**{json.dumps(kw)})",
        ))
        return _stamp(out)
    except Exception as e:  # noqa: BLE001
        logger.exception("cluster_expansion_fit failed")
        return {"error": str(e), "type": type(e).__name__}


def _monte_carlo_sro(**kw: Any) -> dict[str, Any]:
    t0 = time.monotonic()
    try:
        import icet
        from icet import ClusterExpansion
        from mchammer.calculators import ClusterExpansionCalculator
        from mchammer.ensembles import CanonicalEnsemble
        from mchammer.observers import ShortRangeOrderObserver

        ref = str(kw["ce_ref"])
        key = ref.removeprefix("ce://")
        sidecar_path, ce_path = _CE_DIR / f"{key}.json", _CE_DIR / f"{key}.ce"
        if not ref.startswith("ce://") or not sidecar_path.exists():
            return {"error": f"unknown ce_ref {ref!r}; run cluster_expansion_fit first"}
        side = json.loads(sidecar_path.read_text())
        ce = ClusterExpansion.read(str(ce_path))
        temperature, phase = float(kw["temperature_K"]), side["phase"]
        n_cells, n_sweeps = int(kw.get("n_cells", 4)), int(kw.get("n_sweeps", 200))
        seed = int(kw.get("seed", 20260506))

        # The sampling cell is the CE's own primitive cell repeated, so fit and
        # MC share one lattice by construction; re-derive a from it and prove it.
        structure = _decorate(ce.primitive_structure.repeat(n_cells), side["elements"],
                              side["composition_fractions"], np.random.default_rng(seed), exact=True)
        a = round(float(structure.cell.lengths()[0]) / n_cells * _PRIM_TO_CONV[phase], 9)
        if abs(a - side["parent_lattice_a_A"]) > 1e-6:
            return {"error": (f"Monte Carlo supercell lattice {a} A does not match the fitted parent "
                              f"lattice {side['parent_lattice_a_A']} A — fit and sampling disagree"),
                    "parent_lattice_a_A": side["parent_lattice_a_A"], "supercell_lattice_a_A": a}
        logger.info("monte_carlo_sro: parent lattice a = %.6f A from the sampling cell", a)

        nn = a / 2 ** 0.5 if phase == "fcc" else a * 3 ** 0.5 / 2
        ensemble = CanonicalEnsemble(structure, ClusterExpansionCalculator(structure, ce),
                                     temperature=temperature, random_seed=seed)
        ensemble.attach_observer(ShortRangeOrderObserver(
            ce.get_cluster_space_copy(), structure, radius=nn + 0.1, interval=len(structure)))
        ensemble.run(n_sweeps * len(structure))
        # One row per sweep: mctrial, potential, acceptance_ratio (per interval),
        # sro_<A>_<B>_<shell>. Warren-Cowley is averaged over the second half.
        df = ensemble.data_container.data
        tail, cv = df.iloc[len(df) // 2:], side["cv_rmse_meV_per_atom"]
        out = {
            "ce_ref": ref, "parent_lattice_a_A": a, "temperature_K": temperature,
            "n_atoms": len(structure), "n_sweeps": n_sweeps,
            "acceptance_ratio": float(df["acceptance_ratio"].iloc[1:].mean()),
            "warren_cowley": {"-".join(c.split("_")[1:3]): float(tail[c].mean())
                              for c in df.columns if c.startswith("sro_")},
            "fit_cv_rmse_meV_per_atom": cv, "fit_usable": cv <= _GATE_MEV,
            "resolution_note": (f"the fit's CV error is {cv:.1f} meV/atom; an ordering signal whose "
                                "energy scale is comparable to that is a null result at this "
                                "resolution, not order"),
            "wall_time_s": round(time.monotonic() - t0, 2),
        }
        attach(out, build(
            tool_name="monte_carlo_sro", engine="mchammer", engine_version=icet.__version__,
            activity="cluster_expansion.monte_carlo_sro", inputs=kw,
            units={"parent_lattice_a_A": "A", "temperature_K": "K",
                   "warren_cowley": "dimensionless (Warren-Cowley alpha, first shell)",
                   "fit_cv_rmse_meV_per_atom": "meV/atom", "wall_time_s": "s"},
            derived_from=[file_ref(ce_path, "cluster_expansion"),
                          file_ref(sidecar_path, "cluster_expansion_sidecar")],
            reproduce=f"monte_carlo_sro(**{json.dumps(kw)})",
        ))
        return _stamp(out)
    except Exception as e:  # noqa: BLE001
        logger.exception("monte_carlo_sro failed")
        return {"error": str(e), "type": type(e).__name__}


_COMPOSITION_SCHEMA = {
    "type": "object",
    "description": ("Composition as INTEGER ATOM COUNTS per element (the ratio sets the target "
                    "fractions). Example: {\"atoms\": {\"Ni\": 50, \"Cu\": 50}}. NOT atomic fractions."),
    "properties": {"atoms": {
        "type": "object", "description": "Element symbol → integer atom count.",
        "patternProperties": {"^[A-Z][a-z]?$": {"type": "integer", "minimum": 0}},
        "additionalProperties": False,
    }},
    "required": ["atoms"],
}


def create_cluster_expansion_tools(registry: ToolRegistry) -> None:
    """Register cluster_expansion_fit and monte_carlo_sro (both approval-gated)."""

    registry.register(Tool(
        name="cluster_expansion_fit",
        description=(
            "Fit a cluster expansion (icet) for an alloy on an fcc or bcc parent lattice. "
            "The lattice constant is taken ONCE: from lattice_a_A, or derived from a relaxed "
            "structure's volume as (Z*V/atom)^(1/3) (Z=4 fcc, 2 bcc) — never a_eff*sqrt(2). "
            "It is checked against the constituents' reference lattice constants (>10% off "
            "is refused) and returned as parent_lattice_a_A. Training energies come from the "
            "MACE calculator; the result reports the k-fold CV RMSE and whether it clears the "
            "15 meV/atom gate. Returns ce_ref for monte_carlo_sro."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "composition": _COMPOSITION_SCHEMA,
                "phase": {"type": "string", "enum": ["fcc", "bcc"], "default": "fcc"},
                "lattice_a_A": {"type": "number", "description":
                                "Conventional lattice constant in A. Give this OR relaxed_cache_ref."},
                "relaxed_cache_ref": {"type": "string", "description":
                                      "cache:// URI from mace_relax_structure; a is derived from its volume."},
                "n_structures": {"type": "integer", "minimum": 8, "maximum": 200, "default": 40,
                                 "description": "Random training decorations."},
                "cutoffs_A": {"type": "array", "items": {"type": "number"}, "default": [5.0, 4.0],
                              "description": "icet cluster cutoffs in A by order (pairs, triplets, ...)."},
                "supercell": {"type": "integer", "minimum": 2, "maximum": 4, "default": 2,
                              "description": "Training-cell repeat of the primitive cell per axis."},
                "seed": {"type": "integer", "default": 20260506},
            },
            "required": ["composition"],
            "additionalProperties": False,
        },
        func=_cluster_expansion_fit,
        requires_approval=True,
        source="builtin",
        source_detail="app.tools.cluster_expansion",
    ))

    registry.register(Tool(
        name="monte_carlo_sro",
        description=(
            "Canonical Monte Carlo (mchammer) on a fitted cluster expansion to measure "
            "first-shell Warren-Cowley short-range order at temperature: alpha ≈ 0 is a random "
            "solid solution, alpha < 0 ordering, alpha > 0 clustering. The supercell is the CE's "
            "own primitive cell repeated, so fit and sampling share one lattice; the result echoes "
            "parent_lattice_a_A re-derived from the sampling cell and errors if it differs from "
            "the fit. Signals comparable to the fit's CV error are a null result."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "ce_ref": {"type": "string", "description": "ce:// reference from cluster_expansion_fit."},
                "temperature_K": {"type": "number", "minimum": 1, "maximum": 3000},
                "n_cells": {"type": "integer", "minimum": 3, "maximum": 6, "default": 4,
                            "description": "Primitive-cell repeats per axis."},
                "n_sweeps": {"type": "integer", "minimum": 10, "maximum": 2000, "default": 200,
                             "description": "MC sweeps (one sweep = one trial per atom)."},
                "seed": {"type": "integer", "default": 20260506},
            },
            "required": ["ce_ref", "temperature_K"],
            "additionalProperties": False,
        },
        func=_monte_carlo_sro,
        requires_approval=True,
        source="builtin",
        source_detail="app.tools.cluster_expansion",
    ))
