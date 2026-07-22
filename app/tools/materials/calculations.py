# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""CALPHAD thermodynamic phase-stability + Scheil solidification (steel scope).

Genuine CALPHAD (pycalphad Gibbs-energy minimization + the scheil package for
Gulliver-Scheil solidification), BUT the only bundled database is the open
MatCalc mc_fe steel TDB (ODbL) — a DILUTE-STEEL assessment, valid ONLY inside
its published window (Fe-base; wt% limits like Cr<25, Ni<26, Co<3, Cu<1;
673-2000 K; no Ta at all). It is NOT an HEA database and NOT a Thermo-Calc
TCHEA substitute: equimolar HEAs (CoCrFeNiCu, NbMoTaW, ...) are 10-25x
outside the assessed window, where CALPHAD output is unassessed extrapolation,
not physics. These tools therefore REFUSE out-of-window compositions instead
of silently extrapolating. For HEA single-phase screening use
`hea_descriptors`; for 0 K stability use `phase_stability`.

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

# The bundled open TDB (MatCalc mc_fe v2.059, ODbL) — a DILUTE-STEEL
# assessment. Sourced from the pycalphad-sandbox repo (Open Database License).
_TDB_DIR = Path(__file__).parent / "tdb"
DEFAULT_TDB = "steel_odbl"

# Assessed validity window of the bundled mc_fe steel TDB, transcribed from
# the database header (steel_odbl.tdb: "This database has been optimised
# inside the following limits"). Outside these limits the Gibbs functions are
# unassessed extrapolation — the tools below REFUSE rather than extrapolate.
_STEEL_DB_ELEMENTS = {
    "FE", "AL", "B", "C", "CO", "CR", "CU", "H", "HF", "LA", "MN", "MO",
    "N", "NB", "NI", "O", "P", "PD", "S", "SI", "TI", "V", "W", "Y",
}
_STEEL_DB_WTPCT_LIMITS: dict[str, float] = {
    "AL": 3, "B": 0.5, "C": 0.5, "CO": 3, "CR": 25, "CU": 1, "HF": 0.5,
    "LA": 0.5, "MN": 25, "MO": 5, "N": 1, "NB": 1, "NI": 26, "O": 0.5,
    "P": 0.005, "PD": 4, "S": 0.5, "SI": 3.5, "TI": 0.5, "V": 0.5,
    "W": 3, "Y": 0.5,
}
_STEEL_DB_T_RANGE_K = (673.0, 2000.0)
# Default equilibrium grid — starts at 700 K, INSIDE the DB's 673 K floor
# (the old default of 500 K silently extrapolated below the assessed range).
_DEFAULT_T_RANGE = [700, 2000, 100]


def _is_bundled_steel_db(tdb_path: Path) -> bool:
    return tdb_path == _TDB_DIR / f"{DEFAULT_TDB}.tdb"


def _steel_db_window_violations(elems: list[str], fracs: dict[str, float]) -> list[str]:
    """Check a composition against the mc_fe assessed window; [] if inside."""
    violations: list[str] = []
    unknown = sorted(e for e in elems if e.upper() not in _STEEL_DB_ELEMENTS)
    if unknown:
        violations.append(
            f"element(s) {', '.join(unknown)} are not assessed in the mc_fe steel "
            "database at all"
        )
    if not any(e.upper() == "FE" for e in elems):
        violations.append("no Fe — mc_fe is a steel (Fe-base) assessment")
    try:
        from pymatgen.core import Composition

        comp = Composition({e: fracs[e] for e in elems})
        for e in elems:
            limit = _STEEL_DB_WTPCT_LIMITS.get(e.upper())
            if limit is None:
                continue
            wt_pct = 100.0 * comp.get_wt_fraction(e)
            if wt_pct > limit:
                violations.append(
                    f"{e} = {wt_pct:.1f} wt% exceeds the assessed limit of {limit} wt%"
                )
    except ImportError:
        violations.append("pymatgen unavailable — cannot verify wt% limits")
    return violations


def _refuse_out_of_window(comp: str, violations: list[str]) -> dict:
    """Honest refusal: out-of-window CALPHAD is extrapolation, not physics."""
    return {
        "error": (
            f"composition '{comp}' is outside the assessed validity window of the "
            "bundled MatCalc mc_fe DILUTE-STEEL database (steel_odbl): "
            + "; ".join(violations)
            + ". Refusing to compute — CALPHAD results outside a database's "
            "assessed window are unassessed extrapolation, not physics. No open "
            "HEA CALPHAD database ships with PRISM. For HEA single-phase "
            "screening use hea_descriptors; for 0 K DFT stability use "
            "phase_stability; or import an appropriate TDB into "
            "~/.prism/databases/ and pass database=<name>."
        ),
        "out_of_scope": True,
        "violations": violations,
        "assessed_window": {
            "database": "MatCalc mc_fe v2.059 (dilute steel, Fe-base)",
            "temperature_K": list(_STEEL_DB_T_RANGE_K),
            "wt_pct_limits": dict(_STEEL_DB_WTPCT_LIMITS),
        },
    }


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
        # String keys — downstream code (window guard + pycalphad conditions)
        # looks fractions up by the string symbol; Element-object keys would
        # KeyError there.
        fracs = {str(e): c.get_atomic_fraction(e) for e in c.elements if str(e) != "Va"}
        return elems, fracs
    except Exception:
        return None


# ===========================================================================
# E5: hea_phase_stability — equilibrium phase fractions vs T
# ===========================================================================

