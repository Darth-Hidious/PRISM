"""ThermoCalc plugin skeleton — built-in example for the plugin system.

This is a CONNECTOR STUB, not a working ThermoCalc integration. It shows how a
plugin registers tools; the handlers do NOT compute anything. Real functionality
requires a commercial ThermoCalc license + the TC-Python SDK and a proper
implementation against it. The handlers return an explicit ``not_implemented``
error rather than fabricated numbers, so a user who happens to have TC-Python
installed is never handed echo-back data dressed up as a real calculation.
"""


def _check_tc_python() -> bool:
    """Return True if TC-Python is importable."""
    try:
        import tc_python  # noqa: F401
        return True
    except ImportError:
        return False


def _not_implemented(calculation: str) -> dict:
    """Honest response: the SDK may be present, but nothing was computed."""
    return {
        "status": "not_implemented",
        "error": (
            f"ThermoCalc {calculation} is a stub — TC-Python may be importable, "
            "but no calculation is wired. No result was computed. Implement this "
            "handler against the TC-Python SDK to enable the tool."
        ),
    }


def _tc_equilibrium(**kwargs) -> dict:
    """Stub — returns not_implemented (does not call TC-Python)."""
    return _not_implemented("equilibrium calculation")


def _tc_phase_diagram(**kwargs) -> dict:
    """Stub — returns not_implemented (does not call TC-Python)."""
    return _not_implemented("phase diagram calculation")


def register(registry):
    """Called by PRISM plugin loader."""
    if not _check_tc_python():
        return  # Skip registration if TC-Python not available

    from app.tools.base import Tool

    registry.tool_registry.register(Tool(
        name="tc_equilibrium",
        description=(
            "STUB (not implemented): ThermoCalc equilibrium connector example. "
            "Returns not_implemented — no calculation is performed. Requires a "
            "commercial license and a real TC-Python implementation."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "database": {"type": "string", "description": "ThermoCalc database name, e.g. TCFE12"},
                "components": {"type": "array", "items": {"type": "string"}, "description": "Chemical components"},
                "temperature": {"type": "number", "description": "Temperature in K"},
                "conditions": {"type": "object", "description": "Additional conditions"},
            },
            "required": ["components"],
        },
        func=_tc_equilibrium,
    ))

    registry.tool_registry.register(Tool(
        name="tc_phase_diagram",
        description=(
            "STUB (not implemented): ThermoCalc phase-diagram connector example. "
            "Returns not_implemented — no calculation is performed. Requires a "
            "commercial license and a real TC-Python implementation."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "database": {"type": "string", "description": "ThermoCalc database name"},
                "components": {"type": "array", "items": {"type": "string"}, "description": "Chemical components"},
                "temperature_range": {"type": "array", "items": {"type": "number"}, "description": "[start, stop, step] in K"},
            },
            "required": ["components"],
        },
        func=_tc_phase_diagram,
    ))
