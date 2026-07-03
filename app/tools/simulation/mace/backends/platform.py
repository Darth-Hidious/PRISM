"""PlatformBackend — marc27 platform `ml_predict` job submission.

When a MACE primitive runs against this backend, it does NOT execute MACE
in-process. Instead it builds an `ml_predict` job spec, POSTs it to the
marc27 platform via PRISM's existing `_platform_jobs_submit` helper, then
polls for completion via `_platform_jobs(action="status", job_id=...)`.

This is the production compute path for the CUDA-play architecture:

    PRISM agent (laptop)
        ↓ native function call
    mace_relax_structure / mace_md_equilibrate / ...
        ↓ runner picks PlatformBackend (when MACE_MCP_BACKEND=platform
        ↓ or the heuristic decides the cell is too big for local CPU)
    PlatformBackend.execute
        ↓ HTTPS POST {api}/jobs
    marc27 platform (Railway)
        ↓ enqueues ml_predict job for model=mace-mh-1
    Container `marc27/uip-mace-mh1:latest` (entrypoint_mh1.py)
        ↓ runs MACE-MH-1 (mace-torch + ASE)
    Result + provenance flow back to PRISM via job polling

## Framework note (aerospace-grade honesty)

MACE-MH-1 is PyTorch-only as shipped by upstream (mace-foundations,
mace-torch >= 0.3.12). mace-jax does NOT support the multi-head MH-1
architecture as of 2026. PRISM's broader stack is JAX-native by default
(jax-md / Flax / Equinox / NumPyro / BlackJAX); MACE is one of the explicit
upstream-pinned PyTorch holdouts. The platform-side container therefore
uses mace-torch, not mace-jax. Marketplace metadata in marc27-core records
`framework: pytorch, tier: hybrid-pending-jax` for honest agent routing.

## Tool→task mapping

The marc27 platform's `ml_predict` job type currently exposes three tasks
on UIP models (per `crates/jobs/src/types/ml_predict/registry.rs`):

    supported_tasks = ["single_point", "relax", "md"]

That covers `relax_structure` and `md_equilibrate` directly. The other
three MACE primitives — `phonon_harmonic`, `compute_elastic`,
`compute_dilute_solute` — are not yet first-class `ml_predict` tasks.
They need orchestration via multiple `relax`/`single_point` ml_predict
calls. Until that orchestration ships, this backend raises a clear
`NotImplementedError` for those tools, and the runner falls through to
`local` / `hf_jobs` / `fake` per `select_backend`.

## Configuration

Reads `PRISM_PROJECT_ID` from env (required) — the marc27 project the
job is billed against. PRISM's CLI sets this on `prism project use`.

Optionally reads `PRISM_PLATFORM_POLL_INTERVAL_S` (default 5.0) so test
suites can speed up polling.
"""

from __future__ import annotations

import logging
import os
import time
from typing import Any

from .base import Backend, BackendJob, ProgressCb

logger = logging.getLogger(__name__)


# Map mace primitive tool name → ml_predict supported_task.
# Only the three tasks the platform currently understands are listed;
# everything else raises NotImplementedError below.
_TOOL_TO_ML_PREDICT_TASK: dict[str, str] = {
    "relax_structure": "relax",
    "md_equilibrate": "md",
    # `phonon_harmonic`, `compute_elastic`, `compute_dilute_solute` are
    # not directly representable as a single ml_predict task — they need
    # orchestrated multi-call workflows that we'll add in a follow-up.
}


# Default canonical model for this backend. Override per-call via
# `job.input_payload["options"]["model"]` if the user wants mace-mp-0 or
# another UIP from the registry.
_DEFAULT_MODEL = "mace-mh-1"


