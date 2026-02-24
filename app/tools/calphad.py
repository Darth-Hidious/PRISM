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
    from app.simulation.calphad_bridge import check_calphad_available, _calphad_missing_error
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
        from app.simulation.calphad_bridge import get_calphad_bridge
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
        from app.simulation.calphad_bridge import get_calphad_bridge
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
        from app.simulation.calphad_bridge import get_calphad_bridge
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
        from app.simulation.calphad_bridge import get_calphad_bridge
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
        from app.simulation.calphad_bridge import get_calphad_bridge
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
        from app.simulation.calphad_bridge import get_calphad_bridge
        bridge = get_calphad_bridge()

        source_path = kwargs["source_path"]
        name = kwargs.get("name")

        return bridge.databases.import_database(source_path, name)
    except Exception as e:
        return {"error": str(e)}


# ===========================================================================
# Registration
# ===========================================================================

def create_calphad_tools(registry: ToolRegistry) -> None:
    """Register all CALPHAD tools."""

    registry.register(Tool(
        name="calculate_phase_diagram",
        description="Calculate a binary/ternary phase diagram from a TDB thermodynamic database.",
        input_schema={
            "type": "object",
            "properties": {
                "database_name": {"type": "string", "description": "Name of the TDB database (without .tdb extension)"},
                "components": {"type": "array", "items": {"type": "string"}, "description": "Chemical components, e.g. ['Al', 'Ni']"},
                "phases": {"type": "array", "items": {"type": "string"}, "description": "Phases to include (default: all in database)"},
                "temperature_range": {"type": "array", "items": {"type": "number"}, "description": "Temperature range as [start, stop, step] in K. Default: [300, 2000, 50]"},
                "pressure": {"type": "number", "description": "Pressure in Pa. Default: 101325"},
            },
            "required": ["database_name", "components"],
        },
        func=_calculate_phase_diagram,
        requires_approval=True,
    ))

    registry.register(Tool(
        name="calculate_equilibrium",
        description="Calculate thermodynamic equilibrium at specific conditions (T, P, composition).",
        input_schema={
            "type": "object",
            "properties": {
                "database_name": {"type": "string", "description": "Name of the TDB database"},
                "components": {"type": "array", "items": {"type": "string"}, "description": "Chemical components, e.g. ['Al', 'Ni']"},
                "phases": {"type": "array", "items": {"type": "string"}, "description": "Phases to include (default: all)"},
                "conditions": {"type": "object", "description": "Equilibrium conditions, e.g. {\"T\": 1000, \"P\": 101325, \"X(AL)\": 0.3}"},
            },
            "required": ["database_name", "components", "conditions"],
        },
        func=_calculate_equilibrium,
        requires_approval=True,
    ))

    registry.register(Tool(
        name="calculate_gibbs_energy",
        description="Calculate Gibbs energy surface for specified phases at given temperature.",
        input_schema={
            "type": "object",
            "properties": {
                "database_name": {"type": "string", "description": "Name of the TDB database"},
                "components": {"type": "array", "items": {"type": "string"}, "description": "Chemical components"},
                "phases": {"type": "array", "items": {"type": "string"}, "description": "Phases to calculate Gibbs energy for"},
                "temperature": {"type": "number", "description": "Temperature in K"},
                "pressure": {"type": "number", "description": "Pressure in Pa. Default: 101325"},
            },
            "required": ["database_name", "components", "phases", "temperature"],
        },
        func=_calculate_gibbs_energy,
    ))

    registry.register(Tool(
        name="list_calphad_databases",
        description="List available thermodynamic TDB database files.",
        input_schema={"type": "object", "properties": {}},
        func=_list_databases,
    ))

    registry.register(Tool(
        name="list_phases",
        description="List phases available in a thermodynamic database, optionally filtered by components.",
        input_schema={
            "type": "object",
            "properties": {
                "database_name": {"type": "string", "description": "Name of the TDB database"},
                "components": {"type": "array", "items": {"type": "string"}, "description": "Filter phases by components"},
            },
            "required": ["database_name"],
        },
        func=_list_phases,
    ))

    registry.register(Tool(
        name="import_calphad_database",
        description="Import a TDB thermodynamic database file into PRISM's managed directory.",
        input_schema={
            "type": "object",
            "properties": {
                "source_path": {"type": "string", "description": "Path to the TDB file to import"},
                "name": {"type": "string", "description": "Name for the database (default: filename without extension)"},
            },
            "required": ["source_path"],
        },
        func=_import_database,
    ))
