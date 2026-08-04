"""Core KWN precipitation-kinetics wrapper around kawin.

Everything kawin/pycalphad related is imported inside functions so this
module imports cleanly (and ``check_precipitation_available`` can gate
registration) even when the heavy deps are absent.

Unit discipline: kawin works in strict SI internally (time s, temperature
K, radii m, compositions mole fraction, diffusivity m^2/s, energies J).
All outputs are returned in SI with explicit field names; nanometre views
are derived, never substituted. A radius in metres reported as nanometres
is exactly the error class this module must not commit, so the unit lives
in the key name and the ``units`` map on the Tool.

Failure discipline: any validation error, unconverged solve, or exception
returns a structured failure dict whose values are all strings/booleans —
never a plausible-looking radius or volume fraction that did not come from
a completed solve.
"""
from __future__ import annotations

import math
from pathlib import Path
from typing import Any, Dict, List, Optional, Union

R_GAS = 8.31446261815324  # J/(mol K), CODATA

VALID_NUCLEATION_SITES = (
    "BULK",
    "DISLOCATIONS",
    "GRAIN BOUNDARIES",
    "GRAIN EDGES",
    "GRAIN CORNERS",
)

# Physical output keys of a successful solve. A failure dict must contain
# none of these (and, more strictly, no numeric values at all).
NUMERIC_RESULT_KEYS = (
    "time_s",
    "temperature_K",
    "phases",
    "matrix_composition",
    "size_distribution",
    "final",
    "n_steps",
    "equilibrium_check",
)


def _failure(stage: str, message: str, **extra: str) -> dict:
    """Structured failure: strings and booleans only, never numbers."""
    out: Dict[str, Any] = {
        "status": "failed",
        "stage": stage,
        "error": message,
        "converged": False,
    }
    for k, v in extra.items():
        out[k] = str(v)
    return out


def _is_finite_number(x) -> bool:
    return isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x)


def _resolve_tdb(database: str):
    """Resolve a TDB argument to an existing file path, or None.

    Accepts a direct path to a .tdb file, or the name of a database in the
    managed store (~/.prism/databases, see calphad_bridge.DatabaseStore).
    Never downloads anything.
    """
    p = Path(database).expanduser()
    if p.is_file():
        return p
    candidates = [p.with_suffix(".tdb")] if p.suffix == "" else []
    managed = Path.home() / ".prism" / "databases"
    for name in {p.name, p.stem + ".tdb"}:
        candidates.append(managed / name)
    for c in candidates:
        if c.is_file():
            return c
    return None


