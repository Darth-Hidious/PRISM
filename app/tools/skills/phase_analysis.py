"""Phase analysis skill: analyze phase stability using CALPHAD."""

from app.tools.skills.base import Skill, SkillStep


def _analyze_phases(**kwargs) -> dict:
    """Analyze phase stability: load TDB, compute equilibrium, summarize."""
    from app.tools.simulation.calphad_bridge import (
        _calphad_missing_error,
        check_calphad_available,
    )

    if not check_calphad_available():
        return _calphad_missing_error()

    database_name = kwargs["database_name"]
    components = kwargs["components"]
    temperature = kwargs.get("temperature", 1000)
    pressure = kwargs.get("pressure", 101325)
    composition = kwargs.get("composition")

    from app.tools.simulation.calphad_bridge import get_calphad_bridge

    bridge = get_calphad_bridge()
    results = {}

    # Step 1: Load database and list phases
    phases = bridge.databases.get_phases(database_name, components)
    if phases is None:
        return {"error": f"Database '{database_name}' not found"}
    results["available_phases"] = phases

    # Step 2: Calculate equilibrium at given conditions
    conditions = {"T": temperature, "P": pressure}
    if composition:
        conditions.update(composition)

    try:
        eq_result = bridge.calculate_equilibrium(
            database_name=database_name,
            components=components,
            phases=None,
            conditions=conditions,
        )
        results["equilibrium"] = eq_result
    except Exception as e:
        results["equilibrium"] = {"error": str(e)}

    # Step 3: Calculate phase diagram across temperature range (optional)
    try:
        diagram = bridge.calculate_phase_diagram(
            database_name=database_name,
            components=components,
            phases=None,
            temperature_range=[300, 2000, 100],
            pressure=pressure,
        )
        results["phase_diagram"] = {
            "n_points": diagram.get("n_points", 0),
            # A scan that quietly dropped half its temperatures used to look
            # identical to one that converged everywhere.
            "n_failed_points": diagram.get("n_failed_points"),
            "temperature_range": [300, 2000, 100],
            "provenance": diagram.get("provenance"),
        }
    except Exception as e:
        results["phase_diagram"] = {"error": str(e)}

    # Build summary
    summary = {
        "database": database_name,
        "components": components,
        "conditions": conditions,
        "available_phases": len(phases),
        "results": results,
        # Lift the equilibrium bundle to the top so the summary itself is
        # traceable, not only the nested step it came from.
        "provenance": (results.get("equilibrium") or {}).get("provenance"),
    }

    # Add stable phases from equilibrium if available
    eq = results.get("equilibrium", {})
    if "phases_present" in eq:
        summary["stable_phases"] = eq["phases_present"]
        summary["phase_fractions"] = eq.get("phase_fractions", {})

    return summary


PHASE_ANALYSIS_SKILL = Skill(
    name="analyze_phases",
    description=(
        "Analyze phase stability using CALPHAD: load a thermodynamic database, "
        "calculate the phase diagram, identify stable phases at given conditions, "
        "and generate a summary. Requires pycalphad."
    ),
    steps=[
        SkillStep("load_database", "Load thermodynamic database", "list_calphad_databases"),
        SkillStep("calculate_diagram", "Calculate phase diagram", "calculate_phase_diagram"),
        SkillStep("find_stable", "Identify stable phases at conditions", "calculate_equilibrium"),
        SkillStep("summarize", "Generate phase analysis summary", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "database_name": {
                "type": "string",
                "description": "Name of the TDB database to use",
            },
            "components": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Chemical components to analyze, e.g. ['W', 'Rh']",
            },
            "temperature": {
                "type": "number",
                "description": "Temperature in K for equilibrium (default: 1000)",
            },
            "pressure": {
                "type": "number",
                "description": "Pressure in Pa (default: 101325)",
            },
            "composition": {
                "type": "object",
                "description": "Composition conditions, e.g. {\"X(W)\": 0.5}",
            },
        },
        "required": ["database_name", "components"],
        "additionalProperties": False,
    },
    func=_analyze_phases,
    category="thermodynamics",
    requires_approval=True,
)
