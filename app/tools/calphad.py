"""CALPHAD tools — thermodynamic database management and calculations.

All tools follow the same pattern as simulation.py:
  - _guard() + private _func(**kwargs) -> dict
  - Registration via create_calphad_tools(registry).
"""
from app.tools.base import Tool, ToolRegistry

# Imported at module level so the delegation below (and tests) can see one
# authoritative answer to "can this interpreter run pycalphad".
from app.tools.simulation.calphad_bridge import check_calphad_available

import os


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
            database_path=kwargs.get("_database_path"),
            licensed_source=kwargs.get("_licensed_source"),
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
            database_path=kwargs.get("_database_path"),
            licensed_source=kwargs.get("_licensed_source"),
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
            database_path=kwargs.get("_database_path"),
            licensed_source=kwargs.get("_licensed_source"),
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

        result = bridge.databases.import_database(source_path, name)
        if result.get("imported"):
            result["coverage_validated"] = False
            result["install_hint"] = (
                "Declare this owned file's elements, validated systems, version, "
                "licence and evidence class in ~/.prism/licensed_sources.json "
                "before computing. Import alone does not establish coverage."
            )
        return result
    except Exception as e:
        return {"error": str(e)}


# ===========================================================================
# Registration
# ===========================================================================

# ---------------------------------------------------------------------------
# Round 5 unified dispatchers
# ---------------------------------------------------------------------------

def sidecar_available() -> bool:
    """Whether the science sidecar is provisioned with CALPHAD support."""
    try:
        from app.tools._sidecar import SIDECAR_VENV

        return (SIDECAR_VENV / ".provisioned").exists() or (
            SIDECAR_VENV / "bin" / "python3"
        ).exists()
    except Exception:
        return False


def sidecar_call(tool: str, args: dict) -> dict:
    from app.tools._sidecar import call_tool

    return call_tool(tool, args)


def _in_sidecar() -> bool:
    """True when this process IS the sidecar — it must never delegate to itself."""
    return os.environ.get("PRISM_IN_SIDECAR") == "1"


def _delegate(tool: str, kwargs: dict) -> "dict | None":
    """Run a CALPHAD action in the sidecar when this interpreter cannot.

    The main venv is Python 3.14; pycalphad pins a dependency with no 3.14
    wheel, which is precisely why the sidecar venv (3.12) exists and registers
    these same tools. Measured 2026-09-06: without this hop the tool answered
    "pycalphad is not installed" on a machine where it was installed and
    working. Returns None when the caller should run locally instead.
    """
    if check_calphad_available() or _in_sidecar():
        return None
    if not sidecar_available():
        return {
            "error": "pycalphad is not available in this interpreter and the science "
            "sidecar is not provisioned.",
            "remedy": "prism provision extra calphad (creates ~/.prism/venv-sci and installs pycalphad)",
        }
    out = sidecar_call(tool, kwargs)
    if isinstance(out, dict):
        out.setdefault("ran_in", "sidecar")
    return out


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
        remote = _delegate("calphad", {"action": "list_phases", **kwargs})
        if remote is not None:
            return remote
        return _list_phases(**kwargs)
    if action == "import":
        if not kwargs.get("source_path"):
            return {"error": "Action 'import' requires `source_path`"}
        return _import_database(**kwargs)
    return {"error": f"Unknown action '{action}'. Valid: list_databases, list_phases, import"}


