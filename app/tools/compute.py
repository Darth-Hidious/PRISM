"""Compute Broker — UNIFIED tool collapsing 6 actions into one entry point.

DRAFT for Round 4 collapse work. Will replace app/tools/compute.py once PR #12 merges.

Replaces:
  compute_gpus, compute_providers, compute_estimate, compute_submit,
  compute_status, compute_cancel
Discriminator: `action` (string enum)

Approval semantics: same as today (none). Flag for follow-up:
  action='submit' is the money-spending one; should arguably set
  requires_approval=True. Out of scope for the collapse PR.
"""
import logging
from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


def _get_client():
    """Get or create a MARC27 PlatformClient."""
    try:
        from marc27 import PlatformClient
        return PlatformClient()
    except Exception as e:
        logger.warning(f"MARC27 SDK not available: {e}")
        return None


# --- Per-action handlers (each one stays small; no logic duplicated) -------

def _act_list_gpus(client, **_) -> dict:
    gpus = client.compute.list_gpus()
    return {"gpus": gpus, "count": len(gpus), "source": "marc27_compute_broker"}


def _act_list_providers(client, **_) -> dict:
    providers = client.compute.list_providers()
    return {
        "providers": providers,
        "count": len(providers),
        "source": "marc27_compute_broker",
    }


def _act_estimate(client, **kw) -> dict:
    estimate = client.compute.estimate(
        image=kw["image"],
        inputs={},
        gpu_type=kw.get("gpu_type"),
        timeout_seconds=kw.get("timeout_seconds", 3600),
    )
    return {"estimate": estimate, "source": "marc27_compute_broker"}


def _act_submit(client, **kw) -> dict:
    result = client.compute.submit(
        image=kw["image"],
        inputs=kw["inputs"],
        gpu_type=kw.get("gpu_type"),
        budget_max_usd=kw.get("budget_max_usd"),
        provider_preference=kw.get("provider_preference", "cheapest"),
        timeout_seconds=kw.get("timeout_seconds", 3600),
        env_vars=kw.get("env_vars", {}),
    )
    return {"job": result, "source": "marc27_compute_broker"}


def _act_status(client, **kw) -> dict:
    result = client.compute.status(kw["job_id"])
    return {"job": result, "source": "marc27_compute_broker"}


def _act_cancel(client, **kw) -> dict:
    client.compute.cancel(kw["job_id"])
    return {"status": "cancelled", "job_id": kw["job_id"]}


# (handler, list-of-required-args)
_DISPATCH: dict[str, tuple] = {
    "list_gpus":      (_act_list_gpus,      []),
    "list_providers": (_act_list_providers, []),
    "estimate":       (_act_estimate,       ["image"]),
    "submit":         (_act_submit,         ["image", "inputs"]),
    "status":         (_act_status,         ["job_id"]),
    "cancel":         (_act_cancel,         ["job_id"]),
}


def _compute(**kwargs) -> dict:
    """Single entry point dispatched by `action`."""
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": f"Missing 'action'. Valid: {list(_DISPATCH.keys())}",
            "hint": "Call as compute(action='list_gpus') or compute(action='submit', image=..., inputs=...)",
        }

    if action not in _DISPATCH:
        return {
            "error": f"Unknown action '{action}'. Valid: {list(_DISPATCH.keys())}",
        }

    handler, required = _DISPATCH[action]
    missing = [r for r in required if r not in kwargs]
    if missing:
        return {
            "error": f"Action '{action}' requires: {missing}",
            "provided": list(kwargs.keys()),
        }

    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected. Run `prism login` first."}

    try:
        return handler(client, **kwargs)
    except Exception as e:
        return {"error": str(e), "action": action}


_DESCRIPTION = (
    "MARC27 Compute Broker — submit, monitor, and cancel containerized "
    "GPU/CPU jobs across providers (PRISM mesh nodes, RunPod, Lambda). "
    "ONE tool, six actions selected via `action`:\n"
    "  • action='list_gpus' — show available GPU types + pricing. No other args.\n"
    "  • action='list_providers' — show registered backends. No other args.\n"
    "  • action='estimate' — cost preview, FREE; requires `image`. Optional: `gpu_type`, `timeout_seconds`.\n"
    "  • action='submit' — DISPATCHES A REAL JOB (money-spending); requires `image` + `inputs`. "
    "Set `budget_max_usd` to cap spend.\n"
    "  • action='status' — poll one job; requires `job_id`. Cheap, no spend.\n"
    "  • action='cancel' — abort queued/running job; requires `job_id`. Idempotent.\n"
    "Typical sequence: list_gpus → estimate → submit → status (loop) → [cancel if needed]. "
    "NOT for listing LLM models (use models_list) and NOT for ML prediction (use predict)."
)


_SCHEMA = {
    "type": "object",
    "properties": {
        "action": {
            "type": "string",
            "enum": list(_DISPATCH.keys()),
            "description": "Which compute-broker operation to perform.",
        },
        "image": {
            "type": "string",
            "description": (
                "Container image (e.g. 'vasp:6.5.0', 'quantum-espresso:7.4') "
                "or marketplace slug. Required for action='estimate' and action='submit'."
            ),
        },
        "inputs": {
            "type": "object",
            "description": (
                "JSON input payload for the container. Shape depends on the image — "
                "DFT runners want {structure, kpoints, encut, ...}, training jobs want "
                "{dataset_uri, hyperparameters, ...}. Required for action='submit'."
            ),
        },
        "gpu_type": {
            "type": "string",
            "description": (
                "GPU class (e.g. 'A100-80GB', 'H100-80GB'). Optional for "
                "action='estimate'/'submit'; uses cheapest available if omitted."
            ),
        },
        "budget_max_usd": {
            "type": "number",
            "description": (
                "Hard cap on job cost in USD for action='submit'. The broker "
                "refuses dispatch if estimated cost exceeds this. Strongly "
                "recommended for non-trivial jobs."
            ),
        },
        "provider_preference": {
            "type": "string",
            "description": (
                "Routing strategy for action='submit': 'cheapest' (default — "
                "prefers PRISM mesh nodes), 'fastest' (premium cloud), or an "
                "explicit provider name."
            ),
            "default": "cheapest",
        },
        "timeout_seconds": {
            "type": "integer",
            "description": (
                "Wall-time cap in seconds for action='estimate'/'submit'. "
                "Default 3600 (1 hour). Cost = price-per-hour × timeout / 3600."
            ),
            "default": 3600,
        },
        "env_vars": {
            "type": "object",
            "description": (
                "Environment variables passed to the action='submit' container. "
                "Use for API keys, license tokens, feature flags."
            ),
        },
        "job_id": {
            "type": "string",
            "description": (
                "Job ID returned by action='submit' (format: 'job_<uuid>' or "
                "numeric SLURM ID). Required for action='status' and action='cancel'."
            ),
        },
    },
    "required": ["action"],
    "additionalProperties": False,
}


def create_compute_tools(registry: ToolRegistry) -> None:
    """Register the unified `compute` tool (replaces 6 prior tools)."""
    registry.register(Tool(
        name="compute",
        description=_DESCRIPTION,
        input_schema=_SCHEMA,
        func=_compute,
    ))