class PlatformBackend(Backend):
    """Submit MACE primitives as `ml_predict` jobs on the marc27 platform."""

    name = "platform"

    def __init__(self) -> None:
        # Track cancelled jobs so a parallel cancel() call short-circuits
        # the poll loop without waiting for the next status request.
        self._cancelled: set[str] = set()

    # ------------------------------------------------------------------
    # Backend interface
    # ------------------------------------------------------------------

    def cancel(self, job_id: str) -> None:
        """Mark a job for cancellation; the poll loop checks this each tick."""
        self._cancelled.add(job_id)

    def execute(
        self, job: BackendJob, progress: ProgressCb | None = None
    ) -> dict[str, Any]:
        """Submit the job to the platform and block until terminal state."""
        # Lazy-import to keep this module importable without PRISM's full
        # platform stack — useful for `check_mace_available()` introspection
        # before the runner is fully wired.
        from app.tools.platform_jobs import _platform_jobs, _platform_jobs_submit

        task = _TOOL_TO_ML_PREDICT_TASK.get(job.tool_name)
        if task is None:
            raise NotImplementedError(
                f"PlatformBackend doesn't yet support {job.tool_name!r}. "
                f"Supported tools: {sorted(_TOOL_TO_ML_PREDICT_TASK)}. "
                f"The other MACE primitives (phonon_harmonic / compute_elastic "
                f"/ compute_dilute_solute) need multi-call orchestration that "
                f"hasn't shipped yet — fall back to backend=local / hf_jobs "
                f"by setting options.backend explicitly."
            )

        project_id = os.environ.get("PRISM_PROJECT_ID")
        if not project_id:
            raise RuntimeError(
                "PRISM_PROJECT_ID env var is required for the platform backend. "
                "Set it via `prism project use <project>` (which exports it to "
                "the subshell) or `export PRISM_PROJECT_ID=<uuid>` directly."
            )

        # The agent can override the default model on a per-call basis via
        # options.model — e.g. to fall back to mace-mp-0 for cells that fit
        # the older single-head architecture.
        options = job.input_payload.get("options", {}) or {}
        model = options.get("model", _DEFAULT_MODEL)
        head = options.get("head", "omat_pbe")
        dtype = options.get("dtype", "float64")

        payload = {
            "model": model,
            "task": task,
            "input": job.input_payload,
            "options": {
                "head": head,
                "dtype": dtype,
                "seed": job.seed,
            },
            "cache_key": job.cache_key,
            "tool_name": job.tool_name,
        }

        logger.info(
            "PlatformBackend.submit tool=%s model=%s task=%s head=%s",
            job.tool_name, model, task, head,
        )

        submit = _platform_jobs_submit(
            job_type="ml_predict",
            project_id=project_id,
            payload=payload,
        )
        if not isinstance(submit, dict) or submit.get("error"):
            raise RuntimeError(
                f"Platform submit failed for {job.tool_name}: "
                f"{submit.get('error') if isinstance(submit, dict) else submit}"
            )
        platform_job_id = submit.get("job_id") or submit.get("id")
        if not platform_job_id:
            raise RuntimeError(
                f"Platform submit returned no job_id for {job.tool_name}: {submit}"
            )

        if progress:
            progress(
                0.0,
                f"submitted as platform ml_predict job {platform_job_id} "
                f"(model={model}, task={task})",
                0,
                1,
            )

        # Poll until terminal state.
        poll_interval = float(
            os.environ.get("PRISM_PLATFORM_POLL_INTERVAL_S", "5.0")
        )
        deadline = time.time() + job.timeout_seconds
        last_pct: float = 0.0

        while True:
            if platform_job_id in self._cancelled:
                # Best-effort cancel on the platform side.
                try:
                    _platform_jobs(action="cancel", job_id=platform_job_id)
                except Exception:  # noqa: BLE001
                    pass
                raise RuntimeError(
                    f"Platform job {platform_job_id} cancelled locally"
                )

            if time.time() > deadline:
                # Best-effort cancel on timeout.
                try:
                    _platform_jobs(action="cancel", job_id=platform_job_id)
                except Exception:  # noqa: BLE001
                    pass
                raise TimeoutError(
                    f"Platform job {platform_job_id} exceeded timeout "
                    f"{job.timeout_seconds}s for tool {job.tool_name}"
                )

            status_resp = _platform_jobs(action="status", job_id=platform_job_id)
            if not isinstance(status_resp, dict) or status_resp.get("error"):
                # Transient API failures shouldn't kill the job — retry once
                # after the normal poll interval, then escalate.
                logger.warning(
                    "platform status check returned error for %s: %s",
                    platform_job_id,
                    status_resp,
                )
                time.sleep(poll_interval)
                continue

            status = (status_resp.get("status") or "").lower()
            pct = float(status_resp.get("progress_percent", last_pct) or last_pct)

            if progress and pct != last_pct:
                progress(
                    pct,
                    status_resp.get("message") or status or "running",
                    int(status_resp.get("step", 0)),
                    int(status_resp.get("total", 0)),
                )
                last_pct = pct

            if status == "succeeded":
                result = status_resp.get("result") or {}
                if not isinstance(result, dict):
                    raise RuntimeError(
                        f"Platform job {platform_job_id} succeeded but "
                        f"returned non-dict result: {type(result).__name__}"
                    )
                # Attach backend_details so provenance bundles record where
                # the work actually ran. The mace cache layer downstream uses
                # this to distinguish platform-served vs local-served runs.
                result.setdefault("backend_details", {}).update({
                    "backend": "platform",
                    "platform_job_id": platform_job_id,
                    "model": model,
                    "task": task,
                    "head": head,
                    "wall_time_s": round(time.time() - (deadline - job.timeout_seconds), 2),
                })
                if progress:
                    progress(100.0, "succeeded", 1, 1)
                return result

            if status in {"failed", "cancelled", "error"}:
                error = (
                    status_resp.get("error")
                    or status_resp.get("message")
                    or "no details from platform"
                )
                raise RuntimeError(
                    f"Platform job {platform_job_id} ended with status={status} "
                    f"for {job.tool_name}: {error}"
                )

            # status is queued / submitted / running / unknown — keep polling
            time.sleep(poll_interval)
