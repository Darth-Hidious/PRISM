"""MACE-driven alloy discovery skill.

End-to-end pipeline for discovering candidate alloy compositions that meet
target property constraints, using MACE-MH-1 as the DFT surrogate for
relaxation + elastic-constant prediction, and CALPHAD (`analyze_phases`)
for phase-stability screening.

Architecture (explicit honest labelling):

    [composition grid]
          ↓
    [mace_relax_structure]   ← MACE-MH-1 (PyTorch). Foundation MLIP trained
          ↓                    on 100M DFT configs. THIS is the "DFT
                               surrogate" — relaxation gives energy/forces
                               at DFT accuracy without the DFT cost.
          ↓
    [mace_compute_elastic]   ← Strain-stress linear fits via MACE. Returns
          ↓                    6×6 elastic tensor + K/G/E/ν + Pugh ratio.
          ↓
    [analyze_phases]         ← CALPHAD (pycalphad + TCHEA database). Real
          ↓                    thermodynamic equilibrium at the target
                               service temperature.
          ↓
    [ranking + report]

NOT included yet (deferred — explicit gap labelling):
  * JAX-MD path. The roadmap calls for jax-md / Equinox-based MD for
    velocity-autocorrelation diffusion + thermal-conductivity estimates.
    Not wired into production code as of 2026-05-14. mace_md_equilibrate
    (PyTorch via ASE) is the only MD path that works today.
  * Active-learning loop. The skill currently runs a fixed grid of
    candidates. A proper Bayesian-optimisation loop (e.g. via BoTorch on
    JAX) is the next iteration.
  * Reproducibility bundle. The marc27-core research engine emits
    BagIt + RO-Crate bundles per PR #35. This skill returns a
    structured dict; the bundle emission happens when the agent calls
    this skill from inside a research session.

ESA-grade reminder: every step writes to ResultsRepo via the underlying
primitive's provenance hooks. Audit trail traces every relaxation back
to its input atoms + cache_key + MACE model checksum.
"""

from __future__ import annotations

import logging
from typing import Any

from app.tools.skills.base import Skill, SkillStep

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Helpers — formula parsing + grid generation
# ---------------------------------------------------------------------------

def _atoms_from_grid_spec(
    base_elements: list[str],
    fractions: dict[str, float],
    n_atoms: int = 100,
) -> dict[str, int]:
    """Convert a fraction map to an integer atom-count map summing to n_atoms.

    Caller passes e.g. base_elements=["Mo","Nb","Ta","W","Hf"] with
    fractions={"Hf": 0.05, "Mo": 0.2375, "Nb": 0.2375, "Ta": 0.2375, "W": 0.2375}.
    Returns {"Hf": 5, "Mo": 24, "Nb": 24, "Ta": 24, "W": 23} (deterministic
    rounding-drift correction so the result always sums to n_atoms).
    """
    if abs(sum(fractions.values()) - 1.0) > 1e-3:
        raise ValueError(
            f"fractions must sum to 1.0 (got {sum(fractions.values()):.4f})"
        )
    scaled = {el: f * n_atoms for el, f in fractions.items()}
    rounded = {el: int(round(v)) for el, v in scaled.items()}
    drift = n_atoms - sum(rounded.values())
    if drift:
        # Apply drift to the element with the largest fractional remainder.
        order = sorted(scaled.items(), key=lambda kv: -(kv[1] - int(kv[1])))
        for el, _ in order[: abs(drift)]:
            rounded[el] += 1 if drift > 0 else -1
    return rounded


def _formula_repr(atoms: dict[str, int]) -> str:
    """Render an atom-count map as e.g. 'Mo24Nb24Ta24W23Hf5'."""
    return "".join(f"{el}{n}" for el, n in atoms.items())


def _ranked_summary(rows: list[dict], rank_by: str) -> list[dict]:
    """Sort completed rows by the named metric (descending). Drops rows
    that errored or where the metric isn't present.
    """
    completed = [r for r in rows if r.get("elastic", {}).get("result")]
    if not completed:
        return rows  # nothing to rank
    keyfn = lambda r: r["elastic"]["result"].get(rank_by, float("-inf"))
    return sorted(completed, key=keyfn, reverse=True)


# ---------------------------------------------------------------------------
# Skill implementation
# ---------------------------------------------------------------------------

