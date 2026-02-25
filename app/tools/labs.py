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
        description="List available premium lab services (A-Labs, DfM, Cloud DFT, Quantum, Synchrotron, HT Screening).",
        input_schema={
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "Filter by category: a-labs, dfm, cloud-dft, quantum, synchrotron, ht-screening",
                },
            },
        },
        func=_list_lab_services,
    ))

    registry.register(Tool(
        name="get_lab_service_info",
        description="Get detailed information about a specific premium lab service including capabilities, requirements, and pricing.",
        input_schema={
            "type": "object",
            "properties": {
                "service_id": {"type": "string", "description": "Service identifier (e.g. matlantis_dft, dfm_assessment)"},
            },
            "required": ["service_id"],
        },
        func=_get_lab_service_info,
    ))

    registry.register(Tool(
        name="check_lab_subscriptions",
        description="Check active premium lab subscriptions and usage.",
        input_schema={"type": "object", "properties": {}},
        func=_check_lab_subscriptions,
    ))

    registry.register(Tool(
        name="submit_lab_job",
        description="Submit a job to a premium lab service (requires subscription and API key).",
        input_schema={
            "type": "object",
            "properties": {
                "service_id": {"type": "string", "description": "Service identifier"},
                "parameters": {"type": "object", "description": "Job-specific parameters (varies by service)"},
            },
            "required": ["service_id"],
        },
        func=_submit_lab_job,
        requires_approval=True,
    ))
