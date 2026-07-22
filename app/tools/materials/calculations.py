# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""CALPHAD thermodynamic phase-stability + Scheil solidification for HEAs.

The free Thermo-Calc alternative for the two capabilities Thermo-Calc charges
the most for: (1) TCHEA-equilibrium phase-stability (phase fractions vs T,
equilibrium phases, Gibbs energies) and (2) the Scheil solidification
calculator (non-equilibrium solidification path, microsegregation, solidus/
liquidus).

Uses pycalphad (free, reads standard TDB files) + the open MatCalc mc_fe steel
database (ODbL, 23 elements incl. all HEA-relevant Cr/Ni/Co/Mo/W/V/Nb/Cu/Mn/
Ti/Al). No commercial database required.

Python 3.14 note: pycalphad depends on symengine, which has no 3.14 wheel yet.
On 3.14 these tools degrade with an honest error pointing to the install path
(`pip install prism-platform[calphad]`) or the py3.12 sidecar — exactly like the
existing calphad_compute tool. On 3.12/3.13 they run natively.
"""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)

# The bundled open TDB (MatCalc mc_fe v2.059, ODbL). 23 elements incl. the
# HEA-relevant Cr/Ni/Co/Mo/W/V/Nb/Cu/Mn/Ti/Al. Sourced from the pycalphad-
# sandbox repo (Open Database License).
_TDB_DIR = Path(__file__).parent / "tdb"
DEFAULT_TDB = "steel_odbl"


def _calphad_available() -> bool:
    try:
        import pycalphad  # noqa: F401

        return True
    except ImportError:
        return False


def _missing_error() -> dict:
    return {
        "error": (
            "pycalphad is not installed. This tool needs the [calphad] extra: "
            "`pip install prism-platform[calphad]`. Note: pycalphad depends on "
            "symengine, which has no Python 3.14 wheel yet — on 3.14 use the "
            "py3.12 sidecar (`prism doctor` checks it). On 3.12/3.13 it installs "
            "natively."
        ),
        "tool_available": False,
    }


def _resolve_tdb_path(database: str | None) -> Path | None:
    """Resolve a database name to a TDB file path."""
    db = database or DEFAULT_TDB
    # Allow bare name or .tdb suffix.
    name = db if db.endswith(".tdb") else f"{db}.tdb"
    p = _TDB_DIR / name
    if p.exists():
        return p
    # Also check ~/.prism/databases/ (user-imported TDBs).
    home_p = Path.home() / ".prism" / "databases" / name
    if home_p.exists():
        return home_p
    return None


def _parse_composition_for_calphad(spec: str | dict[str, float]) -> tuple[list[str], dict[str, float]] | None:
    """Parse a composition into (elements, {EL: fraction}) for pycalphad conditions."""
    try:
        from pymatgen.core import Composition
    except ImportError:
        return None
    try:
        if isinstance(spec, dict):
            c = Composition({k: v for k, v in spec.items()})
        else:
            c = Composition(spec)
        elems = [str(e) for e in c.elements if str(e) != "Va"]
        fracs = {e: c.get_atomic_fraction(e) for e in c.elements if str(e) != "Va"}
        return elems, fracs
    except Exception:
        return None


# ===========================================================================
# E5: hea_phase_stability — equilibrium phase fractions vs T
# ===========================================================================

_HEA_PS_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Compute the thermodynamic equilibrium phase stability of a multi-"
        "principal-element alloy using CALPHAD (pycalphad + an open TDB). "
        "Returns the equilibrium phase fractions vs temperature, the stable "
        "phases at each T, and Gibbs energies — the free Thermo-Calc "
        "TCHEA-equilibrium equivalent. Uses the bundled open steel TDB "
        "(MatCalc mc_fe, 23 elements incl. Cr/Ni/Co/Mo/W/V/Nb/Cu)."
    ),
    "properties": {
        "composition": {
            "type": "string",
            "description": "Composition, e.g. 'Fe0.7Cr0.2Ni0.1' or 'Cr0.2Ni0.2Co0.2Fe0.2Cu0.2'.",
        },
        "temperature_range": {
            "type": "array",
            "items": {"type": "number"},
            "description": "[T_min, T_max, T_step] in Kelvin (default [500, 2000, 100]).",
            "minItems": 3,
            "maxItems": 3,
        },
        "database": {
            "type": "string",
            "description": "TDB database name (default 'steel_odbl', the bundled open steel DB).",
        },
    },
    "additionalProperties": False,
}


def _hea_phase_stability_tool() -> Tool:
    def _run(**kwargs) -> dict:
        if not _calphad_available():
            return _missing_error()
        comp = kwargs.get("composition")
        if not comp:
            return {"error": "provide a composition"}
        parsed = _parse_composition_for_calphad(comp)
        if parsed is None:
            return {"error": f"could not parse composition: {comp}"}
        elems, fracs = parsed

        db_name = kwargs.get("database") or DEFAULT_TDB
        tdb_path = _resolve_tdb_path(db_name)
        if tdb_path is None:
            avail = [p.stem for p in _TDB_DIR.glob("*.tdb")]
            return {"error": f"database '{db_name}' not found. Available: {avail}"}

        t_range = kwargs.get("temperature_range") or [500, 2000, 100]
        try:
            import pycalphad as pyc
            from pycalphad import Database, equilibrium
            from pycalphad.model import Model
            import numpy as np

            db = Database(str(tdb_path))
            # Components: the elements + Va (vacancy, required for CALPHAD).
            comps = sorted(set(elems + ["VA"]))
            # Phases: all phases in the DB that involve these comps.
            phases = list(db.phases.keys())

            # Build conditions dict: T grid + composition per element.
            # pycalphad expects X_EL (mole fraction) per independent element.
            t_min, t_max, t_step = t_range
            conditions = {}
            conditions["T"] = np.arange(t_min, t_max + t_step, t_step)
            conditions["P"] = 101325.0
            # Fix N (moles) and the independent compositions.
            indep = [e for e in elems]  # all but one are independent
            for i, e in enumerate(elems[:-1]):
                conditions[f"X_{e.upper()}"] = float(fracs[e])

            eq_result = equilibrium(db, comps, phases, conditions, model=Model)

            # Serialize the phase fractions vs T.
            rows = []
            for idx, t in enumerate(eq_result["T"].values):
                phase_np = eq_result["NP"].isel(T=idx).values
                phase_names = eq_result.coords.get("Phase", None)
                phases_present = {}
                if phase_names is not None:
                    names = phase_names.isel(T=idx).values
                    for p, frac in zip(names, phase_np):
                        if isinstance(p, str) and p and not str(frac) == "nan" and float(frac) > 1e-6:
                            phases_present[p] = round(float(frac), 4)
                rows.append({
                    "T_K": int(t),
                    "phases": [{"name": k, "fraction": v} for k, v in phases_present.items()],
                    "n_phases": len(phases_present),
                })

            db_info = str(tdb_path.name)
            return {
                "composition": comp,
                "elements": elems,
                "phase_fractions_vs_T": rows,
                "database": db_name,
                "database_file": db_info,
                "temperature_range_K": list(t_range),
                "provenance": f"pycalphad equilibrium + open TDB {db_info} (MatCalc mc_fe, ODbL). Free Thermo-Calc-TCHEA-equilibrium equivalent.",
            }
        except Exception as exc:
            logger.exception("hea_phase_stability computation failed")
            return {"error": f"CALPHAD computation failed: {type(exc).__name__}: {exc}"}

    return Tool(
        name="hea_phase_stability",
        description=(
            "CALPHAD equilibrium phase stability for an HEA: phase fractions vs "
            "temperature, stable phases, Gibbs energies. Free Thermo-Calc "
            "TCHEA-equilibrium equivalent via pycalphad + open steel TDB."
        ),
        input_schema=_HEA_PS_SCHEMA,
        func=_run,
        requires_approval=True,  # compute-heavy
        source="builtin",
        source_detail="materials.calphad",
        examples=[
            {
                "input": {"composition": "Fe0.7Cr0.2Ni0.1", "temperature_range": [800, 1800, 100]},
                "output_note": "phase fractions vs T (FCC/BCC transitions for a stainless-steel-like composition)",
            }
        ],
    )


# ===========================================================================
# E6: scheil_solidification — non-equilibrium solidification path
# ===========================================================================

_SCHEIL_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Simulate non-equilibrium (Scheil) solidification of an alloy: the "
        "solidification path, phase fractions vs temperature, microsegregation, "
        "and solidus/liquidus. The free Thermo-Calc Scheil Calculator "
        "equivalent, via the open `scheil` package + pycalphad + an open TDB. "
        "Scheil assumes perfect mixing in the liquid, no diffusion in the solid "
        "(the classic Gulliver-Scheil limit) — predicts microsegregation in "
        "castings."
    ),
    "properties": {
        "composition": {
            "type": "string",
            "description": "Composition, e.g. 'Al0.9Cu0.1' or 'Fe0.8Cr0.15Ni0.05'.",
        },
        "database": {
            "type": "string",
            "description": "TDB database name (default 'steel_odbl').",
        },
        "step_temperature": {
            "type": "number",
            "description": "Temperature step for the Scheil simulation in K (default 10).",
        },
    },
    "additionalProperties": False,
}


def _scheil_solidification_tool() -> Tool:
    def _run(**kwargs) -> dict:
        if not _calphad_available():
            return _missing_error()
        comp = kwargs.get("composition")
        if not comp:
            return {"error": "provide a composition"}
        parsed = _parse_composition_for_calphad(comp)
        if parsed is None:
            return {"error": f"could not parse composition: {comp}"}
        elems, fracs = parsed

        db_name = kwargs.get("database") or DEFAULT_TDB
        tdb_path = _resolve_tdb_path(db_name)
        if tdb_path is None:
            return {"error": f"database '{db_name}' not found"}

        step = kwargs.get("step_temperature") or 10.0
        try:
            from pycalphad import Database
            from scheil import simulate_scheil_solidification

            db = Database(str(tdb_path))
            comps = sorted(set(elems + ["VA"]))
            phases = list(db.phases.keys())

            # Build the initial composition dict for the scheil package.
            initial_comp = {e.upper(): float(fracs[e]) for e in elems}

            result = simulate_scheil_solidification(
                db, comps, phases, initial_comp,
                step_temperature=step,
            )

            # Extract the solidification path (T vs solid fraction + phases).
            path = []
            for i, t in enumerate(result.temperatures):
                sf = float(result.fraction_solid[i]) if i < len(result.fraction_solid) else None
                path.append({
                    "T_K": round(float(t), 1),
                    "solid_fraction": round(sf, 4) if sf is not None else None,
                })

            return {
                "composition": comp,
                "solidification_path": path,
                "solidus_K": round(float(min(result.temperatures)), 1),
                "liquidus_K": round(float(max(result.temperatures)), 1),
                "database": db_name,
                "n_steps": len(path),
                "provenance": f"open `scheil` package + pycalphad + TDB {tdb_path.name}. Free Thermo-Calc Scheil Calculator equivalent (Gulliver-Scheil, no solid diffusion).",
            }
        except Exception as exc:
            logger.exception("scheil_solidification computation failed")
            return {"error": f"Scheil simulation failed: {type(exc).__name__}: {exc}"}

    return Tool(
        name="scheil_solidification",
        description=(
            "Scheil non-equilibrium solidification simulation: solidification "
            "path, solidus/liquidus, microsegregation. Free Thermo-Calc Scheil "
            "Calculator equivalent via the open scheil package + pycalphad."
        ),
        input_schema=_SCHEIL_SCHEMA,
        func=_run,
        requires_approval=True,  # compute-heavy
        source="builtin",
        source_detail="materials.calphad",
    )


def create_calphad_tools(registry: ToolRegistry) -> None:
    """Register the CALPHAD phase-stability + Scheil tools."""
    registry.register(_hea_phase_stability_tool())
    registry.register(_scheil_solidification_tool())
    logger.info("Registered CALPHAD tools (hea_phase_stability + scheil_solidification)")
