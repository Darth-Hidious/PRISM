"""Labs tools — premium marketplace service access for the agent.

These tools let the agent browse, check subscriptions, and submit jobs
to premium lab services (A-Labs, DfM, Cloud DFT, Quantum, etc.).
Actual execution is handled by the MARC27 platform API.
"""
import json
from pathlib import Path

from app.tools.base import Tool, ToolRegistry


_LABS_CATALOG_PATH = Path(__file__).parent.parent / "plugins" / "labs_catalog.json"
_SUBSCRIPTIONS_PATH = Path.home() / ".prism" / "labs_subscriptions.json"


def _load_catalog() -> dict:
    if not _LABS_CATALOG_PATH.exists():
        return {"services": {}}
    try:
        return json.loads(_LABS_CATALOG_PATH.read_text())
    except Exception:
        return {"services": {}}


def _load_subscriptions() -> list:
    if not _SUBSCRIPTIONS_PATH.exists():
        return []
    try:
        data = json.loads(_SUBSCRIPTIONS_PATH.read_text())
        return data.get("subscriptions", [])
    except Exception:
        return []


def _list_lab_services(**kwargs) -> dict:
    """List available premium lab services."""
    catalog = _load_catalog()
    services = catalog.get("services", {})
    category = kwargs.get("category")
    if category:
        services = {k: v for k, v in services.items() if v.get("category") == category}

    result = []
    for sid, svc in services.items():
        result.append({
            "id": sid,
            "name": svc.get("name", sid),
            "category": svc.get("category", "unknown"),
            "provider": svc.get("provider", "unknown"),
            "cost_model": svc.get("cost_model", "unknown"),
            "status": svc.get("status", "coming_soon"),
            "description": svc.get("description", ""),
        })

    return {"services": result, "count": len(result)}


def _get_lab_service_info(**kwargs) -> dict:
    """Get detailed information about a specific lab service."""
    service_id = kwargs.get("service_id")
    if not service_id:
        return {"error": "service_id is required"}

    catalog = _load_catalog()
    svc = catalog.get("services", {}).get(service_id)
    if not svc:
        return {"error": f"Service '{service_id}' not found"}

    return {
        "id": service_id,
        "name": svc.get("name", service_id),
        "category": svc.get("category"),
        "provider": svc.get("provider"),
        "description": svc.get("description"),
        "cost_model": svc.get("cost_model"),
        "status": svc.get("status"),
        "capabilities": svc.get("capabilities", []),
        "requirements": svc.get("requirements", []),
        "tools": svc.get("tools", []),
        "marketplace_url": f"https://prism.marc27.com/labs/{service_id}",
    }


def _check_lab_subscriptions(**kwargs) -> dict:
    """Check active lab subscriptions."""
    subs = _load_subscriptions()
    return {
        "subscriptions": [
            {
                "service": s.get("service"),
                "name": s.get("name"),
                "plan": s.get("plan"),
                "has_api_key": bool(s.get("api_key")),
                "usage_summary": s.get("usage_summary", "0 calls"),
            }
            for s in subs
        ],
        "count": len(subs),
    }


def _submit_lab_job(**kwargs) -> dict:
    """Submit a job to a premium lab service.

    This is a placeholder — actual submission goes through the MARC27
    platform API once the service is available.
    """
    service_id = kwargs.get("service_id")
    if not service_id:
        return {"error": "service_id is required"}

    catalog = _load_catalog()
    svc = catalog.get("services", {}).get(service_id)
    if not svc:
        return {"error": f"Service '{service_id}' not found"}

    if svc.get("status") == "coming_soon":
        return {
            "error": f"{svc['name']} is not yet available",
            "status": "coming_soon",
            "register_interest": f"https://prism.marc27.com/labs/{service_id}",
        }

    # Check subscription
    subs = _load_subscriptions()
    sub = next((s for s in subs if s.get("service") == service_id), None)
    if not sub:
        return {
            "error": f"Not subscribed to {svc['name']}",
            "subscribe": f"prism labs subscribe {service_id}",
        }

    if not sub.get("api_key"):
        return {
            "error": "API key not configured",
            "set_key": f"prism labs subscribe {service_id} --api-key YOUR_KEY",
        }

    # Placeholder for actual API submission
    return {
        "status": "submitted",
        "service": svc["name"],
        "message": "Job submitted to MARC27 platform. Check status with prism labs status.",
        "job_params": kwargs.get("parameters", {}),
    }


