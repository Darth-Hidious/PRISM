"""Labs — UNIFIED tool collapsing 4 marketplace actions into one entry point.

Replaces (4 tools → 1):
  list_lab_services        → action='list'        (browse catalog)
  get_lab_service_info     → action='info'        (one service detail)
  check_lab_subscriptions  → action='subscriptions' (active subscriptions)
  submit_lab_job           → action='submit'      (dispatch a job)

PRESERVES the catalog + subscriptions JSON sources unchanged.
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


# --- Per-action handlers ---------------------------------------------------

def _act_list(**kw) -> dict:
    catalog = _load_catalog()
    services = catalog.get("services", {})
    category = kw.get("category")
    if category:
        services = {k: v for k, v in services.items() if v.get("category") == category}

    return {
        "services": [
            {
                "id": sid,
                "name": svc.get("name", sid),
                "category": svc.get("category", "unknown"),
                "provider": svc.get("provider", "unknown"),
                "cost_model": svc.get("cost_model", "unknown"),
                "status": svc.get("status", "coming_soon"),
                "description": svc.get("description", ""),
            }
            for sid, svc in services.items()
        ],
        "count": len(services),
    }


def _act_info(**kw) -> dict:
    service_id = kw.get("service_id")
    if not service_id:
        return {"error": "Action 'info' requires `service_id`"}
    svc = _load_catalog().get("services", {}).get(service_id)
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


def _act_subscriptions(**_) -> dict:
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


def _act_submit(**kw) -> dict:
    service_id = kw.get("service_id")
    if not service_id:
        return {"error": "Action 'submit' requires `service_id`"}

    svc = _load_catalog().get("services", {}).get(service_id)
    if not svc:
        return {"error": f"Service '{service_id}' not found"}

    if svc.get("status") == "coming_soon":
        return {
            "error": f"{svc['name']} is not yet available",
            "status": "coming_soon",
            "register_interest": f"https://prism.marc27.com/labs/{service_id}",
        }

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

    # HONESTY: there is no live dispatch path yet. Never pretend a job
    # was submitted — earlier versions returned status="submitted"
    # without doing anything, which lied to the agent (and the user).
    return {
        "error": (
            f"Job dispatch for {svc['name']} is not implemented yet — "
            "no job was submitted. Lab submission is coming to the "
            "MARC27 platform; list/info/subscriptions are real today."
        ),
        "status": "not_implemented",
        "register_interest": f"https://prism.marc27.com/labs/{service_id}",
    }


_DISPATCH = {
    "list":          _act_list,
    "info":          _act_info,
    "subscriptions": _act_subscriptions,
    "submit":        _act_submit,
}


def _labs(**kwargs) -> dict:
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": f"Missing 'action'. Valid: {list(_DISPATCH.keys())}",
            "hint": "labs(action='list') / labs(action='info', service_id='...') / labs(action='submit', service_id='...', parameters={...})",
        }
    handler = _DISPATCH.get(action)
    if not handler:
        return {"error": f"Unknown action '{action}'. Valid: {list(_DISPATCH.keys())}"}
    try:
        return handler(**kwargs)
    except Exception as e:
        return {"error": str(e), "action": action}


_DESCRIPTION = (
    "MARC27 Premium Labs marketplace catalog — autonomous robotic synthesis "
    "(A-Labs), design-for-manufacturing assessment, hosted DFT/QE/CP2K, real "
    "quantum hardware, synchrotron beamline time, HT screening. "
    "IMPORTANT: job SUBMISSION IS NOT LIVE YET — every service is "
    "'coming_soon' and action='submit' returns an error without dispatching "
    "anything. Do NOT promise users a lab run. Browsing is real. "
    "ONE tool, four actions:\n"
    "  • action='list' — browse catalog (real). Optional `category` filter "
    "(a-labs, dfm, cloud-dft, quantum, synchrotron, ht-screening).\n"
    "  • action='info' — full detail on one service (real). Requires `service_id`.\n"
    "  • action='subscriptions' — show active subscriptions + usage. No args.\n"
    "  • action='submit' — NOT YET AVAILABLE; returns status='coming_soon' / "
    "'not_implemented' with a register-interest link.\n"
    "NOT for compute-broker GPU jobs (use compute) and NOT for local "
    "simulations (use sim_tools)."
)


_SCHEMA = {
    "type": "object",
    "properties": {
        "action": {
            "type": "string",
            "enum": list(_DISPATCH.keys()),
            "description": "Which labs operation to perform.",
        },
        "service_id": {
            "type": "string",
            "description": "Lab service ID. Required for action='info' and action='submit'.",
        },
        "category": {
            "type": "string",
            "enum": ["a-labs", "dfm", "cloud-dft", "quantum", "synchrotron", "ht-screening"],
            "description": "Optional filter for action='list'.",
        },
        "parameters": {
            "type": "object",
            "description": "Job-specific payload for action='submit'. Shape depends on the service.",
        },
    },
    "required": ["action"],
    "additionalProperties": False,
}


def create_labs_tools(registry: ToolRegistry) -> None:
    """Register the unified `labs` tool (replaces 4 prior tools)."""
    registry.register(Tool(
        name="labs",
        description=_DESCRIPTION,
        input_schema=_SCHEMA,
        func=_labs,
    ))
