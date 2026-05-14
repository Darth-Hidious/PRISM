"""Async job dispatcher.

The MCP tool handlers in ``mace_mcp.tools.*`` build a ``BackendJob``,
hand it to the ``JobRunner``, and return a ``JobHandle`` immediately. The
runner spawns a worker task that calls the chosen backend's
``execute()`` in a thread executor (most backends are sync / blocking), and
writes status + result + provenance to disk as they happen.

Progress callbacks from the backend are translated into:
  - SQLite updates (so ``get_job`` sees them).
  - MCP ``notifications/progress`` messages (so the client sees them
    live, if it supplied a progress token).
"""

from __future__ import annotations

import asyncio
import json
import traceback
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Awaitable, Callable

from ..auth import get_cache_dir, scrub_token
from ..cache import CacheStore
from ..ids import new_job_id
from ..logging_cfg import get_logger
from ..schemas import JobHandle
from . import provenance as provmod
from .store import JobStore

log = get_logger("mace_mcp.runner")

# Type aliases
ProgressEmitter = Callable[[str, float, str, int, int], Awaitable[None] | None]


class JobRunner:
    def __init__(
        self,
        store: JobStore,
        backends: dict[str, Any],
        cache_root: Path | None = None,
        max_workers: int = 4,
    ) -> None:
        self.store = store
        self.backends = backends
        self.cache = CacheStore(Path(cache_root) if cache_root else get_cache_dir())
        self.executor = ThreadPoolExecutor(max_workers=max_workers, thread_name_prefix="mace-mcp-job")
        self._tasks: dict[str, asyncio.Task] = {}
        # progress emitter is set by the MCP server at startup so that
        # ``notifications/progress`` can be pushed to the client.
        self.progress_emitter: ProgressEmitter | None = None
        # progress_token map: job_id -> client-supplied token
        self._progress_tokens: dict[str, Any] = {}

    # ------------------------------------------------------------------
    async def submit(
        self,
        tool_name: str,
        input_payload: dict[str, Any],
        cache_key: str,
        backend_name: str,
        seed: int,
        progress_token: Any | None = None,
        timeout_seconds: int = 3600,
    ) -> JobHandle:
        """Create a job row, return a JobHandle, kick off the worker.

        Cache-hit shortcut: if the cache already has a result for this
        ``cache_key``, we mark the job ``succeeded`` immediately, inline
        the result, and skip the backend.
        """
        from .store import now_iso

        job_id = new_job_id()
        # Cache hit? Short-circuit.
        cached = self.cache.read_result(cache_key) if self.cache.has_result(cache_key) else None
        self.store.create(
            job_id=job_id,
            tool_name=tool_name,
            input_payload=input_payload,
            cache_key=cache_key,
            backend=backend_name if not cached else "cache",
        )
        if progress_token is not None:
            self._progress_tokens[job_id] = progress_token

        if cached is not None:
            self.store.transition(job_id, "submitted")
            self.store.transition(job_id, "running")
            self.store.set_result(job_id, cached)
            self.store.transition(job_id, "succeeded")
            self.store.set_provenance_ref(job_id, f"cache://{cache_key}/provenance.json")
            return JobHandle(
                job_id=job_id,
                status="succeeded",
                tool_name=tool_name,
                estimated_seconds=0,
                cache_hit=True,
                result=cached,
                provenance_ref=f"cache://{cache_key}/provenance.json",
                cache_key=cache_key,
            )

        backend = self.backends[backend_name]
        est = backend.estimate_seconds(_pseudo_job(tool_name, input_payload, cache_key))
        # Kick off worker
        task = asyncio.create_task(
            self._run_one(
                job_id=job_id,
                tool_name=tool_name,
                input_payload=input_payload,
                cache_key=cache_key,
                backend_name=backend_name,
                seed=seed,
                timeout_seconds=timeout_seconds,
            )
        )
        self._tasks[job_id] = task
        return JobHandle(
            job_id=job_id,
            status="queued",
            tool_name=tool_name,
            estimated_seconds=est,
            cache_hit=False,
            cache_key=cache_key,
        )

    # ------------------------------------------------------------------
    async def _run_one(
        self,
        *,
        job_id: str,
        tool_name: str,
        input_payload: dict[str, Any],
        cache_key: str,
        backend_name: str,
        seed: int,
        timeout_seconds: int,
    ) -> None:
        backend = self.backends[backend_name]
        loop = asyncio.get_running_loop()

        # Progress relay (sync callback from backend thread → async server)
        def on_progress(pct: float, msg: str, step: int, total: int) -> None:
            self.store.update_progress(job_id, pct, msg, step, total)
            if self.progress_emitter is None:
                return
            token = self._progress_tokens.get(job_id)
            if token is None:
                return
            try:
                # Schedule the async emit on the event loop without blocking the
                # worker thread.
                res = self.progress_emitter(token, pct, msg, step, total)
                if asyncio.iscoroutine(res):
                    asyncio.run_coroutine_threadsafe(res, loop)
            except Exception:
                pass

        from ..backends.base import BackendJob  # local import to avoid cycle
        bj = BackendJob(
            tool_name=tool_name,
            input_payload=input_payload,
            cache_key=cache_key,
            seed=seed,
            timeout_seconds=timeout_seconds,
        )
        self.store.transition(job_id, "submitted")
        self.store.transition(job_id, "running")
        t0_iso = datetime.now(timezone.utc).isoformat()
        try:
            result = await asyncio.wait_for(
                loop.run_in_executor(self.executor, backend.execute, bj, on_progress),
                timeout=timeout_seconds + 60,
            )
        except asyncio.CancelledError:
            self.store.set_error(job_id, {"kind": "cancelled", "message": "task cancelled"})
            self.store.transition(job_id, "cancelled")
            backend.cancel(cache_key)
            return
        except InterruptedError:
            self.store.transition(job_id, "cancelled")
            return
        except Exception as ex:
            tb = traceback.format_exc()
            self.store.set_error(
                job_id,
                {
                    "kind": ex.__class__.__name__,
                    "message": scrub_token(str(ex)),
                    "traceback": scrub_token(tb),
                },
            )
            self.store.transition(job_id, "failed")
            log.error(
                "job_failed",
                job_id=job_id,
                tool_name=tool_name,
                error=scrub_token(str(ex)),
            )
            return

        # Persist artefacts (CIF, traj) + provenance + result
        cif_text = result.pop("cif_text", None)
        traj_json = result.pop("traj_json", None)
        backend_details = result.pop("backend_details", {})

        if cif_text:
            self.cache.write_structure_cif(cache_key, cif_text)
            result["structure_cif_ref"] = f"cache://{cache_key}/structure.cif"
        if traj_json is not None:
            self.cache.write_traj_json(cache_key, traj_json)
            result["traj_ref"] = f"cache://{cache_key}/traj.json"

        wall = float(result.get("wall_time_s", 0.0))
        head = input_payload.get("options", {}).get("head", "omat_pbe")
        dtype = input_payload.get("options", {}).get("dtype", "float64")
        prov = provmod.build(
            tool_name=tool_name,
            job_id=job_id,
            cache_key=cache_key,
            input_payload=input_payload,
            result_summary=_summarise_result(result),
            head=head,
            dtype=dtype,
            backend=backend.name,
            backend_details=backend_details,
            wall_time_s=wall,
            quality_flags={"started_at_iso": t0_iso},
        )
        self.cache.write_provenance(cache_key, prov)
        self.cache.write_meta(
            cache_key,
            {
                "tool_name": tool_name,
                "source_job_id": job_id,
                "head": head,
                "phase": _phase_from_input(input_payload),
                "composition": _composition_from_input(input_payload),
                "n_atoms": _n_atoms_from_input(input_payload),
            },
        )
        prov_ref = f"cache://{cache_key}/provenance.json"
        result["provenance_ref"] = prov_ref

        # Best-effort push to HF Dataset
        try:
            push_url = provmod.push_to_dataset(
                cache_key,
                files={
                    "provenance.json": self.cache.root / cache_key / "provenance.json",
                    "result.json": self.cache.root / cache_key / "result.json",
                    "structure.cif": self.cache.root / cache_key / "structure.cif",
                    "traj.json": self.cache.root / cache_key / "traj.json",
                },
            )
            if push_url:
                self.store.set_provenance_ref(job_id, push_url)
        except Exception as ex:
            log.warning("dataset_push_failed_after_result", error=scrub_token(str(ex)))

        # Write result + finalise
        self.cache.write_result(cache_key, result)
        self.store.set_result(job_id, result)
        self.store.set_provenance_ref(job_id, prov_ref)
        self.store.transition(job_id, "succeeded")

    # ------------------------------------------------------------------
    async def cancel(self, job_id: str) -> str:
        task = self._tasks.get(job_id)
        rec = self.store.get(job_id)
        if rec is None:
            return "unknown"
        if rec.status in {"succeeded", "failed", "cancelled"}:
            return "finished"
        # mark cancelling so subsequent updates see it
        try:
            self.store.transition(job_id, "cancelling")
        except Exception:
            pass
        # tell the backend
        if rec.backend and rec.backend in self.backends and rec.cache_key:
            try:
                self.backends[rec.backend].cancel(rec.cache_key)
            except Exception:
                pass
        if task is not None:
            task.cancel()
        return "cancelling"

    async def shutdown(self) -> None:
        for t in list(self._tasks.values()):
            if not t.done():
                t.cancel()
        self.executor.shutdown(wait=False, cancel_futures=True)