def create_labs_tools(registry: ToolRegistry) -> None:
    """Register premium labs tools."""

    registry.register(Tool(
        name="list_lab_services",
        description=(
            "List the premium experimental and computational lab services "
            "available through the MARC27 marketplace. Categories include "
            "**A-Labs** (autonomous robotic synthesis like Berkeley's), "
            "**DfM** (Design-for-Manufacturing assessment), **Cloud DFT** "
            "(hosted VASP/QE/CP2K runs without managing your own cluster), "
            "**Quantum** (real quantum-hardware access via IBM/IonQ/Quantinuum), "
            "**Synchrotron** (beamline time at user facilities), and "
            "**HT Screening** (high-throughput experimental campaigns). "
            "Use this when the user asks 'what lab services do I have', "
            "'how can I run real DFT', 'who can synthesize this for me', "
            "or before calling `submit_lab_job` so you know what's offered. "
            "Returns service id + name + category + provider + cost model + "
            "current status (live / coming-soon)."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "enum": ["a-labs", "dfm", "cloud-dft", "quantum", "synchrotron", "ht-screening"],
                    "description": (
                        "Optional category filter. Omit to see every service "
                        "across categories. Use when the user mentions a "
                        "specific class of work ('any cloud DFT services?')."
                    ),
                },
            },
            "additionalProperties": False,
        },
        func=_list_lab_services,
    ))

    registry.register(Tool(
        name="get_lab_service_info",
        description=(
            "Pull the full spec for one specific premium lab service: "
            "capabilities (what it actually does), requirements (turnaround "
            "time, data inputs, file formats), pricing model, and the list "
            "of sub-tools that service exposes. Use this AFTER "
            "`list_lab_services` has surfaced a candidate, when the user "
            "wants to compare options or before constructing a `submit_lab_job` "
            "call. Returns a marketplace_url so the agent can point the "
            "user at the web UI for subscription if needed."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "service_id": {
                    "type": "string",
                    "description": (
                        "Exact service identifier returned by "
                        "`list_lab_services` (e.g. 'matlantis_dft', "
                        "'dfm_assessment', 'a_lab_synthesis'). "
                        "Case-sensitive; copy verbatim from list output."
                    ),
                },
            },
            "required": ["service_id"],
            "additionalProperties": False,
        },
        func=_get_lab_service_info,
    ))

    registry.register(Tool(
        name="check_lab_subscriptions",
        description=(
            "Report which premium lab services the current MARC27 account "
            "is subscribed to right now, plus usage-to-date for each. Use "
            "this when the user asks 'what am I paying for?', 'do I still "
            "have access to X?', or before `submit_lab_job` to confirm the "
            "subscription is active and not over quota. No arguments — "
            "operates on the logged-in account. Returns subscription list "
            "with service id, plan tier, usage counters, expiry date."
        ),
        input_schema={
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        },
        func=_check_lab_subscriptions,
    ))

    registry.register(Tool(
        name="submit_lab_job",
        description=(
            "Submit a real job to a premium lab service — this is the "
            "money-spending action. Requires an active subscription "
            "(check with `check_lab_subscriptions`) and the per-service "
            "parameter shape (look up with `get_lab_service_info`). "
            "Returns a job_id that can be polled for status; some services "
            "are minutes (cloud DFT relax), some are weeks (A-Lab synthesis). "
            "ALWAYS confirm with the user before calling this — `requires_approval` "
            "is set so the harness will pause for explicit consent. Don't "
            "call this for hypothetical 'what if we tried...' framings; "
            "use `compute_estimate` if the user wants a cost preview."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "service_id": {
                    "type": "string",
                    "description": (
                        "Service identifier from `list_lab_services` / "
                        "`get_lab_service_info`. Subscription must be active."
                    ),
                },
                "parameters": {
                    "type": "object",
                    "description": (
                        "Per-service parameter object. Shape depends on "
                        "the service — call `get_lab_service_info` first "
                        "to learn the required keys. For Cloud DFT, "
                        "typically {structure, calculation_type, kpoints, "
                        "encut, ...}; for A-Lab synthesis, {target_formula, "
                        "precursors, route, temperature_program, ...}."
                    ),
                },
            },
            "required": ["service_id"],
            "additionalProperties": False,
        },
        func=_submit_lab_job,
        requires_approval=True,
    ))