def _validate_inputs(
    database: Any,
    components: Any,
    matrix_phase: Any,
    precipitates: Any,
    matrix_composition: Any,
    diffusivity: Any,
    temperature_K: Any,
    time_s: Any,
    schedule: Any,
) -> Optional[dict]:
    """Return a failure dict for bad inputs, else None. Pure validation:
    no kawin import, no numerics produced."""
    if not isinstance(database, str) or not database.strip():
        return _failure("input_validation",
                        "'database' must be a path to a .tdb file or the "
                        "name of a database in ~/.prism/databases")
    if _resolve_tdb(database) is None:
        return _failure("input_validation",
                        f"thermodynamic database not found: {database!r} "
                        "(expected a .tdb file path or a database name in "
                        "~/.prism/databases). Nothing was downloaded — "
                        "stage the TDB on disk first.")
    if not isinstance(components, list) or len(components) < 2:
        return _failure("input_validation",
                        "'components' must be a list of >= 2 element "
                        "symbols with the solvent (matrix) element first, "
                        "e.g. ['AL', 'ZR']")
    if not all(isinstance(c, str) and c.isalpha() and c.isupper() for c in components):
        return _failure("input_validation",
                        "every entry in 'components' must be an uppercase "
                        f"element symbol, got {components!r}")
    if len(set(components)) != len(components):
        return _failure("input_validation",
                        f"'components' contains duplicates: {components!r}")
    if not isinstance(matrix_phase, str) or not matrix_phase.strip():
        return _failure("input_validation",
                        "'matrix_phase' must be the parent phase name as "
                        "it appears in the TDB, e.g. 'FCC_A1'")

    if not isinstance(precipitates, list) or not precipitates:
        return _failure("input_validation",
                        "'precipitates' must be a non-empty list of "
                        "precipitate definitions")
    solutes = components[1:]
    for i, spec in enumerate(precipitates):
        if not isinstance(spec, dict):
            return _failure("input_validation",
                            f"precipitates[{i}] must be an object")
        phase = spec.get("phase")
        if not isinstance(phase, str) or not phase.strip():
            return _failure("input_validation",
                            f"precipitates[{i}].phase must be the phase "
                            "name as it appears in the TDB")
        gamma = spec.get("interfacial_energy_J_per_m2")
        if not _is_finite_number(gamma) or gamma <= 0:
            return _failure("input_validation",
                            f"precipitates[{i}].interfacial_energy_J_per_m2 "
                            "must be a positive finite number (J/m^2). "
                            "KWN cannot nucleate without an interfacial "
                            "energy and this tool will not invent one.")
        vm = spec.get("molar_volume_m3_per_mol")
        if not _is_finite_number(vm) or vm <= 0:
            return _failure("input_validation",
                            f"precipitates[{i}].molar_volume_m3_per_mol "
                            "must be a positive finite number (m^3/mol)")
        apc = spec.get("atoms_per_unit_cell")
        if not isinstance(apc, int) or isinstance(apc, bool) or apc < 1:
            return _failure("input_validation",
                            f"precipitates[{i}].atoms_per_unit_cell must be "
                            "a positive integer")
        site = str(spec.get("nucleation_site", "BULK")).upper()
        if site not in VALID_NUCLEATION_SITES:
            return _failure("input_validation",
                            f"precipitates[{i}].nucleation_site must be one "
                            f"of {list(VALID_NUCLEATION_SITES)}, got {site!r}")

    # Matrix composition: mole fractions of the solutes.
    if isinstance(matrix_composition, dict):
        for el in matrix_composition:
            if el not in solutes:
                return _failure("input_validation",
                                f"matrix_composition element {el!r} is not "
                                f"a solute (solutes are {solutes}; the "
                                "solvent balance is implied)")
        missing = [el for el in solutes if el not in matrix_composition]
        if missing:
            return _failure("input_validation",
                            "matrix_composition missing solute(s) "
                            f"{missing}; give mole fractions for every "
                            "solute element")
        comp_vals = [matrix_composition[el] for el in solutes]
    elif isinstance(matrix_composition, list):
        if len(matrix_composition) != len(solutes):
            return _failure("input_validation",
                            f"matrix_composition list must have one mole "
                            f"fraction per solute {solutes}, got "
                            f"{len(matrix_composition)} entries")
        comp_vals = list(matrix_composition)
    else:
        return _failure("input_validation",
                        "matrix_composition must be a {element: mole "
                        "fraction} dict or a list ordered by solutes")
    for v in comp_vals:
        if not _is_finite_number(v) or not (0.0 < v < 1.0):
            return _failure("input_validation",
                            "matrix_composition values must be mole "
                            f"fractions strictly inside (0, 1), got {v!r}")
    if sum(comp_vals) >= 1.0:
        return _failure("input_validation",
                        "matrix_composition mole fractions must sum to < 1 "
                        "(the solvent carries the balance)")

    # Diffusivity: Arrhenius D(T) = D0 * exp(-Q / (R T)).
    if not isinstance(diffusivity, dict) or not diffusivity:
        return _failure("input_validation",
                        "'diffusivity' is required: either "
                        "{'D0_m2_per_s': float, 'Q_J_per_mol': float} "
                        "applied to all solutes, or {element: {D0_m2_per_s,"
                        " Q_J_per_mol}} per solute")

    def _check_arrhenius(tag: str, entry: Any) -> Optional[dict]:
        if not isinstance(entry, dict):
            return _failure("input_validation",
                            f"diffusivity[{tag!r}] must be an object with "
                            "D0_m2_per_s and Q_J_per_mol")
        d0 = entry.get("D0_m2_per_s")
        q = entry.get("Q_J_per_mol")
        if not _is_finite_number(d0) or d0 <= 0:
            return _failure("input_validation",
                            f"diffusivity[{tag!r}].D0_m2_per_s must be a "
                            "positive finite number (m^2/s)")
        if not _is_finite_number(q) or q <= 0:
            return _failure("input_validation",
                            f"diffusivity[{tag!r}].Q_J_per_mol must be a "
                            "positive finite number (J/mol)")
        return None

    if "D0_m2_per_s" in diffusivity or "Q_J_per_mol" in diffusivity:
        err = _check_arrhenius("all", diffusivity)
        if err:
            return err
    else:
        for el in diffusivity:
            if el not in solutes:
                return _failure("input_validation",
                                f"diffusivity element {el!r} is not a "
                                f"solute {solutes}")
        missing = [el for el in solutes if el not in diffusivity]
        if missing:
            return _failure("input_validation",
                            f"diffusivity missing solute(s) {missing}")
        for el, entry in diffusivity.items():
            err = _check_arrhenius(el, entry)
            if err:
                return err

    # Heat-treatment schedule: isothermal hold OR an explicit T-t profile.
    if schedule is not None:
        if not (temperature_K is None and time_s is None):
            return _failure("input_validation",
                            "give EITHER an isothermal hold "
                            "(temperature_K + time_s) OR a 'schedule' "
                            "profile, not both")
        if not isinstance(schedule, dict):
            return _failure("input_validation",
                            "'schedule' must be {times_s: [...], "
                            "temperatures_K: [...]}")
        times = schedule.get("times_s")
        temps = schedule.get("temperatures_K")
        if (not isinstance(times, list) or not isinstance(temps, list)
                or len(times) != len(temps) or len(times) < 2):
            return _failure("input_validation",
                            "schedule.times_s and schedule.temperatures_K "
                            "must be equal-length lists (>= 2 points)")
        for t in times:
            if not _is_finite_number(t) or t < 0:
                return _failure("input_validation",
                                f"schedule times must be >= 0 s, got {t!r}")
        for T in temps:
            if not _is_finite_number(T) or T <= 0:
                return _failure("input_validation",
                                f"schedule temperatures must be > 0 K, got {T!r}")
        if any(b <= a for a, b in zip(times, times[1:])):
            return _failure("input_validation",
                            "schedule.times_s must be strictly increasing")
    else:
        if not _is_finite_number(temperature_K) or temperature_K <= 0:
            return _failure("input_validation",
                            "'temperature_K' must be a positive finite "
                            "temperature in kelvin (or pass 'schedule')")
        if not _is_finite_number(time_s) or time_s <= 0:
            return _failure("input_validation",
                            "'time_s' must be a positive finite hold "
                            "duration in seconds (or pass 'schedule')")
    return None


