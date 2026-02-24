"""Simulation planning skill: generate job plans for top candidates."""

from app.config.preferences import UserPreferences
from app.skills.base import Skill, SkillStep


def _plan_simulations(**kwargs) -> dict:
    """Read top candidates and generate a simulation job plan (no execution)."""
    dataset_name = kwargs["dataset_name"]
    compute_budget = kwargs.get("compute_budget")
    simulation_types = kwargs.get("simulation_types", ["energy_minimization"])
    code = kwargs.get("code", "lammps")
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
            job = {
                "job_id": f"plan_{i}_{sim_type}",
                "formula": formula,
                "simulation_type": sim_type,
                "code": code,
                "status": "planned",
            }

            if compute_budget == "hpc":
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
        "note": "These are planned jobs only. Execute with run_simulation after review.",
    }


SIM_PLAN_SKILL = Skill(
    name="plan_simulations",
    description=(
        "Generate a simulation job plan for top candidates in a dataset. "
        "Plans only — does NOT execute simulations (expensive, needs user "
        "confirmation). Applies HPC settings from preferences."
    ),
    steps=[
        SkillStep("load_candidates", "Load dataset candidates", "internal"),
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
                "description": "Simulation types to plan (default: energy_minimization)",
            },
            "code": {
                "type": "string",
                "description": "Simulation code to use (default: lammps)",
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
)