_HEA_PS_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Compute CALPHAD equilibrium phase stability (phase fractions vs "
        "temperature, stable phases) via pycalphad + the bundled open MatCalc "
        "mc_fe DILUTE-STEEL TDB. STEEL SCOPE ONLY: the bundled database is "
        "assessed for Fe-base compositions (e.g. Cr<25, Ni<26, Co<3, Cu<1 wt%, "
        "673-2000 K, no Ta) — out-of-window compositions such as equimolar "
        "HEAs (NbMoTaW, CoCrFeNiCu) are REFUSED rather than extrapolated. "
        "This is not an HEA database and not a Thermo-Calc TCHEA substitute. "
        "For HEA screening use hea_descriptors / phase_stability instead."
    ),
    "properties": {
        "composition": {
            "type": "string",
            "description": "Fe-base composition inside the steel window, e.g. 'Fe0.7Cr0.2Ni0.1'.",
        },
        "temperature_range": {
            "type": "array",
            "items": {"type": "number"},
            "description": (
                "[T_min, T_max, T_step] in Kelvin (default [700, 2000, 100]). "
                "The bundled steel DB is assessed 673-2000 K; requests outside "
                "that window are refused."
            ),
            "minItems": 3,
            "maxItems": 3,
        },
        "database": {
            "type": "string",
            "description": (
                "TDB database name (default 'steel_odbl', the bundled open "
                "dilute-steel DB). User TDBs in ~/.prism/databases/ are accepted "
                "but carry no assessed-window check."
            ),
        },
    },
    "additionalProperties": False,
}


def _hea_phase_stability_tool() -> Tool:
    def _run(**kwargs) -> dict:
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

        t_range = kwargs.get("temperature_range") or list(_DEFAULT_T_RANGE)

        # Scope-honesty gate (before the pycalphad check so refusal works
        # everywhere): the bundled DB is a dilute-steel assessment — refuse
        # compositions/temperatures outside its published window instead of
        # silently extrapolating.
        if _is_bundled_steel_db(tdb_path):
            violations = _steel_db_window_violations(elems, fracs)
            t_lo, t_hi = _STEEL_DB_T_RANGE_K
            if float(t_range[0]) < t_lo or float(t_range[1]) > t_hi:
                violations.append(
                    f"temperature range [{float(t_range[0]):g}, {float(t_range[1]):g}] K "
                    f"is outside the assessed {t_lo:g}-{t_hi:g} K window"
                )
            if violations:
                return _refuse_out_of_window(comp, violations)

        if not _calphad_available():
            return _missing_error()
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
                "provenance": (
                    f"pycalphad Gibbs-energy minimization + open TDB {db_info} "
                    "(MatCalc mc_fe v2.059 dilute-steel assessment, ODbL); "
                    "composition verified inside the DB's assessed window"
                ),
            }
        except Exception as exc:
            logger.exception("hea_phase_stability computation failed")
            return {"error": f"CALPHAD computation failed: {type(exc).__name__}: {exc}"}

    return Tool(
        name="hea_phase_stability",
        description=(
            "CALPHAD equilibrium phase stability (phase fractions vs temperature) "
            "via pycalphad + the bundled open MatCalc mc_fe DILUTE-STEEL TDB. "
            "Steel scope only — refuses compositions outside the DB's assessed "
            "window (equimolar HEAs are out of scope; not a TCHEA substitute)."
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
            },
            {
                "input": {"composition": "NbMoTaW"},
                "output_note": "REFUSED (out_of_scope): Ta not in the steel DB, no Fe — the bundled dilute-steel TDB cannot model refractory HEAs",
            },
        ],
    )


# ===========================================================================
# E6: scheil_solidification — non-equilibrium solidification path
# ===========================================================================

_SCHEIL_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Simulate non-equilibrium (Gulliver-Scheil) solidification of a STEEL "
        "composition: solidification path, phase fractions vs temperature, "
        "microsegregation, solidus/liquidus. Uses the open `scheil` package + "
        "pycalphad + the bundled MatCalc mc_fe DILUTE-STEEL TDB — same steel "
        "scope as hea_phase_stability: out-of-window compositions (equimolar "
        "HEAs, non-Fe-base alloys) are REFUSED rather than extrapolated. "
        "Scheil assumes perfect mixing in the liquid, no diffusion in the "
        "solid — predicts microsegregation in castings."
    ),
    "properties": {
        "composition": {
            "type": "string",
            "description": "Fe-base composition inside the steel window, e.g. 'Fe0.8Cr0.15Ni0.05'.",
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

        # Scope-honesty gate: refuse compositions outside the bundled steel
        # DB's assessed window instead of silently extrapolating.
        if _is_bundled_steel_db(tdb_path):
            violations = _steel_db_window_violations(elems, fracs)
            if violations:
                return _refuse_out_of_window(comp, violations)

        if not _calphad_available():
            return _missing_error()

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
                "provenance": (
                    f"open `scheil` package + pycalphad + TDB {tdb_path.name} "
                    "(MatCalc mc_fe dilute-steel assessment, ODbL). Gulliver-Scheil "
                    "limit (no solid diffusion); composition verified inside the "
                    "DB's assessed window"
                ),
            }
        except Exception as exc:
            logger.exception("scheil_solidification computation failed")
            return {"error": f"Scheil simulation failed: {type(exc).__name__}: {exc}"}

    return Tool(
        name="scheil_solidification",
        description=(
            "Gulliver-Scheil non-equilibrium solidification simulation for "
            "STEEL compositions: solidification path, solidus/liquidus, "
            "microsegregation. Open scheil package + pycalphad + bundled "
            "dilute-steel TDB; refuses out-of-window compositions."
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