def _calphad_compute(**kwargs) -> dict:
    """Resolve a coverage-validated TDB, then dispatch a real calculation."""
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: phase_diagram, equilibrium, gibbs",
            "hint": (
                "calphad_compute(action='phase_diagram', components=[...]) / "
                "calphad_compute(action='equilibrium', components=[...], conditions={...}) / "
                "calphad_compute(action='gibbs', components=[...], phases=[...], temperature=...)"
            ),
        }
    if action not in {"phase_diagram", "equilibrium", "gibbs"}:
        return {
            "error": f"Unknown action '{action}'. Valid: phase_diagram, equilibrium, gibbs"
        }
    if not kwargs.get("components"):
        return {"error": f"Action '{action}' requires `components` (list)"}
    if action == "equilibrium" and not kwargs.get("conditions"):
        return {"error": "Action 'equilibrium' requires `conditions` dict"}
    if action == "gibbs":
        if not kwargs.get("phases"):
            return {"error": "Action 'gibbs' requires `phases` list"}
        if "temperature" not in kwargs:
            return {"error": "Action 'gibbs' requires `temperature`"}

    # The engine gate comes BEFORE source resolution. Every _calculate_*
    # already calls _guard(), but resolution refuses first on any machine
    # without an entitled TDB, so on a pycalphad-less install the agent only
    # ever saw a licensing refusal whose install_hint says "configure/acquire
    # a TDB" — advice that costs money and still would not let the tool run.
    # Checking the local, free precondition first also avoids a platform
    # entitlement round-trip for a computation that cannot happen.
    err = _guard()
    if err:
        return err

    from app.tools.licensed_sources import (
        SourceRefusal,
        SourceRequest,
        get_licensed_source_resolver,
    )

    request = SourceRequest.thermodynamic_database(
        kwargs["components"],
        preferred_source=kwargs.get("database_name"),
    )
    resolved = get_licensed_source_resolver().resolve(request)
    if isinstance(resolved, SourceRefusal):
        return resolved.as_dict()
    if resolved.access_kind != "file" or resolved.path is None:
        return {
            "status": "refused",
            "error": (
                f"Entitled TDB '{resolved.name}' is not attached as a readable "
                "file for local pycalphad execution"
            ),
            "refusal": {
                "code": "licensed_source_access_unavailable",
                "source_type": resolved.source_type.value,
                "source": resolved.provenance(),
                "access_kind": resolved.access_kind,
            },
            "install_hint": (
                "Attach the entitled source through the platform as a file mount, "
                "or configure an already-owned local TDB file. No database was "
                "downloaded or substituted."
            ),
        }
    if not resolved.path.is_file():
        return {
            "status": "refused",
            "error": f"Resolved TDB file is unavailable: {resolved.path}",
            "refusal": {
                "code": "licensed_source_file_unavailable",
                "source_type": resolved.source_type.value,
                "source": resolved.provenance(),
            },
            "install_hint": (
                "Restore the configured file or platform mount. No database was "
                "downloaded or substituted."
            ),
        }

    kwargs["database_name"] = resolved.source_id
    kwargs["_database_path"] = resolved.path
    kwargs["_licensed_source"] = resolved
    if action == "phase_diagram":
        return _calculate_phase_diagram(**kwargs)
    if action == "equilibrium":
        return _calculate_equilibrium(**kwargs)
    return _calculate_gibbs_energy(**kwargs)


# ---------------------------------------------------------------------------
# Tool descriptions
# ---------------------------------------------------------------------------

_CALPHAD_DESCRIPTION = (
    "CALPHAD database catalog + IO operations (read-only / no compute). "
    "ONE tool, three actions:\n"
    "  • action='list_databases' — show raw TDB files in the PRISM-managed "
    "directory. Presence here does not establish validated coverage. No args.\n"
    "  • action='list_phases' — list phases available in a database, "
    "optionally filtered by components. Required: `database_name`. "
    "Optional: `components` to filter.\n"
    "  • action='import' — import a TDB database file into PRISM's managed "
    "directory. Required: `source_path`. Optional: `name` (default: file "
    "stem). Import does not assert coverage; configure source metadata before "
    "compute.\n"
    "For actual CALPHAD calculations (phase diagrams, equilibrium, Gibbs "
    "energy), use the separate `calphad_compute` tool — those are "
    "approval-gated because they spend compute budget."
)


_CALPHAD_COMPUTE_DESCRIPTION = (
    "CALPHAD thermodynamic calculations. ONE tool, three actions. "
    "COMPUTE-HEAVY (runs locally via pycalphad, no credits charged) — "
    "requires_approval=True; the harness will prompt before each call. "
    "Before computing, PRISM resolves a source in strict order: configured "
    "owned file, server-confirmed entitled source, structured refusal. A TDB "
    "must declare a validated system covering every requested component; "
    "element presence alone is insufficient. `database_name` is an optional "
    "preferred source id/name, never an entitlement assertion.\n"
    "  • action='phase_diagram' — calculate a binary/ternary phase diagram. "
    "Required: `components` (e.g. ['Al', 'Ni']). Optional: `database_name`, "
    "`phases`, `temperature_range` (default [300, 2000, 50]), `pressure` "
    "(default 101325 Pa).\n"
    "  • action='equilibrium' — calculate equilibrium at specific T/P/X. "
    "Required: `components`, `conditions` (dict like {T: 1000, P: 101325, "
    "'X(AL)': 0.3}). Optional: `database_name`, `phases`.\n"
    "  • action='gibbs' — Gibbs energy surface for specified phases at a "
    "given temperature. Required: `components`, `phases`, `temperature`. "
    "Optional: `database_name`, `pressure`.\n"
    "Every successful result carries source id, database version, licence, "
    "coverage, source evidence class, TDB SHA-256, pycalphad version, exact "
    "conditions, units and a reproduce string. Licensed input never promotes "
    "its evidence class."
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
        # Catalog/IO dispatcher: no compute, no spend. Silence means GATED
        # now, so declare it.
        requires_approval=False,
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
                    "description": (
                        "Optional preferred licensed-source id or name. The source "
                        "must still pass local-file or server-side entitlement and "
                        "coverage checks."
                    ),
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
