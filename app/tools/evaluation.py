# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Tiered evaluation ladder for material candidates.

The discovery loop used to score every candidate with a millisecond
empirical formula (Yang Ω/δ + Guo-Liu VEC) and call that an evaluation.
That screen is legitimate — as a FIRST filter. This module arranges the
physics PRISM actually has into an explicit cost ladder where each rung
is more expensive and more trustworthy than the one below it:

  tier 0  empirical screen        Yang Ω/δ + Guo/Liu VEC (app/tools/materials/hea.py).
                                  Pure math, milliseconds. A screen, NOT phase equilibria.
  tier 1  MACE foundation MLIP    MACE-MP-0 (MIT weights; ASL weights stay opt-in
                                  behind MACE_ACCEPT_ASL_LICENSE — the gate in
                                  app/tools/simulation/mace/core/calculator.py is
                                  untouched). Real atomistic relaxation energy.
  tier 2  CALPHAD phase equilibria  pycalphad equilibrium against a TDB: real phase
                                  fractions + Gibbs energy, not a phase_prediction string.
  tier 3  Quantum ESPRESSO (pw.x)  DFT. Input generation + output parsing are wired
                                  (app/tools/simulation/qe); pw.x itself is NOT executed
                                  on machines without the binary — see below.

Contract (the product is the traceability):
  * One interface: ``evaluate_candidate(candidate, tier=N)``. Higher tiers
    are opt-in; the default is tier 0.
  * Escalation is cheap-first and explicit: tiers run in order 0→3 and only
    a candidate that SURVIVES tier N (its gate passes) is handed to tier N+1.
  * Every returned property sits inside the block of the tier that produced
    it, and every block carries its provenance (tier number, method, engine,
    engine version, reproduce string). Nobody can mistake an empirical
    estimate for a DFT result, because the number never travels alone.
  * A tier whose dependency is missing reports ``status: "unavailable"`` with
    an install hint and produces NO properties — the gated-registration
    pattern of ``app/tools/simulation/qe/__init__.py`` applied per candidate.
    Present or absent; never registered-but-broken.
  * NEVER fabricate a number. A tier that cannot run leaves its properties
    absent — not estimated, not defaulted. ``fallback_proposals`` was deleted
    from this repo for inventing alloy compositions; that deletion is the
    standard this module is written to.

