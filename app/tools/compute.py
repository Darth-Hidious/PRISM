"""Compute Broker tools — submit jobs, check GPUs, monitor status.

These tools connect to the MARC27 Compute Broker API.
Users can dispatch jobs to RunPod, PRISM nodes, or any registered provider.
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


def _list_gpus(**kwargs) -> dict:
    """List available GPU types with pricing across all providers."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    try:
        gpus = client.compute.list_gpus()
        return {
            "gpus": gpus,
            "count": len(gpus),
            "source": "marc27_compute_broker",
        }
    except Exception as e:
        return {"error": str(e)}


def _list_providers(**kwargs) -> dict:
    """List registered compute providers."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    try:
        providers = client.compute.list_providers()
        return {
            "providers": providers,
            "count": len(providers),
            "source": "marc27_compute_broker",
        }
    except Exception as e:
        return {"error": str(e)}


def _estimate_cost(**kwargs) -> dict:
    """Estimate cost of a compute job before submitting."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    image = kwargs.get("image", "")
    gpu_type = kwargs.get("gpu_type")
    timeout = kwargs.get("timeout_seconds", 3600)

    try:
        estimate = client.compute.estimate(
            image=image,
            inputs={},
            gpu_type=gpu_type,
            timeout_seconds=timeout,
        )
        return {
            "estimate": estimate,
            "source": "marc27_compute_broker",
        }
    except Exception as e:
        return {"error": str(e)}


def _submit_job(**kwargs) -> dict:
    """Submit a compute job to the broker."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    image = kwargs.get("image", "")
    inputs = kwargs.get("inputs", {})
    gpu_type = kwargs.get("gpu_type")
    budget = kwargs.get("budget_max_usd")
    preference = kwargs.get("provider_preference", "cheapest")
    timeout = kwargs.get("timeout_seconds", 3600)
    env_vars = kwargs.get("env_vars", {})

    try:
        result = client.compute.submit(
            image=image,
            inputs=inputs,
            gpu_type=gpu_type,
            budget_max_usd=budget,
            provider_preference=preference,
            timeout_seconds=timeout,
            env_vars=env_vars,
        )
        return {
            "job": result,
            "source": "marc27_compute_broker",
        }
    except Exception as e:
        return {"error": str(e)}


def _job_status(**kwargs) -> dict:
    """Check the status of a compute job."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    job_id = kwargs.get("job_id", "")
    try:
        result = client.compute.status(job_id)
        return {
            "job": result,
            "source": "marc27_compute_broker",
        }
    except Exception as e:
        return {"error": str(e)}