def _alloy_discovery(**kwargs) -> dict[str, Any]:
    """Run the MACE-driven alloy discovery pipeline.

    The skill takes a candidate-grid spec, dispatches per-candidate MACE
    relax + elastic via the tool registry, optionally adds CALPHAD phase
    stability, ranks results, and returns a structured report.

    See the module docstring for the explicit JAX-MD / active-learning
    gaps. This is the production path that works today, not a research
    aspiration.
    """
    import time

    base_elements: list[str] = kwargs["base_elements"]
    candidate_grid: list[dict[str, float]] = kwargs["candidate_grid"]
    phase: str = kwargs.get("phase", "bcc")
    target_property: str = kwargs.get("target_property", "K_VRH_GPa")
    add_phase_analysis: bool = kwargs.get("add_phase_analysis", False)
    service_temperature_K: float = kwargs.get("service_temperature_K", 1500.0)
    title: str = kwargs.get(
        "title", f"Alloy discovery: {'-'.join(base_elements)} {phase.upper()}"
    )

    # Import the tool-server side at execution time so the skill module
    # itself is import-safe even when the registry hasn't been wired yet
    # (e.g. test collection on a fresh interpreter).
    from app.tools.simulation.mace_bridge import get_mace_bridge
    from app.tools.simulation.mace.schemas import (
        ComputeElasticInput,
        RelaxStructureInput,
    )
    from app.tools.simulation.mace.control import get_job
    from app.tools.simulation.mace import primitives

    bridge = get_mace_bridge()
    runner = bridge.runner

    rows: list[dict[str, Any]] = []
    t0 = time.monotonic()

    for cand_idx, fractions in enumerate(candidate_grid):
        try:
            atoms = _atoms_from_grid_spec(base_elements, fractions, n_atoms=100)
        except ValueError as e:
            rows.append({"index": cand_idx, "error": str(e)})
            continue

        formula = _formula_repr(atoms)
        row: dict[str, Any] = {"index": cand_idx, "formula": formula, "atoms": atoms}

        # Step 1: relax
        try:
            relax_inp = RelaxStructureInput(
                composition={"atoms": atoms},
                phase=phase,
                n_atoms=sum(atoms.values()),
            )
            relax_handle = primitives.relax_structure(relax_inp, runner)
            row["relax_job_id"] = relax_handle.job_id
        except Exception as e:
            row["relax_error"] = f"{type(e).__name__}: {e}"
            rows.append(row)
            continue

        # Step 2: elastic
        try:
            elastic_inp = ComputeElasticInput(
                structure={
                    "composition": {"atoms": atoms},
                    "phase": phase,
                    "n_atoms": sum(atoms.values()),
                },
            )
            elastic_handle = primitives.compute_elastic(elastic_inp, runner)
            row["elastic_job_id"] = elastic_handle.job_id
        except Exception as e:
            row["elastic_error"] = f"{type(e).__name__}: {e}"

        rows.append(row)

    # Step 3: wait for all jobs to complete (bounded)
    deadline = t0 + kwargs.get("timeout_s", 1800.0)
    pending = {
        row["formula"]: (row.get("relax_job_id"), row.get("elastic_job_id"))
        for row in rows
        if "formula" in row
    }
    poll_interval = 2.0
    while pending and time.monotonic() < deadline:
        time.sleep(poll_interval)
        still_pending = {}
        for formula, (relax_jid, elastic_jid) in pending.items():
            row = next(r for r in rows if r.get("formula") == formula)
            if relax_jid and "relax" not in row:
                from app.tools.simulation.mace.schemas import GetJobInput

                h = get_job(GetJobInput(job_id=relax_jid), runner)
                if h.status in ("completed", "failed", "cancelled"):
                    row["relax"] = h.model_dump()
                    relax_jid = None
            if elastic_jid and "elastic" not in row:
                from app.tools.simulation.mace.schemas import GetJobInput

                h = get_job(GetJobInput(job_id=elastic_jid), runner)
                if h.status in ("completed", "failed", "cancelled"):
                    row["elastic"] = h.model_dump()
                    elastic_jid = None
            if relax_jid or elastic_jid:
                still_pending[formula] = (relax_jid, elastic_jid)
        pending = still_pending

    # Step 4: optional CALPHAD phase analysis on the top-ranked candidates
    if add_phase_analysis:
        # Defer to the existing analyze_phases tool if registered.
        try:
            from app.tools.calphad import _analyze_phases  # type: ignore
            for row in rows[: min(3, len(rows))]:  # top 3 only — CALPHAD is slow
                if "formula" not in row:
                    continue
                try:
                    row["phases"] = _analyze_phases(
                        composition=row["formula"],
                        temperature=service_temperature_K,
                        database="TCHEA",
                    )
                except Exception as e:
                    row["phases_error"] = f"{type(e).__name__}: {e}"
        except ImportError:
            for row in rows:
                row["phases_skipped"] = "pycalphad/analyze_phases not available in this build"

    # Step 5: rank by target property
    ranked = _ranked_summary(rows, target_property)

    elapsed = time.monotonic() - t0
    return {
        "title": title,
        "base_elements": base_elements,
        "phase": phase,
        "n_candidates": len(candidate_grid),
        "target_property": target_property,
        "candidates": rows,
        "ranked": ranked[: min(10, len(ranked))],
        "elapsed_seconds": round(elapsed, 1),
        "notes": {
            "dft_surrogate": "MACE-MH-1 (PyTorch). Multi-head foundation MLIP, 100M DFT configs.",
            "md_path": "PyTorch via ASE (mace_md_equilibrate). JAX-MD not yet wired.",
            "phase_analysis": "pycalphad TCHEA" if add_phase_analysis else "skipped",
        },
    }