# ----------------------------------------------------------------------
# helpers
# ----------------------------------------------------------------------

def _pseudo_job(tool: str, ip: dict[str, Any], cache_key: str):
    from ..backends.base import BackendJob

    return BackendJob(tool_name=tool, input_payload=ip, cache_key=cache_key)


def _summarise_result(result: dict[str, Any]) -> dict[str, Any]:
    """Trim large arrays out of the result for the provenance summary."""
    out: dict[str, Any] = {}
    for k, v in result.items():
        if isinstance(v, list) and len(v) > 16:
            out[k] = f"<{len(v)}-element array>"
        elif isinstance(v, list) and v and isinstance(v[0], list) and len(v[0]) > 8:
            out[k] = f"<{len(v)}x{len(v[0])} matrix>"
        else:
            out[k] = v
    return out


def _composition_from_input(ip: dict[str, Any]) -> dict[str, int] | None:
    if "composition" in ip:
        return ip["composition"]["atoms"]
    if "matrix_composition" in ip:
        return ip["matrix_composition"]["atoms"]
    if "structure" in ip and ip["structure"].get("composition"):
        return ip["structure"]["composition"]["atoms"]
    return None


def _phase_from_input(ip: dict[str, Any]) -> str | None:
    return (
        ip.get("phase")
        or ip.get("matrix_phase")
        or (ip.get("structure") or {}).get("phase")
    )


def _n_atoms_from_input(ip: dict[str, Any]) -> int | None:
    if "n_atoms" in ip:
        return int(ip["n_atoms"])
    if "structure" in ip and "n_atoms" in ip["structure"]:
        return int(ip["structure"]["n_atoms"])
    return None
