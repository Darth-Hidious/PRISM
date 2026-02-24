"""Simulation planning skill: generate job plans for top candidates.

Supports smart routing between CALPHAD, DFT, and MD methods.
"""

from app.config.preferences import UserPreferences
from app.skills.base import Skill, SkillStep


# Simulation type → method routing tables
_CALPHAD_TYPES = {"phase_diagram", "equilibrium", "gibbs_energy", "phase_stability"}
_DFT_TYPES = {"energy_minimization", "static", "elastic_constants", "phonons", "equation_of_state"}
_MD_TYPES = {"md", "thermal_expansion"}


def _resolve_method(sim_type: str, method: str) -> str:
    """Resolve the simulation method for a given type."""
    if method != "auto":
        return method
    if sim_type in _CALPHAD_TYPES:
        return "calphad"
    if sim_type in _MD_TYPES:
        return "md"
    return "dft"


def _plan_simulations(**kwargs) -> dict:
    """Read top candidates and generate a simulation job plan (no execution)."""
    dataset_name = kwargs["dataset_name"]
    compute_budget = kwargs.get("compute_budget")
    simulation_types = kwargs.get("simulation_types", ["energy_minimization"])
    code = kwargs.get("code", "lammps")
    method = kwargs.get("method", "auto")
    database_name = kwargs.get("database_name")
    max_jobs = kwargs.get("max_jobs", 10)

    prefs = UserPreferences.load()
    compute_budget = compute_budget or prefs.compute_budget

    from app.data.store import DataStore

    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found"}

    # Determine formula column
    formula_col = None
    for cand in ("formula", "formula_pretty"):
        if cand in df.columns:
            formula_col = cand
            break
    if not formula_col:
        return {"error": "No formula column found in dataset"}

    candidates = df.head(max_jobs)

    jobs = []
    for i, (_, row) in enumerate(candidates.iterrows()):
        formula = str(row[formula_col])
        for sim_type in simulation_types:
            resolved = _resolve_method(sim_type, method)

            job = {
                "job_id": f"plan_{i}_{sim_type}",
                "formula": formula,
                "simulation_type": sim_type,
                "method": resolved,
                "status": "planned",
            }

            if resolved == "calphad":
                if database_name:
                    job["database_name"] = database_name
            else:
                job["code"] = code

            if compute_budget == "hpc" and resolved != "calphad":
                job["hpc"] = {
                    "queue": prefs.hpc_queue,
                    "cores": prefs.hpc_cores,
                }

            jobs.append(job)

    return {
        "dataset_name": dataset_name,
        "planned_jobs": len(jobs),
        "compute_budget": compute_budget,
        "jobs": jobs,
        "note": (
            "These are planned jobs only. "
            "Execute CALPHAD jobs with calculate_equilibrium/calculate_phase_diagram, "
            "and DFT/MD jobs with run_simulation after review."
        ),
    }


SIM_PLAN_SKILL = Skill(
    name="plan_simulations",
    description=(
        "Generate a simulation job plan for top candidates in a dataset. "
        "Supports smart routing: CALPHAD for phase stability/equilibrium, "
        "DFT for energy/elastic/phonon calculations, MD for dynamics. "
        "Plans only — does NOT execute simulations (expensive, needs user "
        "confirmation). Applies HPC settings from preferences."
    ),
    steps=[
        SkillStep("load_candidates", "Load dataset candidates", "internal"),
        SkillStep("route_methods", "Route simulation types to CALPHAD/DFT/MD", "internal"),
        SkillStep("plan_jobs", "Generate job plans per candidate", "internal"),
        SkillStep("apply_hpc", "Apply HPC settings if configured", "internal", optional=True),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "dataset_name": {
                "type": "string",
                "description": "Name of the dataset with candidate materials",
            },
            "compute_budget": {
                "type": "string",
                "description": "Compute budget: local or hpc (default from preferences)",
            },
            "simulation_types": {
                "type": "array",
                "items": {"type": "string"},
                "description": (
                    "Simulation types to plan. "
                    "CALPHAD: phase_diagram, equilibrium, gibbs_energy, phase_stability. "
                    "DFT: energy_minimization, static, elastic_constants, phonons, equation_of_state. "
                    "MD: md, thermal_expansion. Default: energy_minimization"
                ),
            },
            "method": {
                "type": "string",
                "description": "Method: calphad, dft, md, or auto (default: auto — routes by simulation_type)",
            },
            "code": {
                "type": "string",
                "description": "Simulation code for DFT/MD jobs (default: lammps)",
            },
            "database_name": {
                "type": "string",
                "description": "TDB database name for CALPHAD jobs",
            },
            "max_jobs": {
                "type": "integer",
                "description": "Maximum number of jobs to plan (default: 10)",
            },
        },
        "required": ["dataset_name"],
    },
    func=_plan_simulations,
    category="simulation",
    requires_approval=True,
)
