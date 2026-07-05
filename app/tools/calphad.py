"""CALPHAD tools — thermodynamic database management and calculations.

All tools follow the same pattern as simulation.py:
  - _guard() + private _func(**kwargs) -> dict
  - Registration via create_calphad_tools(registry).
"""
from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _guard():
    """Return an error dict if pycalphad is unavailable, else None."""
    from app.tools.simulation.calphad_bridge import check_calphad_available, _calphad_missing_error
    if not check_calphad_available():
        return _calphad_missing_error()
    return None


# ===========================================================================
# Calculation Tools (guarded — require pycalphad)
# ===========================================================================

def _calculate_phase_diagram(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.calphad_bridge import get_calphad_bridge
        bridge = get_calphad_bridge()

        database_name = kwargs["database_name"]
        components = kwargs["components"]
        phases = kwargs.get("phases")
        temperature_range = kwargs.get("temperature_range", [300, 2000, 50])
        pressure = kwargs.get("pressure", 101325)

        return bridge.calculate_phase_diagram(
            database_name=database_name,
            components=components,
            phases=phases,
            temperature_range=temperature_range,
            pressure=pressure,
        )
    except Exception as e:
        return {"error": str(e)}


def _calculate_equilibrium(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.calphad_bridge import get_calphad_bridge
        bridge = get_calphad_bridge()

        database_name = kwargs["database_name"]
        components = kwargs["components"]
        phases = kwargs.get("phases")
        conditions = kwargs["conditions"]

        return bridge.calculate_equilibrium(
            database_name=database_name,
            components=components,
            phases=phases,
            conditions=conditions,
        )
    except Exception as e:
        return {"error": str(e)}


def _calculate_gibbs_energy(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.calphad_bridge import get_calphad_bridge
        bridge = get_calphad_bridge()

        database_name = kwargs["database_name"]
        components = kwargs["components"]
        phases = kwargs["phases"]
        temperature = kwargs["temperature"]
        pressure = kwargs.get("pressure", 101325)

        return bridge.calculate_gibbs_energy(
            database_name=database_name,
            components=components,
            phases=phases,
            temperature=temperature,
            pressure=pressure,
        )
    except Exception as e:
        return {"error": str(e)}


# ===========================================================================
# Database Management Tools (NO guard — filesystem ops)
# ===========================================================================

def _list_databases(**kwargs) -> dict:
    """List available TDB files. No pycalphad needed."""
    try:
        from app.tools.simulation.calphad_bridge import get_calphad_bridge
        bridge = get_calphad_bridge()
        databases = bridge.databases.list_databases()
        return {"databases": databases, "count": len(databases)}
    except Exception as e:
        return {"error": str(e)}


def _list_phases(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.calphad_bridge import get_calphad_bridge
        bridge = get_calphad_bridge()

        database_name = kwargs["database_name"]
        components = kwargs.get("components")

        phases = bridge.databases.get_phases(database_name, components)
        if phases is None:
            return {"error": f"Database '{database_name}' not found"}

        return {"database": database_name, "phases": phases, "count": len(phases)}
    except Exception as e:
        return {"error": str(e)}


def _import_database(**kwargs) -> dict:
    """Import a user TDB file. No pycalphad needed."""
    try:
        from app.tools.simulation.calphad_bridge import get_calphad_bridge
        bridge = get_calphad_bridge()

        source_path = kwargs["source_path"]
        name = kwargs.get("name")

        return bridge.databases.import_database(source_path, name)
    except Exception as e:
        return {"error": str(e)}


# ===========================================================================
# Registration
# ===========================================================================

# ---------------------------------------------------------------------------
# Round 5 unified dispatchers
# ---------------------------------------------------------------------------

def _calphad(**kwargs) -> dict:
    """Read-only CALPHAD dispatcher: catalog + import. No approval gate.

    Replaces list_calphad_databases / list_phases / import_calphad_database.
    Compute actions (phase_diagram / equilibrium / gibbs) live in the
    separate `calphad_compute` tool which is approval-gated.
    """
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: list_databases, list_phases, import",
            "hint": (
                "calphad(action='list_databases') / "
                "calphad(action='list_phases', database_name='...') / "
                "calphad(action='import', source_path='...')"
            ),
        }
    if action == "list_databases":
        return _list_databases(**kwargs)
    if action == "list_phases":
        if not kwargs.get("database_name"):
            return {"error": "Action 'list_phases' requires `database_name`"}
        return _list_phases(**kwargs)
    if action == "import":
        if not kwargs.get("source_path"):
            return {"error": "Action 'import' requires `source_path`"}
        return _import_database(**kwargs)
    return {"error": f"Unknown action '{action}'. Valid: list_databases, list_phases, import"}


def _calphad_compute(**kwargs) -> dict:
    """Compute-heavy local CALPHAD dispatcher (pycalphad). Approval-gated —
    runs locally, no credits charged.

    Replaces calculate_phase_diagram / calculate_equilibrium /
    calculate_gibbs_energy. All three are real CALPHAD calculations
    that take seconds to minutes and produce structured results.
    """
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: phase_diagram, equilibrium, gibbs",
            "hint": (
                "calphad_compute(action='phase_diagram', database_name='...', components=[...]) / "
                "calphad_compute(action='equilibrium', database_name='...', components=[...], conditions={...}) / "
                "calphad_compute(action='gibbs', database_name='...', components=[...], phases=[...], temperature=...)"
            ),
        }

    if not kwargs.get("database_name"):
        return {"error": f"Action '{action}' requires `database_name`"}
    if not kwargs.get("components"):
        return {"error": f"Action '{action}' requires `components` (list)"}

    if action == "phase_diagram":
        return _calculate_phase_diagram(**kwargs)
    if action == "equilibrium":
        if not kwargs.get("conditions"):
            return {"error": "Action 'equilibrium' requires `conditions` dict"}
        return _calculate_equilibrium(**kwargs)
    if action == "gibbs":
        if not kwargs.get("phases"):
            return {"error": "Action 'gibbs' requires `phases` list"}
        if "temperature" not in kwargs:
            return {"error": "Action 'gibbs' requires `temperature`"}
        return _calculate_gibbs_energy(**kwargs)
    return {"error": f"Unknown action '{action}'. Valid: phase_diagram, equilibrium, gibbs"}


# ---------------------------------------------------------------------------
# Tool descriptions
# ---------------------------------------------------------------------------

_CALPHAD_DESCRIPTION = (
    "CALPHAD database catalog + IO operations (read-only / no compute). "
    "ONE tool, three actions:\n"
    "  • action='list_databases' — show available TDB thermodynamic database "
    "files in the PRISM-managed directory. No args.\n"
    "  • action='list_phases' — list phases available in a database, "
    "optionally filtered by components. Required: `database_name`. "
    "Optional: `components` to filter.\n"
    "  • action='import' — import a TDB database file into PRISM's managed "
    "directory. Required: `source_path`. Optional: `name` (default: file "
    "stem).\n"
    "For actual CALPHAD calculations (phase diagrams, equilibrium, Gibbs "
    "energy), use the separate `calphad_compute` tool — those are "
    "approval-gated because they spend compute budget."
)


_CALPHAD_COMPUTE_DESCRIPTION = (
    "CALPHAD thermodynamic calculations. ONE tool, three actions. "
    "COMPUTE-HEAVY (runs locally via pycalphad, no credits charged) — "
    "requires_approval=True; the harness will prompt before each call.\n"
    "  • action='phase_diagram' — calculate a binary/ternary phase diagram. "
    "Required: `database_name`, `components` (e.g. ['Al', 'Ni']). "
    "Optional: `phases`, `temperature_range` (default [300, 2000, 50]), "
    "`pressure` (default 101325 Pa).\n"
    "  • action='equilibrium' — calculate equilibrium at specific T/P/X. "
    "Required: `database_name`, `components`, `conditions` (dict like "
    "{T: 1000, P: 101325, 'X(AL)': 0.3}). Optional: `phases`.\n"
    "  • action='gibbs' — Gibbs energy surface for specified phases at a "
    "given temperature. Required: `database_name`, `components`, `phases`, "
    "`temperature`. Optional: `pressure`.\n"
    "Use `calphad(action='list_databases')` first to discover what's "
    "available, then `calphad(action='list_phases')` to see what phases "
    "the database covers."
)


def create_calphad_tools(registry: ToolRegistry) -> None:
    """Register the unified `calphad` (read-only) + `calphad_compute` tools.

    Round 5 collapses 6 → 2:
      list_calphad_databases + list_phases + import_calphad_database
        → calphad(action=…)
      calculate_phase_diagram + calculate_equilibrium +
        calculate_gibbs_energy → calphad_compute(action=…)

    The split mirrors compute / compute_submit and bash_task / stop_bash_task:
    destructive or compute-heavy actions stay isolated for per-tool
    approval gating.
    """
    registry.register(Tool(
        name="calphad",
        description=_CALPHAD_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list_databases", "list_phases", "import"],
                    "description": "Which CALPHAD catalog/IO operation.",
                },
                "database_name": {
                    "type": "string",
                    "description": "Database name for action='list_phases'.",
                },
                "components": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Component filter for action='list_phases'.",
                },
                "source_path": {
                    "type": "string",
                    "description": "Path to TDB file for action='import'.",
                },
                "name": {
                    "type": "string",
                    "description": "Name to register under for action='import' (default: file stem).",
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_calphad,
    ))

    registry.register(Tool(
        name="calphad_compute",
        description=_CALPHAD_COMPUTE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["phase_diagram", "equilibrium", "gibbs"],
                    "description": "Which CALPHAD calculation to run.",
                },
                "database_name": {
                    "type": "string",
                    "description": "TDB database name. Required for all actions.",
                },
                "components": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Chemical components, e.g. ['Al', 'Ni']. Required.",
                },
                "phases": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Phases to include. Required for action='gibbs'; optional for others (default: all in DB).",
                },
                "temperature_range": {
                    "type": "array",
                    "items": {"type": "number"},
                    "description": "T range [start, stop, step] in K for action='phase_diagram'. Default [300, 2000, 50].",
                },
                "temperature": {
                    "type": "number",
                    "description": "Temperature in K for action='gibbs'.",
                },
                "pressure": {
                    "type": "number",
                    "description": "Pressure in Pa. Default 101325.",
                },
                "conditions": {
                    "type": "object",
                    "description": "Equilibrium conditions for action='equilibrium', e.g. {T: 1000, P: 101325, 'X(AL)': 0.3}.",
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_calphad_compute,
        requires_approval=True,
    ))
