"""Compute Broker — unified read-only tool + standalone money-spending submit.

Two tools:

  - `compute(action=…)` — read-only and idempotent operations:
    list_gpus, list_providers, estimate, status, cancel.
    No approval required. Cheap to call repeatedly (estimate is FREE,
    list_gpus/providers are catalog reads, status is a poll, cancel
    is idempotent).

  - `compute_submit(...)` — STANDALONE money-spending action.
    Dispatches a real GPU/CPU job to the MARC27 Compute Broker.
    `requires_approval=True` — the harness MUST prompt the user before
    each call. Cost is real, sometimes substantial (~$0.10–$10+ per job).

WHY THE SPLIT

Earlier collapse work folded all 6 compute_* tools into one
`compute(action=…)` tool. That was clean for surface reduction, but
collapsed the approval semantics: `requires_approval` is a per-tool
boolean, not per-action. Either every compute call needed approval
(annoying for list_gpus / estimate / status polling) or none did
(unsafe for submit). Splitting submit back out preserves the original
approval gate for money-spending while keeping the rest collapsed.

Same architectural pattern as bash_task / stop_bash_task: destructive
or money-spending actions stay isolated for approval.
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


# (handler, list-of-required-args) — read-only / idempotent actions ONLY.
# The money-spending `submit` action is NOT in this dispatch; it lives as
# a separate standalone tool with requires_approval=True (see below).
_DISPATCH: dict[str, tuple] = {
    "list_gpus":      (_act_list_gpus,      []),
    "list_providers": (_act_list_providers, []),
    "estimate":       (_act_estimate,       ["image"]),
    "status":         (_act_status,         ["job_id"]),
    "cancel":         (_act_cancel,         ["job_id"]),
}


def _compute(**kwargs) -> dict:
    """Read-only dispatcher. Money-spending `submit` is in `compute_submit`."""
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": f"Missing 'action'. Valid: {list(_DISPATCH.keys())}",
            "hint": (
                "compute(action='list_gpus') / compute(action='estimate', image='...') / "
                "compute(action='status', job_id='...'). For dispatching a real job, "
                "use the separate compute_submit tool (approval-gated)."
            ),
        }

    if action == "submit":
        # Hard-redirect callers who try the old interface
        return {
            "error": (
                "action='submit' moved to a separate tool `compute_submit` for "
                "safety. compute_submit requires user approval before each call "
                "because it spends real money. Call compute_submit(image=..., "
                "inputs=...) directly."
            ),
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


def _compute_submit(**kwargs) -> dict:
    """Money-spending dispatcher for compute_submit (approval-gated)."""
    if not kwargs.get("image"):
        return {"error": "compute_submit requires `image`"}
    if "inputs" not in kwargs:
        return {"error": "compute_submit requires `inputs` (JSON payload, may be empty {})"}

    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected. Run `prism login` first."}

    try:
        return _act_submit(client, **kwargs)
    except Exception as e:
        return {"error": str(e)}


_DESCRIPTION = (
    "MARC27 Compute Broker — read-only and idempotent operations across "
    "providers (PRISM mesh nodes, RunPod, Lambda). For dispatching real "
    "GPU/CPU jobs, use the separate `compute_submit` tool (approval-gated, "
    "money-spending).\n"
    "\n"
    "ONE tool, five read-only actions selected via `action`:\n"
    "  • action='list_gpus' — show available GPU types + pricing. No other args.\n"
    "  • action='list_providers' — show registered backends. No other args.\n"
    "  • action='estimate' — cost preview, FREE; requires `image`. Optional: `gpu_type`, `timeout_seconds`.\n"
    "  • action='status' — poll one job; requires `job_id`. Cheap, no spend.\n"
    "  • action='cancel' — abort queued/running job; requires `job_id`. Idempotent.\n"
    "Typical sequence: list_gpus → estimate → compute_submit → status (loop) → "
    "[cancel if needed]. NOT for listing LLM models (use models_list) and NOT "
    "for ML prediction (use predict)."
)


_SUBMIT_DESCRIPTION = (
    "Dispatch a real containerized GPU/CPU job to the MARC27 Compute Broker. "
    "MONEY-SPENDING ACTION — requires user approval before each call (the "
    "harness will prompt you).\n"
    "\n"
    "Required args: `image` (Docker tag like 'vasp:6.5.0' or marketplace slug), "
    "`inputs` (JSON payload for the container). Strongly recommended: "
    "`budget_max_usd` to hard-cap spend (broker refuses dispatch if estimate "
    "exceeds).\n"
    "\n"
    "Optional: `gpu_type` (A100-80GB, H100-80GB, ...; uses cheapest available "
    "if omitted), `provider_preference` ('cheapest' default — prefers PRISM "
    "mesh; 'fastest' = premium cloud; or specific provider name), "
    "`timeout_seconds` (wall-time cap, default 3600), `env_vars` (API keys, "
    "license tokens).\n"
    "\n"
    "Returns the job_id you can poll with compute(action='status', "
    "job_id=...). Use compute(action='estimate') first if cost is a concern. "
    "NOT for atomistic SLURM submission (that goes through pyiron sim_tools)."
)


_SCHEMA = {
    "type": "object",
    "properties": {
        "action": {
            "type": "string",
            "enum": list(_DISPATCH.keys()),
            "description": "Which read-only compute-broker operation to perform.",
        },
        "image": {
            "type": "string",
            "description": (
                "Container image (e.g. 'vasp:6.5.0', 'quantum-espresso:7.4') "
                "or marketplace slug. Required for action='estimate'."
            ),
        },
        "gpu_type": {
            "type": "string",
            "description": (
                "GPU class (e.g. 'A100-80GB', 'H100-80GB') for action='estimate'."
            ),
        },
        "timeout_seconds": {
            "type": "integer",
            "description": (
                "Wall-time cap in seconds for action='estimate'. "
                "Default 3600 (1 hour). Cost = price-per-hour × timeout / 3600."
            ),
            "default": 3600,
        },
        "job_id": {
            "type": "string",
            "description": (
                "Job ID returned by compute_submit (format: 'job_<uuid>' or "
                "numeric SLURM ID). Required for action='status' and action='cancel'."
            ),
        },
    },
    "required": ["action"],
    "additionalProperties": False,
}


_SUBMIT_SCHEMA = {
    "type": "object",
    "properties": {
        "image": {
            "type": "string",
            "description": (
                "Container image (Docker tag, e.g. 'vasp:6.5.0') or "
                "marketplace slug registered with the broker."
            ),
        },
        "inputs": {
            "type": "object",
            "description": (
                "JSON input payload for the container. Shape depends on the "
                "image — DFT runners want {structure, kpoints, encut, ...}; "
                "training jobs want {dataset_uri, hyperparameters, ...}. "
                "Pass {} if the image needs no inputs."
            ),
        },
        "gpu_type": {
            "type": "string",
            "description": "GPU class (e.g. 'A100-80GB'). Use compute(action='list_gpus') first.",
        },
        "budget_max_usd": {
            "type": "number",
            "description": (
                "Hard cap on this job's cost in USD. Broker refuses dispatch "
                "if estimated cost exceeds this. STRONGLY recommended for "
                "non-trivial jobs to prevent runaway charges."
            ),
        },
        "provider_preference": {
            "type": "string",
            "description": (
                "Routing strategy: 'cheapest' (default, prefers PRISM mesh "
                "nodes), 'fastest' (premium cloud), or specific provider name."
            ),
            "default": "cheapest",
        },
        "timeout_seconds": {
            "type": "integer",
            "description": (
                "Wall-time cap in seconds. Job is killed at the cap even if "
                "not done. Default 3600 (1 hour)."
            ),
            "default": 3600,
        },
        "env_vars": {
            "type": "object",
            "description": (
                "Environment variables passed to the container. Use for API "
                "keys, license tokens, feature flags."
            ),
        },
    },
    "required": ["image", "inputs"],
    "additionalProperties": False,
}


def create_compute_tools(registry: ToolRegistry) -> None:
    """Register the read-only `compute` tool + the approval-gated `compute_submit`.

    The split mirrors the bash_task / stop_bash_task pattern: destructive
    or money-spending actions stay isolated for per-tool approval gating.
    """
    registry.register(Tool(
        name="compute",
        description=_DESCRIPTION,
        input_schema=_SCHEMA,
        func=_compute,
    ))
    registry.register(Tool(
        name="compute_submit",
        description=_SUBMIT_DESCRIPTION,
        input_schema=_SUBMIT_SCHEMA,
        func=_compute_submit,
        requires_approval=True,
    ))
