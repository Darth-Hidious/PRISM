"""Scenario coverage: the sync→async bridge must NOT serialise jobs.

This is the acceptance test for the JobRunner persistent-loop fix. It
proves the three properties any real research mission depends on — submit
returns before the job finishes, the job still completes in the
background, and independent jobs run concurrently (not one-after-another).
The pre-fix drain implementation would FAIL property 1 and 3 (every
submit blocked until its job finished → an N-candidate grid took N× as
long and blew its wall-clock deadline).

Uses a controllable slow backend because FakeBackend is sub-millisecond —
too fast to distinguish concurrent from serial.
"""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from app.tools.mace import _run_async
from app.tools.simulation.mace.backends.base import Backend, BackendJob
from app.tools.simulation.mace.jobs import JobRunner, JobStore

_DUR = 0.4  # seconds each job "computes"


class _SlowBackend(Backend):
    name = "local"

    def __init__(self) -> None:
        self._cancelled: set[str] = set()

    def execute(self, job: BackendJob, progress=None) -> dict:
        time.sleep(_DUR)
        return {"value": 1.0, "wall_time_s": _DUR}

    def cancel(self, job_id: str) -> None:
        self._cancelled.add(job_id)

    def estimate_seconds(self, job: BackendJob) -> int:
        return 1


def _runner(tmp_path: Path) -> JobRunner:
    store = JobStore(db_path=tmp_path / "jobs.db")
    return JobRunner(
        store=store,
        backends={"local": _SlowBackend()},
        cache_root=tmp_path / "cache",
        max_workers=4,
    )


def _submit(runner: JobRunner, key: str):
    return _run_async(
        runner.submit(
            tool_name="relax_structure",
            input_payload={"composition": {"atoms": {"Cu": 4}}, "phase": "fcc"},
            cache_key=key,
            backend_name="local",
            seed=1,
        )
    )


def _wait_succeeded(runner: JobRunner, job_id: str, timeout: float) -> str:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        rec = runner.store.get(job_id)
        if rec is not None and rec.status in ("succeeded", "failed", "cancelled"):
            return rec.status
        time.sleep(0.02)
    return "timeout"


def test_submit_is_non_blocking(tmp_path):
    runner = _runner(tmp_path)
    t0 = time.monotonic()
    handle = _submit(runner, "k-nonblock")
    elapsed = time.monotonic() - t0
    # submit returned the handle WAY before the job's _DUR could elapse
    assert elapsed < _DUR / 2, f"submit blocked {elapsed:.3f}s (job is {_DUR}s)"
    assert handle.status == "queued"


def test_job_completes_in_background_on_persistent_loop(tmp_path):
    runner = _runner(tmp_path)
    handle = _submit(runner, "k-bg")
    # The submit loop is gone; only a *persistent* loop keeps _run_one
    # alive to completion. Pre-fix per-call loop would have cancelled it.
    assert _wait_succeeded(runner, handle.job_id, timeout=10.0) == "succeeded"
    assert runner.store.get(handle.job_id).result["value"] == 1.0


def test_independent_jobs_run_concurrently_not_serialised(tmp_path):
    runner = _runner(tmp_path)
    n = 4
    t0 = time.monotonic()
    handles = [_submit(runner, f"k-conc-{i}") for i in range(n)]
    submit_elapsed = time.monotonic() - t0
    # All N submits together must be far cheaper than even one job.
    assert submit_elapsed < _DUR, (
        f"{n} submits took {submit_elapsed:.3f}s — serialised "
        f"(pre-fix drain bug; expected << {_DUR}s)"
    )
    for h in handles:
        assert _wait_succeeded(runner, h.job_id, timeout=15.0) == "succeeded"
    total = time.monotonic() - t0
    # 4 jobs, 4 workers, ~_DUR each → concurrent finishes near _DUR, never
    # near n*_DUR. Generous ceiling absorbs scheduler/CI jitter.
    assert total < _DUR * (n - 1), (
        f"{n} jobs took {total:.3f}s — looks serialised "
        f"(serial would be ~{n * _DUR:.1f}s, concurrent ~{_DUR:.1f}s)"
    )