def _build_and_solve(spec: dict) -> dict:
    """Construct the kawin model and solve it. Called only after
    validation passed; any exception becomes a structured failure."""
    from kawin.precipitation import (
        MatrixParameters,
        PrecipitateModel,
        PrecipitateParameters,
        TemperatureParameters,
        VolumeParameter,
    )
    from kawin.thermo import BinaryThermodynamics, MulticomponentThermodynamics

    # Python 3.14 / PEP 649 fix for pycalphad 0.11.2's Workspace (no-op
    # elsewhere). Without it every CALPHAD lookup inside kawin raises
    # AttributeError before any physics runs. See app/tools/pycalphad_compat.py
    # for evidence.
    from app.tools.pycalphad_compat import apply_py314_workspace_shim

    apply_py314_workspace_shim()

    tdb_path = spec["tdb_path"]
    components: List[str] = spec["components"]
    solutes = components[1:]
    matrix_phase: str = spec["matrix_phase"]
    phases = [matrix_phase] + [p["phase"] for p in spec["precipitates"]]

    if len(components) == 2:
        therm_cls = BinaryThermodynamics
    else:
        therm_cls = MulticomponentThermodynamics
    try:
        therm = therm_cls(str(tdb_path), components, phases,
                          drivingForceMethod="tangent")
    except Exception as e:
        return _failure("thermodynamics_setup",
                        f"could not build CALPHAD thermodynamics from "
                        f"{tdb_path.name} for components {components} / "
                        f"phases {phases}: {type(e).__name__}: {e}")

    # Arrhenius diffusivity in the matrix phase, m^2/s.
    diff = spec["diffusivity"]
    try:
        if "D0_m2_per_s" in diff:
            d0, q = diff["D0_m2_per_s"], diff["Q_J_per_mol"]
            therm.setDiffusivity(lambda T, d0=d0, q=q: d0 * math.exp(-q / (R_GAS * T)),
                                 matrix_phase)
        else:
            funcs = {}
            for el in solutes:
                d0, q = diff[el]["D0_m2_per_s"], diff[el]["Q_J_per_mol"]
                funcs[el] = lambda T, d0=d0, q=q: d0 * math.exp(-q / (R_GAS * T))
            therm.setDiffusivity(funcs, matrix_phase)
    except Exception as e:
        return _failure("diffusivity_setup",
                        f"failed to register diffusivity: "
                        f"{type(e).__name__}: {e}")

    try:
        matrix = MatrixParameters(solutes)
        comp_vals = spec["composition_values"]
        matrix.initComposition = comp_vals[0] if len(comp_vals) == 1 else comp_vals
        matrix.volume.setVolume(spec["matrix_molar_volume_m3_per_mol"],
                                VolumeParameter.MOLAR_VOLUME,
                                spec["matrix_atoms_per_unit_cell"])
        nuc_kwargs = {
            "grainSize": spec["grain_size_um"],
            "dislocationDensity": spec["dislocation_density_m_per_m3"],
        }
        if spec.get("bulk_site_density_m3") is not None:
            nuc_kwargs["bulkN0"] = spec["bulk_site_density_m3"]
        matrix.nucleationSites.setNucleationDensity(**nuc_kwargs)

        precipitates = []
        for p in spec["precipitates"]:
            prec = PrecipitateParameters(p["phase"])
            prec.gamma = p["interfacial_energy_J_per_m2"]
            prec.volume.setVolume(p["molar_volume_m3_per_mol"],
                                  VolumeParameter.MOLAR_VOLUME,
                                  p["atoms_per_unit_cell"])
            prec.nucleation.setNucleationType(p["nucleation_site"])
            precipitates.append(prec)
    except Exception as e:
        return _failure("parameter_setup",
                        f"failed to build KWN parameters: "
                        f"{type(e).__name__}: {e}")

    try:
        if spec.get("schedule") is not None:
            # NOTE (kawin 0.5.0 quirk, verified in source): TemperatureParameters
            # interpolates with np.interp(t/3600, times, temps) — its array
            # times are in HOURS while the model clock is seconds
            # (kawin/precipitation/PrecipitationParameters.py,
            # setTemperatureArray). Our contract is seconds everywhere, so
            # convert here at the boundary; silently passing seconds would
            # flatten the profile to its first temperature.
            times_h = [t / 3600.0 for t in spec["schedule"]["times_s"]]
            temperature = TemperatureParameters(times_h,
                                                spec["schedule"]["temperatures_K"])
            sim_time = float(spec["schedule"]["times_s"][-1])
        else:
            temperature = spec["temperature_K"]
            sim_time = float(spec["time_s"])

        model = PrecipitateModel(matrix, precipitates, therm, temperature)
        pbm = spec["pbm"]
        for p in spec["precipitates"]:
            model.setPBMParameters(cMin=pbm["cMin"], cMax=pbm["cMax"],
                                   bins=pbm["bins"], minBins=pbm["minBins"],
                                   maxBins=pbm["maxBins"], phase=p["phase"])
    except Exception as e:
        return _failure("model_setup",
                        f"failed to assemble the KWN model: "
                        f"{type(e).__name__}: {e}")

    try:
        model.solve(sim_time)
    except Exception as e:
        return _failure("solve",
                        f"KWN integration failed: {type(e).__name__}: {e}")

    return _extract_results(model, spec)


