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
            "List registered compute providers in the MARC27 Compute Broker. "
            "Shows which providers are available (RunPod, PRISM nodes, etc.) "
            "and what GPU types each offers."
        ),
        input_schema={"type": "object", "properties": {}},
        func=_list_providers,
    ))

    registry.register(Tool(
        name="compute_estimate",
        description=(
            "Estimate the cost of a compute job before submitting. "
            "Provide the container image and GPU type to get a cost estimate."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "image": {"type": "string", "description": "Container image to run"},
                "gpu_type": {"type": "string", "description": "GPU type (e.g. A100-80GB)"},
                "timeout_seconds": {"type": "integer", "default": 3600},
            },
            "required": ["image"],
        },
        func=_estimate_cost,
    ))

    registry.register(Tool(
        name="compute_submit",
        description=(
            "Submit a compute job to the MARC27 Compute Broker. Dispatches "
            "to the best available provider (cheapest by default, or fastest). "
            "Returns a job_id to track progress. Use compute_status to monitor."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "image": {"type": "string", "description": "Container image or marketplace slug"},
                "inputs": {"type": "object", "description": "JSON input data for the container"},
                "gpu_type": {"type": "string", "description": "GPU type (e.g. A100-80GB, H100-80GB)"},
                "budget_max_usd": {"type": "number", "description": "Max budget in USD"},
                "provider_preference": {
                    "type": "string",
                    "description": "'cheapest' (default, prefers PRISM nodes), 'fastest' (prefers cloud), or provider name",
                    "default": "cheapest",
                },
                "timeout_seconds": {"type": "integer", "default": 3600},
                "env_vars": {"type": "object", "description": "Environment variables"},
            },
            "required": ["image", "inputs"],
        },
        func=_submit_job,
    ))

    registry.register(Tool(
        name="compute_status",
        description=(
            "Check the status of a compute job. Returns status (queued, running, "
            "completed, failed), cost, duration, output, and error details."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "Job ID from compute_submit"},
            },
            "required": ["job_id"],
        },
        func=_job_status,
    ))

    registry.register(Tool(
        name="compute_cancel",
        description="Cancel a running compute job.",
        input_schema={
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "Job ID to cancel"},
            },
            "required": ["job_id"],
        },
        func=_cancel_job,
    ))