Tier 3 honesty note: there is no pw.x binary in the development environment
this module was written in. ``evaluate_candidate`` therefore generates a real
pw.x input file (validated against ASE's espresso-in reader) but reports
``execution.status = "unavailable"`` with the reason, and returns NO energy.
The execution path is written but UNVERIFIED: exercising it requires a pw.x
install, or submission through the SLURM path in crates/compute.
"""

from __future__ import annotations

import logging
import shutil
import time
from pathlib import Path
from typing import Any, Callable, Optional

from app.tools import _provenance as prov
from app.tools.base import Tool, ToolRegistry
from app.tools.evidence import (
    EvidenceClass,
    EvidenceSource,
    coerce_evidence_class,
    roll_up_evidence,
    stamp_evidence,
)

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Tier constants
# ---------------------------------------------------------------------------

TIER_EMPIRICAL = 0
TIER_MACE = 1
TIER_CALPHAD = 2
TIER_QE = 3
MAX_TIER = 3

TIER_NAMES = {
    TIER_EMPIRICAL: "empirical_screen",
    TIER_MACE: "mace_mlip",
    TIER_CALPHAD: "calphad_equilibrium",
    TIER_QE: "quantum_espresso",
}

TIER_EVIDENCE_SOURCES = {
    TIER_EMPIRICAL: EvidenceSource.CITED_COMPUTATION,
    TIER_MACE: EvidenceSource.EXECUTION,
    TIER_CALPHAD: EvidenceSource.EXECUTION,
    TIER_QE: EvidenceSource.EXECUTION,
}

TIER_METHODS = {
    TIER_EMPIRICAL: (
        "Yang (Ω, δ) + Guo/Liu (VEC) empirical screening — heuristic "
        "first-pass filter, NOT phase equilibria (app/tools/materials/hea.py)"
    ),
    TIER_MACE: (
        "MACE foundation machine-learned interatomic potential: BCC supercell "
        "construction + geometry relaxation (app/tools/simulation/mace)"
    ),
    TIER_CALPHAD: (
        "CALPHAD phase equilibria via pycalphad.equilibrium against a TDB "
        "database (app/tools/simulation/calphad_bridge.py)"
    ),
    TIER_QE: (
        "Quantum ESPRESSO pw.x DFT via ASE espresso-in/out "
        "(app/tools/simulation/qe)"
    ),
}

TIER_INSTALL_HINTS = {
    TIER_MACE: (
        "Install the [mace] extra into the PRISM venv: "
        "~/.prism/venv/bin/python -m pip install 'mace-torch>=0.3.12' "
        "'torch>=2.5.0' 'ase>=3.23.0' 'huggingface-hub' (or "
        "pip install 'prism-platform[mace]'). Never use "
        "--break-system-packages."
    ),
    TIER_CALPHAD: (
        "Install the [calphad] extra into the PRISM venv: "
        "~/.prism/venv/bin/python -m pip install 'pycalphad>=0.10,<0.12' "
        "(or pip install 'prism-platform[calphad]')."
    ),
    TIER_QE: (
        "Tier 3 needs (a) the [qe] extra for input/output handling: "
        "~/.prism/venv/bin/python -m pip install 'ase>=3.23.0' "
        "'pymatgen>=2024.1.1', AND (b) a pw.x binary on PATH — install "
        "Quantum ESPRESSO, or submit the generated .in file through the "
        "SLURM path in crates/compute."
    ),
}


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------

def _parse_candidate_composition(candidate: dict) -> tuple[list[str], list[float]]:
    """Validate direct evaluator input without silently normalizing it."""
    from app.tools.materials.hea import _parse_composition_or_raise

    fracs_dict = candidate.get("fractions")
    formula = candidate.get("composition")
    spec = fracs_dict if fracs_dict else formula
    if not spec:
        raise ValueError("candidate needs a composition or fractions dict")
    return _parse_composition_or_raise(spec)


def _reduced_formula(elems: list[str], fracs: list[float]) -> str:
    parts = []
    for e, f in zip(elems, fracs):
        s = f"{f:.4f}".rstrip("0").rstrip(".")
        parts.append(e if s == "1" else f"{e}{s}")
    return "".join(parts)


def _scale_to_atoms(fracs: dict[str, float], n_atoms: int) -> dict[str, int] | None:
    """Largest-remainder allocation of fractions to integer atom counts.

    Returns None when some element would get zero atoms — a supercell that
    silently drops an element would evaluate a different material, which is
    exactly the fabrication this module must not do.
    """
    raw = {e: f * n_atoms for e, f in fracs.items()}
    counts = {e: int(v) for e, v in raw.items()}
    leftover = n_atoms - sum(counts.values())
    order = sorted(raw, key=lambda e: (-(raw[e] - counts[e]), e))
    for e in order[:leftover]:
        counts[e] += 1
    if any(c <= 0 for c in counts.values()):
        return None
    return counts


def _unavailable(reason: str, install_hint: str | None = None) -> dict:
    block = {"status": "unavailable", "reason": reason}
    if install_hint:
        block["install_hint"] = install_hint
    stamp_evidence(block, EvidenceSource.MODEL_ASSERTION)
    return block


def _not_attempted(reason: str) -> dict:
    block = {"status": "not_attempted", "reason": reason}
    stamp_evidence(block, EvidenceSource.MODEL_ASSERTION)
    return block


def _failed(error: str, **extra: Any) -> dict:
    block = {"status": "failed", "error": error}
    block.update(extra)
    stamp_evidence(block, EvidenceSource.MODEL_ASSERTION)
    return block


def _error_result(message: str) -> dict:
    result = {"error": message}
    stamp_evidence(result, EvidenceSource.MODEL_ASSERTION)
    return result


# ---------------------------------------------------------------------------
# Tier availability (cheap, import-light until the tier demands it)
# ---------------------------------------------------------------------------

def tier_status() -> dict[str, dict]:
    """Availability of every tier on THIS machine, with install hints.

    Follows the gated-registration pattern: each check asks whether the tier
    can actually run, and says what is missing when it cannot.
    """
    status: dict[str, dict] = {}

    # Tier 0: pure math + literature parameter tables. Always present; when
    # pymatgen is missing, δ/Tm come back None inside the result, which the
    # screen reports honestly rather than papering over.
    status["0"] = {
        "tier": TIER_EMPIRICAL,
        "name": TIER_NAMES[0],
        "method": TIER_METHODS[0],
        "available": True,
    }

    try:
        from app.tools.simulation.mace_bridge import check_mace_available
        mace_ok = check_mace_available()
    except Exception:
        mace_ok = False
    status["1"] = {
        "tier": TIER_MACE,
        "name": TIER_NAMES[1],
        "method": TIER_METHODS[1],
        "available": bool(mace_ok),
        **({} if mace_ok else {
            "reason": "mace-torch / ase / python-ulid not all importable",
            "install_hint": TIER_INSTALL_HINTS[TIER_MACE],
        }),
    }

    try:
        from app.tools.simulation.calphad_bridge import check_calphad_available
        calphad_ok = check_calphad_available()
    except Exception:
        calphad_ok = False
    status["2"] = {
        "tier": TIER_CALPHAD,
        "name": TIER_NAMES[2],
        "method": TIER_METHODS[2],
        "available": bool(calphad_ok),
        **({} if calphad_ok else {
            "reason": "pycalphad not importable",
            "install_hint": TIER_INSTALL_HINTS[TIER_CALPHAD],
        }),
    }

    try:
        from app.tools.simulation.qe import check_qe_available
        qe_io_ok = check_qe_available()
    except Exception:
        qe_io_ok = False
    pw_path = shutil.which("pw.x")
    status["3"] = {
        "tier": TIER_QE,
        "name": TIER_NAMES[3],
        "method": TIER_METHODS[3],
        "available": bool(qe_io_ok and pw_path),
        # Input generation / output parsing need only ase + pymatgen; the
        # pw.x run itself needs the binary. Reporting the split keeps a
        # machine without QE from looking more broken (or more capable)
        # than it is.
        "io_available": bool(qe_io_ok),
        "execution_available": bool(pw_path),
        "pw_x_path": pw_path,
        **({} if qe_io_ok else {
            "reason": "ase / pymatgen not importable",
        }),
        **({} if pw_path else {
            "execution_reason": "no pw.x binary on PATH",
            "install_hint": TIER_INSTALL_HINTS[TIER_QE],
        }),
    }
    for block in status.values():
        stamp_evidence(block, EvidenceSource.EXECUTION)
    return status


# ---------------------------------------------------------------------------
# Tier 0 — empirical screen (existing, kept verbatim as the first filter)
# ---------------------------------------------------------------------------

_TIER0_UNITS = {
    "delta_H_mix_kJ_per_mol": "kJ/mol",
    "delta_S_mix_J_per_molK": "J/(mol·K)",
    "omega": "dimensionless",
    "VEC": "valence electrons/atom",
    "delta_radius_pct": "%",
    "delta_chi": "Pauling",
    "Tm_estimate_K": "K",
}


def _run_tier0(candidate: dict, elems: list[str], fracs: list[float]) -> dict:
    from app.tools.materials.hea import compute_hea_descriptors

    props = compute_hea_descriptors(
        elems,
        fracs,
        input_evidence_class=candidate.get(
            "evidence_class", EvidenceClass.INDETERMINATE
        ),
    )
    # The empirical screen cannot fail on missing deps, but a composition
    # missing radii yields omega=None → no Yang verdict. That is an absent
    # prediction, not a pass.
    gate_passed = props.get("phase_prediction") == "solid_solution" or (
        props.get("phase_prediction") == "solid_solution_segregation_risk"
    )
    gate_reason = (
        f"phase_prediction={props.get('phase_prediction')!r}"
        + (" (segregation risk flagged)" if props.get("segregation_risk") else "")
    )
    block = {
        "tier": TIER_EMPIRICAL,
        "name": TIER_NAMES[0],
        "method": TIER_METHODS[0],
        "status": "ok",
        "properties": props,
        "gate": {
            "passed": gate_passed,
            "rule": "phase_prediction in {solid_solution, solid_solution_segregation_risk}",
            "reason": gate_reason,
        },
    }
    return prov.attach(block, prov.build(
        tool_name="evaluate_candidate",
        engine="empirical-descriptor-screen",
        engine_version="literature parameters (Yang-Zhang 2012; Guo-Liu 2011; Takeuchi-Inoue 2005)",
        activity="compute_hea_descriptors",
        inputs={"elements": elems, "fractions": fracs},
        units=_TIER0_UNITS,
        reproduce=(
            f"evaluate_candidate(candidate={{'composition': "
            f"'{_reduced_formula(elems, fracs)}'}}, tier=0)"
        ),
        extra={"tier": TIER_EMPIRICAL},
    ))


# ---------------------------------------------------------------------------
# Tier 1 — MACE foundation potential
# ---------------------------------------------------------------------------

_TIER1_UNITS = {
    "energy_per_atom_eV": "eV/atom",
    "volume_per_atom_A3": "Å^3/atom",
    "lattice_a_eff_A": "Å",
    "fmax_final_eV_per_A": "eV/Å",
    "wall_time_s": "s",
}


def _run_tier1(candidate: dict, elems: list[str], fracs: list[float]) -> dict:
    from app.tools.simulation.mace_bridge import check_mace_available

    if not check_mace_available():
        return _unavailable(
            "mace-torch / ase / python-ulid not all importable in this interpreter",
            TIER_INSTALL_HINTS[TIER_MACE],
        )

    phase = candidate.get("phase", "bcc")
    # The MACE local backend's supercell builder works in percentages: the
    # composition must sum to exactly 100 atoms. Anything else is rejected
    # by the builder, so the ladder enforces it up front with a clear error.
    n_atoms = int(candidate.get("n_atoms", 100))
    counts = _scale_to_atoms(dict(zip(elems, fracs)), n_atoms)
    if counts is None:
        return _failed(
            f"composition cannot be represented with {n_atoms} atoms — some "
            "element would get zero atoms; increase n_atoms (must keep every "
            "element present)"
        )

    from app.tools.mace import _run_async
    from app.tools.simulation.mace.control import get_job
    from app.tools.simulation.mace.primitives import relax_structure
    from app.tools.simulation.mace.schemas import (
        GetJobInput,
        RelaxStructureInput,
    )
    from app.tools.simulation.mace_bridge import get_mace_bridge

    try:
        inp = RelaxStructureInput(
            composition={"atoms": counts},
            phase=phase,
            n_atoms=n_atoms,
        )
    except Exception as e:
        return _failed(f"tier 1 input rejected by MACE schema: {e}")

    bridge = get_mace_bridge()
    try:
        handle = _run_async(relax_structure(inp, bridge.runner, bridge.backends))
    except Exception as e:
        return _failed(f"MACE relax_structure submission failed: {e}")

    result = handle.result if handle.cache_hit else None
    job_id = handle.job_id
    if result is None:
        timeout_s = float(candidate.get("mace_timeout_s", 3600.0))
        deadline = time.time() + timeout_s
        poll_s = float(candidate.get("mace_poll_s", 2.0))
        rec = None
        while time.time() < deadline:
            rec = _run_async(get_job(GetJobInput(job_id=job_id), bridge.runner))
            if rec.status in ("succeeded", "failed", "cancelled"):
                break
            time.sleep(poll_s)
        else:
            return _failed(
                f"MACE job {job_id} still running after {timeout_s:.0f}s "
                f"(last status: {getattr(rec, 'status', 'unknown')})"
            )
        if rec.status != "succeeded":
            err = rec.error or {}
            return _failed(
                f"MACE job {job_id} ended as {rec.status}: "
                f"{err.get('message', err)}"
            )
        result = rec.result

    if not isinstance(result, dict) or "error" in (result or {}):
        return _failed(f"MACE job {job_id} returned no usable result: {result!r}")

    props = {k: result.get(k) for k in (
        "energy_per_atom_eV",
        "volume_per_atom_A3",
        "lattice_a_eff_A",
        "n_steps",
        "fmax_final_eV_per_A",
        "wall_time_s",
    ) if result.get(k) is not None}

    # Which weights produced these numbers is a licensing fact as much as a
    # physics fact — record the RESOLVED model (MIT MACE-MP-0 by default; ASL
    # weights only behind MACE_ACCEPT_ASL_LICENSE).
    from app.tools.simulation.mace.core.calculator import calc_signature, resolve_model

    try:
        sig = calc_signature("omat_pbe", "resolved-at-runtime", "float64")
    except Exception:
        repo, fname, licence = resolve_model()
        sig = {"repo_id": repo, "filename": fname, "license": licence}
    try:
        from importlib.metadata import version as _pkg_version
        mace_version = _pkg_version("mace-torch")
    except Exception:
        mace_version = "unknown"

    block = {
        "tier": TIER_MACE,
        "name": TIER_NAMES[1],
        "method": TIER_METHODS[1],
        "status": "ok",
        "job_id": job_id,
        "cache_hit": bool(handle.cache_hit),
        "model": sig,
        "structure": {"phase": phase, "n_atoms": n_atoms, "atom_counts": counts},
        "properties": props,
        "gate": {
            "passed": True,
            "rule": "relaxation succeeded (job status == succeeded)",
            "reason": f"MACE job {job_id} succeeded",
        },
    }
    return prov.attach(block, prov.build(
        tool_name="evaluate_candidate",
        engine="mace-torch",
        engine_version=mace_version,
        activity="relax_structure (MACE foundation potential)",
        inputs={
            "atom_counts": counts,
            "phase": phase,
            "n_atoms": n_atoms,
            "job_id": job_id,
            "cache_key": handle.cache_key,
        },
        units=_TIER1_UNITS,
        reproduce=(
            f"evaluate_candidate(candidate={{'composition': "
            f"'{_reduced_formula(elems, fracs)}', 'phase': {phase!r}, "
            f"'n_atoms': {n_atoms}}}, tier=1)"
        ),
        extra={"tier": TIER_MACE, "model": sig},
    ))


# ---------------------------------------------------------------------------
# Tier 2 — CALPHAD phase equilibria
# ---------------------------------------------------------------------------

_TIER2_UNITS = {
    "gibbs_energy": "J/mol-atom",
    "phase_fractions": "mole fraction of total moles of phase (dimensionless)",
    "temperature_K": "K",
    "pressure_Pa": "Pa",
}


def _find_covering_database(bridge, elems: list[str]) -> str | None:
    """Name of a managed TDB whose elements cover the candidate, else None.

    TDB files store element names uppercase (``AL``, ``ZR``) and carry
    non-element entries (``VA`` vacancies, ``/-`` electrons), so the
    comparison is upper-cased and restricted to real elements.
    """
    wanted = {e.upper() for e in elems}
    for db_info in bridge.databases.list_databases():
        try:
            db = bridge.databases.load(db_info["name"])
            if db is None:
                continue
            db_elems = {str(e).upper() for e in getattr(db, "elements", [])}
            if wanted <= db_elems:
                return db_info["name"]
        except Exception:
            continue
    return None


def _run_tier2(candidate: dict, elems: list[str], fracs: list[float]) -> dict:
    from app.tools.simulation.calphad_bridge import (
        check_calphad_available,
        get_calphad_bridge,
    )

    if not check_calphad_available():
        return _unavailable(
            "pycalphad not importable in this interpreter",
            TIER_INSTALL_HINTS[TIER_CALPHAD],
        )

    bridge = get_calphad_bridge()
    available_dbs = [d["name"] for d in bridge.databases.list_databases()]

    db_name = candidate.get("calphad_database")
    if db_name:
        if bridge.databases.load(db_name) is None:
            return _failed(
                f"CALPHAD database '{db_name}' not found in "
                f"{bridge.databases.base_dir}. Available: {available_dbs or 'none'}"
            )
    else:
        db_name = _find_covering_database(bridge, elems)
        if db_name is None:
            return _unavailable(
                "no thermodynamic database covering "
                f"{_reduced_formula(elems, fracs)} found in "
                f"{bridge.databases.base_dir} "
                f"(available: {available_dbs or 'none'}). A CALPHAD result "
                "requires a TDB for this system — none is synthesized.",
                "Import a TDB covering these elements into ~/.prism/databases "
                "(calphad_import_database tool), or pass calphad_database=<name>.",
            )

    T = float(candidate.get("temperature_K", 1300.0))
    P = float(candidate.get("pressure_Pa", 101325.0))
    # pycalphad/TDB convention is UPPERCASE element names ("AL", "ZR");
    # lowercase components die inside equilibrium() with an opaque
    # "list.index(x): x not in list". Normalise here, once, at the edge.
    comps = [e.upper() for e in elems]
    conditions: dict[str, Any] = {"T": T, "P": P}
    # n-1 independent mole-fraction conditions; the last element is implicit.
    for e, f in zip(comps[:-1], fracs[:-1]):
        conditions[f"X({e})"] = float(f)

    res = bridge.calculate_equilibrium(
        database_name=db_name,
        components=comps,
        phases=None,
        conditions=conditions,
    )
    if "error" in res:
        return _failed(res["error"], database=db_name)

    props = {
        "phases_present": res.get("phases_present"),
        "phase_fractions": res.get("phase_fractions"),
        "gibbs_energy": res.get("gibbs_energy"),
        "temperature_K": T,
        "pressure_Pa": P,
        "database": db_name,
    }
    block = {
        "tier": TIER_CALPHAD,
        "name": TIER_NAMES[2],
        "method": TIER_METHODS[2],
        "status": "ok",
        "properties": props,
        "gate": {
            "passed": True,
            "rule": "equilibrium converged and returned phases + Gibbs energy",
            "reason": f"pycalphad equilibrium on '{db_name}' succeeded",
        },
    }
    # The bridge already built full provenance (TDB hash, pycalphad version,
    # reproduce string). Carry it as THIS block's provenance, with the tier
    # number added — do not double-provenance.
    tier_prov = res.get("provenance") or {}
    tier_prov = dict(tier_prov)
    tier_prov["tier"] = TIER_CALPHAD
    block["provenance"] = tier_prov
    return block


# ---------------------------------------------------------------------------
# Tier 3 — Quantum ESPRESSO (pw.x)
# ---------------------------------------------------------------------------

_TIER3_UNITS = {
    "total_energy_ev": "eV",
    "forces_ev_per_angstrom": "eV/Å",
    "stress_ev_per_angstrom3": "eV/Å^3",
    "wall_time_s": "s",
}


def _build_bcc_supercell(elems: list[str], fracs: list[float], n_atoms: int):
    """Ideal BCC supercell for input generation ONLY.

    Lattice constant is the fraction-weighted average of the elemental BCC
    lattice constants (ASE's experimental values). This is an initial
    structure for pw.x to relax — never a reported property. Returns ASE
    Atoms, or raises ValueError with an honest reason.
    """
    from ase.build import bulk

    a_eff = 0.0
    for e, f in zip(elems, fracs):
        try:
            # cubic=True: the primitive (cubic=False) BCC cell has negative
            # diagonal entries (a/2 * [-1,1,1] basis) and would poison the
            # weighted average.
            a_eff += f * float(bulk(e, "bcc", cubic=True).cell[0, 0])
        except Exception as exc:
            raise ValueError(
                f"no BCC reference lattice for element {e!r}: {exc}"
            ) from exc
    if not a_eff > 0:
        raise ValueError(f"degenerate lattice constant a_eff={a_eff!r}")

    cell = bulk(elems[0], "bcc", a=a_eff, cubic=True)  # 2-atom cell
    rep = max(1, round((n_atoms / len(cell)) ** (1 / 3)))
    atoms = cell.repeat((rep, rep, rep))
    counts = _scale_to_atoms(dict(zip(elems, fracs)), len(atoms))
    if counts is None:
        raise ValueError(
            f"composition cannot be represented in a {len(atoms)}-atom BCC "
            "supercell without dropping an element"
        )
    symbols: list[str] = []
    for e in sorted(counts):
        symbols.extend([e] * counts[e])
    atoms.set_chemical_symbols(symbols)
    return atoms, counts, a_eff


def _run_tier3(candidate: dict, elems: list[str], fracs: list[float]) -> dict:
    try:
        from app.tools.simulation.qe import check_qe_available
        io_ok = check_qe_available()
    except Exception:
        io_ok = False
    if not io_ok:
        return _unavailable(
            "ase / pymatgen not importable — QE input/output handling absent",
            TIER_INSTALL_HINTS[TIER_QE],
        )

    from app.tools.simulation.qe import (
        PseudopotentialNotFoundError,
        resolve_pseudopotentials,
        write_input,
    )

    pw_path = shutil.which("pw.x")
    pseudo_dir = Path(candidate.get("qe_pseudo_dir")
                      or Path.home() / ".prism" / "pseudos")
    workdir = Path(candidate.get("qe_workdir")
                   or Path.home() / ".prism" / "evaluations"
                   / _reduced_formula(elems, fracs))
    workdir.mkdir(parents=True, exist_ok=True)

    n_atoms = int(candidate.get("qe_n_atoms", 16))
    try:
        atoms, counts, a_eff = _build_bcc_supercell(elems, fracs, n_atoms)
    except ValueError as e:
        return _failed(str(e))

    if not pseudo_dir.is_dir():
        return _unavailable(
            f"pseudopotential directory {pseudo_dir} does not exist — pw.x "
            "inputs are never written with guessed pseudopotentials",
            "Download UPF pseudopotentials (e.g. from the QE SSSP library) "
            f"into {pseudo_dir}, one file per element: "
            + ", ".join(f"{e}.upf" for e in elems),
        )
    try:
        pseudos = resolve_pseudopotentials(elems, pseudo_dir)
    except PseudopotentialNotFoundError as e:
        return _failed(str(e))

    calc_type = candidate.get("qe_calculation", "vc-relax")
    kpoints = tuple(candidate.get("qe_kpoints", (6, 6, 6)))
    ecutwfc = float(candidate.get("qe_ecutwfc_ry", 60.0))
    in_path = workdir / "pw.in"
    out_path = workdir / "pw.out"
    try:
        write_input(
            structure=atoms,
            pseudopotentials=pseudos,
            cutoffs={"ecutwfc": ecutwfc},
            kpoints=kpoints,
            calculation_type=calc_type,
            path=in_path,
            pseudo_dir=str(pseudo_dir),
        )
    except Exception as e:
        return _failed(f"pw.x input generation failed: {e}")

    input_block = {
        "input_file": str(in_path),
        "calculation": calc_type,
        "ecutwfc_ry": ecutwfc,
        "kpoints": list(kpoints),
        "pseudopotentials": {k: v for k, v in pseudos.items()},
        "structure_note": (
            f"ideal BCC supercell, {len(atoms)} atoms "
            f"({counts}), a_eff={a_eff:.4f} Å (fraction-weighted elemental "
            "lattice constants) — initial structure only, to be relaxed by pw.x"
        ),
    }

    if pw_path is None:
        # Wired but UNVERIFIED: no pw.x binary on this machine. Input
        # generation is real; execution is absent — no energy, ever.
        return {
            "tier": TIER_QE,
            "name": TIER_NAMES[3],
            "method": TIER_METHODS[3],
            "status": "input_generated",
            "input": input_block,
            "properties": {},
            "execution": {
                "status": "unavailable",
                "verified": False,
                "reason": "no pw.x binary on PATH — DFT was NOT executed",
                "install_hint": TIER_INSTALL_HINTS[TIER_QE],
            },
            "gate": {
                "passed": False,
                "rule": "pw.x run converged",
                "reason": "cannot gate on a run that did not happen",
            },
            "provenance": prov.build(
                tool_name="evaluate_candidate",
                engine="quantum-espresso",
                engine_version="absent (pw.x not installed)",
                activity="write_input only — execution unavailable",
                inputs=input_block,
                units={},
                reproduce=(
                    f"evaluate_candidate(candidate={{'composition': "
                    f"'{_reduced_formula(elems, fracs)}'}}, tier=3)"
                ),
                extra={"tier": TIER_QE},
            ),
        }

    # --- UNVERIFIED execution path: requires a real pw.x install. Written
    # --- against the documented pw.x CLI (pw.x -in file), never exercised
    # --- on the machine this code was produced on.
    import subprocess

    try:
        with open(out_path, "w") as out_f:
            subprocess.run(
                [pw_path, "-in", str(in_path)],
                stdout=out_f,
                stderr=subprocess.STDOUT,
                cwd=str(workdir),
                timeout=float(candidate.get("qe_timeout_s", 86400.0)),
                check=False,
            )
    except Exception as e:
        return _failed(f"pw.x execution failed: {e}", input=input_block)

    from app.tools.simulation.qe import parse_output

    parsed = parse_output(out_path)
    if parsed.get("status") != "ok":
        return _failed(
            f"pw.x run did not produce a converged result: {parsed}",
            input=input_block,
            output_file=str(out_path),
        )
    props = {k: parsed.get(k) for k in (
        "total_energy_ev", "forces_ev_per_angstrom",
        "stress_ev_per_angstrom3", "wall_time_s",
    ) if parsed.get(k) is not None}
    block = {
        "tier": TIER_QE,
        "name": TIER_NAMES[3],
        "method": TIER_METHODS[3],
        "status": "ok",
        "input": input_block,
        "output_file": str(out_path),
        "properties": props,
        "gate": {
            "passed": True,
            "rule": "pw.x run converged (QE convergence marker + JOB DONE)",
            "reason": "parse_output reported status=ok, converged=True",
        },
    }
    return prov.attach(block, prov.build(
        tool_name="evaluate_candidate",
        engine="quantum-espresso",
        engine_version="pw.x (UNVERIFIED path)",
        activity="pw.x scf/relax + parse_output",
        inputs=input_block,
        units=_TIER3_UNITS,
        reproduce=f"run pw.x -in {in_path} then parse_output('{out_path}')",
        extra={"tier": TIER_QE},
    ))


# ---------------------------------------------------------------------------
# The ladder
# ---------------------------------------------------------------------------

_TIER_RUNNERS: dict[int, Callable[[dict, list[str], list[float]], dict]] = {
    TIER_EMPIRICAL: _run_tier0,
    TIER_MACE: _run_tier1,
    TIER_CALPHAD: _run_tier2,
    TIER_QE: _run_tier3,
}


def evaluate_candidate(candidate: dict, tier: int = 0) -> dict:
    """Evaluate one candidate up to the requested fidelity tier.

    Tiers ALWAYS run in ascending order (cheap-first) and stop escalating
    the moment a gate fails or a tier is unavailable: a candidate that did
    not survive tier N is never handed to tier N+1. Higher tiers are opt-in
    via ``tier`` — the default is the empirical screen only.
    """
    if not isinstance(tier, int) or not 0 <= tier <= MAX_TIER:
        return _error_result(f"tier must be an int in 0..{MAX_TIER}, got {tier!r}")
    try:
        input_evidence = coerce_evidence_class(
            candidate.get("evidence_class", EvidenceClass.INDETERMINATE)
        )
        elems, fracs = _parse_candidate_composition(candidate)
    except (TypeError, ValueError) as exc:
        return _error_result(f"invalid candidate: {exc}")

    result: dict[str, Any] = {
        "candidate": {
            "composition": _reduced_formula(elems, fracs),
            "elements": elems,
            "fractions": [round(f, 6) for f in fracs],
            "evidence_class": input_evidence.value,
            "evidence_color": input_evidence.color,
        },
        "requested_tier": tier,
        "tiers": {},
    }

    blocked_reason: str | None = None
    highest_completed = -1
    for t in range(0, tier + 1):
        label = TIER_NAMES[t]
        if blocked_reason is not None:
            block = _not_attempted(blocked_reason)
        else:
            try:
                block = _TIER_RUNNERS[t](candidate, elems, fracs)
            except Exception as e:  # a crashing tier must not fabricate
                logger.exception("tier %d crashed", t)
                block = _failed(f"tier {t} crashed: {e}")
            block.setdefault("tier", t)
            block.setdefault("name", label)
            production_source = (
                TIER_EVIDENCE_SOURCES[t]
                if block.get("status") == "ok"
                else EvidenceSource.MODEL_ASSERTION
            )
            properties = block.get("properties")
            if (
                isinstance(properties, dict)
                and properties
                and "evidence_class" not in properties
            ):
                stamp_evidence(properties, production_source, [input_evidence])
            block_class = stamp_evidence(block, production_source, [input_evidence])
            provenance = block.get("provenance")
            if isinstance(provenance, dict):
                provenance["evidence_class"] = block_class.value
                provenance["evidence_color"] = block_class.color
            if block.get("status") == "ok":
                highest_completed = t
                gate = block.get("gate", {})
                if not gate.get("passed", False):
                    blocked_reason = (
                        f"candidate did not survive tier {t} "
                        f"({label}): {gate.get('reason', 'gate failed')}"
                    )
            elif block.get("status") == "unavailable":
                blocked_reason = (
                    f"tier {t} ({label}) unavailable: {block.get('reason')}"
                )
            else:
                blocked_reason = (
                    f"tier {t} ({label}) failed: {block.get('error', block.get('reason', 'unknown'))}"
                )
        result["tiers"][str(t)] = block

    result["highest_completed_tier"] = highest_completed
    result["stopped_at"] = (
        None if blocked_reason is None or highest_completed == tier
        else {"tier": highest_completed + 1, "reason": blocked_reason}
    )
    reported_properties = [
        block["properties"]
        for block in result["tiers"].values()
        if isinstance(block.get("properties"), dict) and block["properties"]
    ]
    roll_up_evidence(result, reported_properties)
    return result


def escalate_candidates(candidates: list[dict], max_tier: int = MAX_TIER) -> dict:
    """Batch form of the ladder: every candidate through tiers 0..max_tier.

    Cheap-first is enforced PER candidate inside ``evaluate_candidate``;
    this adds the tally so a campaign can see exactly how many candidates
    each tier received, completed, and passed on.
    """
    results = [evaluate_candidate(c, tier=max_tier) for c in candidates]
    summary: dict[str, dict[str, Any]] = {}
    for t in range(0, max_tier + 1):
        key = str(t)
        entered = completed = survived = 0
        for r in results:
            block = r.get("tiers", {}).get(key)
            if not block or block.get("status") == "not_attempted":
                continue
            entered += 1
            if block.get("status") == "ok":
                completed += 1
                if block.get("gate", {}).get("passed"):
                    survived += 1
        summary[key] = {
            "tier": TIER_NAMES[t],
            "entered": entered,
            "completed": completed,
            "survived": survived,
        }
        contributing = [
            result["tiers"][key]["evidence_class"]
            for result in results
            if key in result.get("tiers", {})
        ]
        stamp_evidence(
            summary[key],
            EvidenceSource.CITED_COMPUTATION,
            contributing,
        )
    output = {
        "requested_max_tier": max_tier,
        "results": results,
        "summary": summary,
    }
    roll_up_evidence(output, results)
    return output


# ---------------------------------------------------------------------------
# Tool surface
# ---------------------------------------------------------------------------

_EVAL_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Evaluate a candidate material on the tiered fidelity ladder. "
        "Tier 0 = empirical Yang/Guo-Liu screen (default, milliseconds); "
        "tier 1 = MACE foundation-potential relaxation; tier 2 = CALPHAD "
        "phase equilibria; tier 3 = Quantum ESPRESSO pw.x. Tiers run "
        "cheap-first in order and stop escalating when a gate fails or a "
        "tier is unavailable. Every property block names the tier and "
        "method that produced it."
    ),
    "properties": {
        "composition": {
            "type": "string",
            "description": (
                "Atomic-fraction composition with explicit finite, positive "
                "fractions summing to 1.0 ± 1e-6, e.g. 'W0.5Ta0.3Mo0.2'. "
                "Ratios and percentages are rejected, never normalized."
            ),
        },
        "fractions": {
            "type": "object",
            "description": (
                "Alternative element→fraction dict; values must be finite, "
                "positive, and sum to 1.0 ± 1e-6."
            ),
        },
        "evidence_class": {
            "type": "string",
            "enum": [item.value for item in EvidenceClass],
            "default": EvidenceClass.INDETERMINATE.value,
            "description": (
                "RHEA-aligned class of the candidate/boundary-condition "
                "inputs. No tier result can outrank this class."
            ),
        },
        "tier": {
            "type": "integer",
            "minimum": 0,
            "maximum": 3,
            "description": "Requested fidelity (0-3). Default 0. Higher tiers are opt-in.",
        },
        "phase": {
            "type": "string",
            "description": "Structure prototype for tier 1 (bcc|fcc|hcp|c14_laves). Default bcc.",
        },
        "n_atoms": {
            "type": "integer",
            "description": "Tier-1 supercell size in atoms (default 100; the "
                           "MACE local backend requires compositions summing "
                           "to exactly 100).",
        },
        "temperature_K": {
            "type": "number",
            "description": "Tier-2 equilibrium temperature. Default 1300 K.",
        },
        "calphad_database": {
            "type": "string",
            "description": "TDB name in ~/.prism/databases. Default: first database covering the elements.",
        },
        "qe_pseudo_dir": {
            "type": "string",
            "description": "Directory of UPF pseudopotentials for tier 3. Default ~/.prism/pseudos.",
        },
        "qe_calculation": {
            "type": "string",
            "description": "pw.x calculation type (scf|relax|vc-relax). Default vc-relax.",
        },
    },
    "additionalProperties": False,
}


def _evaluate_candidate_tool(**kwargs: Any) -> dict:
    tier = kwargs.pop("tier", 0)
    try:
        tier = int(tier)
    except (TypeError, ValueError):
        return _error_result(f"tier must be an integer, got {tier!r}")
    return evaluate_candidate(kwargs, tier=tier)


def _tier_status_tool(**kwargs: Any) -> dict:
    result = {"tiers": tier_status()}
    roll_up_evidence(result, result["tiers"].values())
    return result


def create_evaluation_tools(registry: ToolRegistry) -> None:
    """Register the ladder tools. Approval-gated because tiers >= 1 can run
    heavy local compute (and the MACE platform backend spends credits) —
    same reasoning the mace_* tools carry."""
    registry.register(Tool(
        name="evaluate_candidate",
        description=(
            "Evaluate a candidate material at a requested fidelity tier on "
            "the tiered ladder: 0 empirical screen (Yang/Guo-Liu), 1 MACE "
            "MLIP relaxation, 2 CALPHAD phase equilibria, 3 Quantum "
            "ESPRESSO pw.x. Cheap-first escalation; every property carries "
            "the tier + method provenance that produced it; unavailable "
            "tiers report honestly with no fabricated numbers."
        ),
        input_schema=_EVAL_SCHEMA,
        func=_evaluate_candidate_tool,
        requires_approval=True,
        source="builtin",
        source_detail="evaluation",
        examples=[
            {
                "input": {"composition": "W0.5Ta0.3Mo0.2", "tier": 0},
                "output_note": "empirical screen only: Ω, δ, VEC + solid-solution verdict",
            },
            {
                "input": {"composition": "W0.5Ta0.3Mo0.2", "tier": 1, "phase": "bcc", "n_atoms": 100},
                "output_note": "screen + MACE-MP-0 relaxed energy/atom (if the [mace] extra is installed)",
            },
        ],
    ))
    registry.register(Tool(
        name="evaluation_tier_status",
        description=(
            "Report which evaluation tiers are available on this machine "
            "(dependencies present? pw.x installed? covering TDB?) with "
            "install hints for the missing ones."
        ),
        input_schema={"type": "object", "properties": {}, "additionalProperties": False},
        func=_tier_status_tool,
        requires_approval=False,
        source="builtin",
        source_detail="evaluation",
    ))
    logger.info("Registered evaluate_candidate + evaluation_tier_status tools")