def _extract_results(model, spec: dict) -> dict:
    """Pull the solved trajectories out of kawin into a JSON-safe dict.

    All quantities come from the completed solve — nothing is estimated or
    back-filled. Units are carried in the field names (SI).
    """
    import numpy as np

    data = model.data
    n_steps = int(data.n) + 1
    if n_steps < 2:
        return _failure("solve",
                        "KWN solve produced no time steps — the integrator "
                        "did not advance; no results are reported")

    def _col(arr, p):
        vals = np.asarray(arr[:n_steps, p], dtype=float)
        return [float(v) for v in vals]

    phase_names = [p["phase"] for p in spec["precipitates"]]
    phases_out = {}
    for p, phase in enumerate(phase_names):
        radius_m = _col(data.Ravg, p)
        density = _col(data.precipitateDensity, p)
        phases_out[phase] = {
            "volume_fraction": _col(data.volFrac, p),
            "mean_radius_m": radius_m,
            "mean_radius_nm": [1e9 * r for r in radius_m],
            "number_density_m3": density,
            "nucleation_rate_m3_s": _col(data.nucRate, p),
            "critical_radius_m": _col(data.Rcrit, p),
            "driving_force_J_per_mol": _col(data.drivingForce, p),
        }

    # Final particle size distribution per phase: number density per bin.
    size_distribution = {}
    for p, phase in enumerate(phase_names):
        pbm = model.PBM[p]
        widths = np.diff(np.asarray(pbm.PSDbounds, dtype=float))
        psd = np.asarray(pbm.PSD, dtype=float)[:len(widths)]
        centers = np.asarray(pbm.PSDsize, dtype=float)[:len(widths)]
        size_distribution[phase] = {
            "radius_m": [float(r) for r in centers],
            "number_density_per_m3": [float(n) for n in psd],
            "density_per_m3_per_m": [float(n / w) if w > 0 else 0.0
                                     for n, w in zip(psd, widths)],
        }

    solutes = spec["components"][1:]
    comp = np.asarray(data.composition[:n_steps], dtype=float)
    matrix_composition = {
        el: [float(v) for v in comp[:, i]] for i, el in enumerate(solutes)
    }

    final_phases = {
        phase: {k: v[-1] for k, v in out.items() if isinstance(v, list) and v}
        for phase, out in phases_out.items()
    }

    schedule = spec.get("schedule")
    schedule_desc = (
        {"type": "temperature_time_profile",
         "n_points": len(schedule["times_s"])}
        if schedule is not None else
        {"type": "isothermal", "temperature_K": spec["temperature_K"],
         "time_s": spec["time_s"]}
    )

    return {
        "status": "ok",
        "converged": True,
        "model": "Kampmann-Wagner Numerical (kawin PrecipitateModel, Euler)",
        "database": str(spec["tdb_path"]),
        "components": list(spec["components"]),
        "matrix_phase": spec["matrix_phase"],
        "schedule": schedule_desc,
        "n_steps": n_steps,
        "time_s": [float(t) for t in data.time[:n_steps]],
        "temperature_K": [float(t) for t in data.temperature[:n_steps]],
        "phases": phases_out,
        "matrix_composition": matrix_composition,
        "size_distribution": size_distribution,
        "final": final_phases,
    }