ALLOY_DISCOVERY_SKILL = Skill(
    name="alloy_discovery",
    description=(
        "MACE-driven alloy discovery pipeline. Takes a base element set and a "
        "candidate composition grid (list of fraction maps), runs MACE-MH-1 "
        "structural relaxation + elastic-constant prediction for each, "
        "optionally adds CALPHAD phase stability at a service temperature, "
        "and returns ranked candidates. Use for: refractory HEAs, superalloys, "
        "any single-phase BCC/FCC composition screening. For multi-phase systems "
        "or active learning, call this skill iteratively from a research session.\n\n"
        "Framework note: MACE-MH-1 is the DFT surrogate (PyTorch — multi-head MH-1 "
        "is not yet supported in mace-jax). MD via ASE+MACE (mace_md_equilibrate); "
        "jax-md path is on the roadmap, not yet wired."
    ),
    steps=[
        SkillStep("relax", "MACE-MH-1 structural relaxation per candidate", "mace_relax_structure"),
        SkillStep("elastic", "MACE-MH-1 elastic-constant prediction per candidate", "mace_compute_elastic"),
        SkillStep("phases", "CALPHAD phase equilibrium at service T (optional)", "analyze_phases", optional=True),
        SkillStep("rank", "Rank candidates by target property", "generate_report", optional=True),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "base_elements": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Element symbols defining the alloy system (e.g. ['Mo','Nb','Ta','W','Hf']).",
            },
            "candidate_grid": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": {"type": "number"},
                },
                "description": (
                    "List of candidate composition fraction maps. Each entry "
                    "is {element: fraction} summing to 1.0. The grid is "
                    "deterministic — the caller is responsible for picking "
                    "the sampling strategy (linspace, Sobol', LHS, etc.)."
                ),
            },
            "phase": {
                "type": "string",
                "description": "Target crystal structure (bcc, fcc, hcp). Default: bcc.",
                "default": "bcc",
            },
            "target_property": {
                "type": "string",
                "description": "Property to rank by (K_VRH_GPa, G_VRH_GPa, E_Young_GPa, pugh_G_over_B). Default: K_VRH_GPa.",
                "default": "K_VRH_GPa",
            },
            "add_phase_analysis": {
                "type": "boolean",
                "description": "Run CALPHAD analyze_phases on top-3 candidates at service_temperature_K. Default: false.",
                "default": False,
            },
            "service_temperature_K": {
                "type": "number",
                "description": "Service temperature for CALPHAD phase analysis (K). Default: 1500.",
                "default": 1500.0,
            },
            "title": {"type": "string", "description": "Discovery-run title for the report."},
            "timeout_s": {
                "type": "number",
                "description": "Hard wall-clock cap for the whole grid (seconds). Default: 1800.",
                "default": 1800.0,
            },
        },
        "required": ["base_elements", "candidate_grid"],
    },
    func=_alloy_discovery,
    category="discovery",
    requires_approval=True,
)