def _cancel_job(**kwargs) -> dict:
    """Cancel a running compute job."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    job_id = kwargs.get("job_id", "")
    try:
        client.compute.cancel(job_id)
        return {"status": "cancelled", "job_id": job_id}
    except Exception as e:
        return {"error": str(e)}


def create_compute_tools(registry: ToolRegistry) -> None:
    """Register all Compute Broker tools."""

    registry.register(Tool(
        name="compute_gpus",
        description=(
            "List available GPU types with pricing across all compute providers "
            "(RunPod, PRISM nodes, Lambda, etc.). Shows GPU type, VRAM, price "
            "per hour, and availability. Use before submitting jobs to pick "
            "the right hardware."
        ),
        input_schema={"type": "object", "properties": {}},
        func=_list_gpus,
    ))

    registry.register(Tool(
        name="compute_providers",
        description=(
            "GPU/CPU compute providers registered with the MARC27 Compute Broker — "
            "RunPod, PRISM mesh nodes, Lambda, etc. Use this to see which "
            "execution backends are reachable and what hardware tiers they expose. "
            "NOT for listing LLM models (use models_list / models_search) and NOT "
            "for ML prediction models (use list_models)."
        ),
        input_schema={"type": "object", "properties": {}},
        func=_list_providers,
    ))

    registry.register(Tool(
        name="compute_estimate",
        description=(
            "Preview what a compute job would cost BEFORE actually "
            "submitting it. Free to call; no GPU is reserved. Returns "
            "estimated cost in USD, GPU options ranked by price, and "
            "expected wall-time based on similar past jobs. ALWAYS "
            "call this before `compute_submit` when the user has a "
            "budget concern, asks 'how much would it cost to...', or "
            "before any job that might run more than a few minutes. "
            "Pair: `compute_estimate` (preview, free) → user confirms "
            "→ `compute_submit` (real spend) → `compute_status` "
            "(monitor) → optionally `compute_cancel`."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "image": {
                    "type": "string",
                    "description": (
                        "Container image to estimate (e.g. "
                        "`vasp:6.5.0`, `quantum-espresso:7.4`) or a "
                        "marketplace slug. Use the same value you "
                        "intend to pass to `compute_submit`."
                    ),
                },
                "gpu_type": {
                    "type": "string",
                    "description": (
                        "Specific GPU to estimate against (e.g. "
                        "`A100-80GB`, `H100-80GB`). If omitted, "
                        "returns estimates across all available GPU "
                        "types so the agent can pick by cost-vs-speed."
                    ),
                },
                "timeout_seconds": {
                    "type": "integer",
                    "default": 3600,
                    "description": (
                        "Budget cap on wall-time. Default 1 hour. "
                        "Cost = price-per-hour × timeout / 3600."
                    ),
                },
            },
            "required": ["image"],
            "additionalProperties": False,
        },
        func=_estimate_cost,
    ))

    registry.register(Tool(
        name="compute_submit",
        description=(
            "Actually run a compute job — this is the MONEY-SPENDING "
            "action. Dispatches a containerized workload to the "
            "MARC27 Compute Broker, which picks the best matching "
            "provider (PRISM mesh node, RunPod, Lambda, etc.) based "
            "on `provider_preference` ('cheapest' default, 'fastest', "
            "or a specific provider name). Returns a job_id; poll "
            "with `compute_status` to track progress, abort with "
            "`compute_cancel` if needed. ALWAYS prefer running "
            "`compute_estimate` first when the user is cost-sensitive. "
            "Use this for: real DFT/MD runs, large training jobs, "
            "any container the user has authored. Set "
            "`budget_max_usd` to hard-cap spend per job — the broker "
            "refuses dispatch if the estimate exceeds it."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "image": {
                    "type": "string",
                    "description": (
                        "Container image (Docker tag, e.g. "
                        "`vasp:6.5.0`) or a marketplace slug "
                        "registered with the broker."
                    ),
                },
                "inputs": {
                    "type": "object",
                    "description": (
                        "JSON input payload for the container. Shape "
                        "depends on the image — DFT runners typically "
                        "want {structure, kpoints, encut, ...}, "
                        "training jobs want {dataset_uri, "
                        "hyperparameters, ...}."
                    ),
                },
                "gpu_type": {
                    "type": "string",
                    "description": (
                        "GPU class (e.g. `A100-80GB`, `H100-80GB`). "
                        "Use `compute_gpus` to see what's available."
                    ),
                },
                "budget_max_usd": {
                    "type": "number",
                    "description": (
                        "Hard cap on this job's cost in USD. The "
                        "broker refuses dispatch if the estimated "
                        "cost exceeds this. Recommended: always set "
                        "this for non-trivial jobs to prevent "
                        "runaway charges."
                    ),
                },
                "provider_preference": {
                    "type": "string",
                    "description": (
                        "Routing strategy: `cheapest` (default — "
                        "prefers PRISM mesh nodes, then cloud), "
                        "`fastest` (prefers premium cloud), or an "
                        "explicit provider name."
                    ),
                    "default": "cheapest",
                },
                "timeout_seconds": {
                    "type": "integer",
                    "default": 3600,
                    "description": (
                        "Wall-time cap. Job is killed at the cap "
                        "even if not done. Default 1 hour."
                    ),
                },
                "env_vars": {
                    "type": "object",
                    "description": (
                        "Environment variables passed to the "
                        "container. Use for API keys, license tokens, "
                        "feature flags."
                    ),
                },
            },
            "required": ["image", "inputs"],
            "additionalProperties": False,
        },
        func=_submit_job,
    ))

    registry.register(Tool(
        name="compute_status",
        description=(
            "Poll one compute job's state. Returns: status "
            "(`queued` / `running` / `completed` / `failed` / "
            "`cancelled`), elapsed wall-time, cost-incurred-so-far, "
            "the job's stdout/stderr tail (last few KB), and the "
            "final output artifact URLs once status=completed. Use "
            "this in a polling loop after `compute_submit` returned "
            "a job_id. Cheap to call repeatedly — no spend implied. "
            "If the user asks 'is my DFT done?', this is the right "
            "tool. If they want to abort it, follow up with "
            "`compute_cancel`. NOT for listing all jobs (no such "
            "tool today; the broker UI handles that)."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": (
                        "Job ID returned by `compute_submit`. "
                        "Format: `job_<uuid>` or numeric SLURM "
                        "ID depending on broker backend."
                    ),
                },
            },
            "required": ["job_id"],
            "additionalProperties": False,
        },
        func=_job_status,
    ))

    registry.register(Tool(
        name="compute_cancel",
        description=(
            "Stop a compute job that's currently queued or running on the "
            "MARC27 compute broker. Use this when a user says 'cancel my "
            "DFT run', 'kill job X', or you need to abort a job whose "
            "result is no longer wanted (e.g. an over-broad scan, a "
            "fix-up after a typo in the input deck). Returns the job's "
            "final state. Idempotent: cancelling an already-finished or "
            "already-cancelled job is a no-op. Does NOT clean up output "
            "artifacts — those persist; use the file-system tools to "
            "remove them if needed."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": (
                        "Compute job ID returned by `compute_submit`. "
                        "Looks like `job_<uuid>` or a numeric SLURM ID "
                        "depending on the broker backend."
                    ),
                },
            },
            "required": ["job_id"],
            "additionalProperties": False,
        },
        func=_cancel_job,
    ))