def run_kwn(
    database: str,
    components: List[str],
    matrix_phase: str,
    precipitates: List[dict],
    matrix_composition: Union[Dict[str, float], List[float]],
    diffusivity: dict,
    temperature_K: Optional[float] = None,
    time_s: Optional[float] = None,
    schedule: Optional[dict] = None,
    matrix_molar_volume_m3_per_mol: Optional[float] = None,
    matrix_atoms_per_unit_cell: Optional[int] = None,
    grain_size_um: float = 100.0,
    dislocation_density_m_per_m3: float = 5e12,
    bulk_site_density_m3: Optional[float] = None,
    pbm: Optional[dict] = None,
) -> dict:
    """Run a KWN precipitation simulation. See the precipitation_kinetics
    tool schema for argument semantics. Returns a structured result dict;
    failures carry no numeric keys."""
    err = _validate_inputs(database, components, matrix_phase, precipitates,
                           matrix_composition, diffusivity, temperature_K,
                           time_s, schedule)
    if err:
        return err

    if not _is_finite_number(matrix_molar_volume_m3_per_mol) or \
            matrix_molar_volume_m3_per_mol <= 0:
        return _failure("input_validation",
                        "'matrix_molar_volume_m3_per_mol' must be a "
                        "positive finite number (m^3/mol)")
    if not isinstance(matrix_atoms_per_unit_cell, int) or \
            isinstance(matrix_atoms_per_unit_cell, bool) or \
            matrix_atoms_per_unit_cell < 1:
        return _failure("input_validation",
                        "'matrix_atoms_per_unit_cell' must be a positive "
                        "integer (e.g. 4 for FCC)")
    if not _is_finite_number(grain_size_um) or grain_size_um <= 0:
        return _failure("input_validation",
                        "'grain_size_um' must be a positive finite grain "
                        "size in micrometres")
    if not _is_finite_number(dislocation_density_m_per_m3) or \
            dislocation_density_m_per_m3 < 0:
        return _failure("input_validation",
                        "'dislocation_density_m_per_m3' must be a "
                        "non-negative finite dislocation line length per "
                        "volume (m/m^3)")
    if bulk_site_density_m3 is not None and (
            not _is_finite_number(bulk_site_density_m3) or bulk_site_density_m3 <= 0):
        return _failure("input_validation",
                        "'bulk_site_density_m3' must be a positive finite "
                        "site density (1/m^3) when given")

    tdb_path = _resolve_tdb(database)  # validated above; cannot be None
    solutes = components[1:]
    if isinstance(matrix_composition, dict):
        comp_vals = [matrix_composition[el] for el in solutes]
    else:
        comp_vals = list(matrix_composition)

    pbm_params = {"cMin": 1e-10, "cMax": 1e-8, "bins": 75,
                  "minBins": 50, "maxBins": 100}
    if pbm:
        if not isinstance(pbm, dict):
            return _failure("input_validation", "'pbm' must be an object")
        for key in ("cMin", "cMax"):
            if key in pbm and (not _is_finite_number(pbm[key]) or pbm[key] <= 0):
                return _failure("input_validation",
                                f"pbm.{key} must be a positive finite "
                                "radius bound in metres")
        for key in ("bins", "minBins", "maxBins"):
            if key in pbm and (not isinstance(pbm[key], int)
                               or isinstance(pbm[key], bool) or pbm[key] < 2):
                return _failure("input_validation",
                                f"pbm.{key} must be an integer >= 2")
        pbm_params.update(pbm)

    spec = {
        "tdb_path": tdb_path,
        "components": list(components),
        "matrix_phase": matrix_phase,
        "precipitates": [
            {
                "phase": p["phase"],
                "interfacial_energy_J_per_m2": p["interfacial_energy_J_per_m2"],
                "molar_volume_m3_per_mol": p["molar_volume_m3_per_mol"],
                "atoms_per_unit_cell": p["atoms_per_unit_cell"],
                "nucleation_site": str(p.get("nucleation_site", "BULK")).upper(),
            }
            for p in precipitates
        ],
        "composition_values": comp_vals,
        "diffusivity": diffusivity,
        "temperature_K": temperature_K,
        "time_s": time_s,
        "schedule": schedule,
        "matrix_molar_volume_m3_per_mol": matrix_molar_volume_m3_per_mol,
        "matrix_atoms_per_unit_cell": matrix_atoms_per_unit_cell,
        "grain_size_um": grain_size_um,
        "dislocation_density_m_per_m3": dislocation_density_m_per_m3,
        "bulk_site_density_m3": bulk_site_density_m3,
        "pbm": pbm_params,
    }
    return _build_and_solve(spec)
