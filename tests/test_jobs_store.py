"""JobStore state machine."""

from __future__ import annotations

import pytest

from app.tools.simulation.mace.jobs.store import JobStore, JobStoreError


def test_create_and_get(tmp_path):
    s = JobStore(tmp_path / "jobs.db")
    s.create("J1", "relax_structure", {"composition": {"atoms": {"Fe": 50, "Ti": 50}}})
    rec = s.get("J1")
    assert rec is not None
    assert rec.status == "queued"
    assert rec.tool_name == "relax_structure"


def test_happy_path_transitions(tmp_path):
    s = JobStore(tmp_path / "jobs.db")
    s.create("J1", "relax_structure", {})
    for new in ("submitted", "running", "succeeded"):
        s.transition("J1", new)
    rec = s.get("J1")
    assert rec.status == "succeeded"
    assert rec.finished_at is not None


def test_invalid_transition_raises(tmp_path):
    s = JobStore(tmp_path / "jobs.db")
    s.create("J1", "relax_structure", {})
    with pytest.raises(JobStoreError):
        s.transition("J1", "succeeded")  # cannot skip from queued


def test_terminal_state_idempotent(tmp_path):
    s = JobStore(tmp_path / "jobs.db")
    s.create("J1", "relax_structure", {})
    for new in ("submitted", "running", "succeeded"):
        s.transition("J1", new)
    # Re-applying same status: no-op
    s.transition("J1", "succeeded")
    # Cancelling a terminal job: no-op
    s.transition("J1", "cancelled")
    rec = s.get("J1")
    assert rec.status == "succeeded"


def test_progress_update(tmp_path):
    s = JobStore(tmp_path / "jobs.db")
    s.create("J1", "relax_structure", {})
    s.transition("J1", "submitted")
    s.transition("J1", "running")
    s.update_progress("J1", 33.3, "step 5/15", 5, 15)
    rec = s.get("J1")
    assert rec.progress.percent == pytest.approx(33.3)
    assert rec.progress.step == 5
    assert "5/15" in rec.progress.message


def test_set_result_and_error(tmp_path):
    s = JobStore(tmp_path / "jobs.db")
    s.create("J1", "relax_structure", {})
    s.transition("J1", "submitted")
    s.transition("J1", "running")
    s.set_result("J1", {"energy_per_atom_eV": -8.1})
    s.transition("J1", "succeeded")
    assert s.get("J1").result["energy_per_atom_eV"] == -8.1

    s.create("J2", "relax_structure", {})
    s.transition("J2", "submitted")
    s.transition("J2", "running")
    s.set_error("J2", {"kind": "Boom", "message": "kaboom"})
    s.transition("J2", "failed")
    assert s.get("J2").error["kind"] == "Boom"


def test_list_filter(tmp_path):
    s = JobStore(tmp_path / "jobs.db")
    s.create("J1", "relax_structure", {})
    s.create("J2", "compute_elastic", {})
    s.transition("J2", "submitted")
    s.transition("J2", "running")
    s.transition("J2", "succeeded")
    succ = s.list(status_filter="succeeded")
    assert len(succ) == 1 and succ[0].job_id == "J2"
    queued = s.list(status_filter="queued")
    assert len(queued) == 1 and queued[0].job_id == "J1"


def test_cancel_from_queued(tmp_path):
    s = JobStore(tmp_path / "jobs.db")
    s.create("J1", "relax_structure", {})
    s.transition("J1", "cancelled")
    assert s.get("J1").status == "cancelled"


# ---------------------------------------------------------------------------
# A job whose owner process is gone must not read as "running" forever
# ---------------------------------------------------------------------------

def _fresh_store(tmp_path):
    from app.tools.simulation.mace.jobs.store import JobStore
    return JobStore(tmp_path / "jobs.db")


def test_running_records_its_owner_process(tmp_path):
    import os
    st = _fresh_store(tmp_path)
    st.create("J1", "md_equilibrate", {"T_K": 772})
    st.transition("J1", "submitted"); st.transition("J1", "running")
    assert st.get("J1").owner_pid == os.getpid()


def test_a_job_whose_owner_died_reads_as_interrupted(tmp_path):
    """2026-09-05: three 772 K MD jobs ran inside a tool server that was
    killed with its TUI. The store kept them 'running' at 1150/2000 steps
    for the rest of the evening, and every mace_get_job poll repeated it."""
    import os, subprocess
    st = _fresh_store(tmp_path)
    dead = subprocess.Popen(["/usr/bin/true"]); dead.wait()          # a pid that is certainly gone
    st.create("J2", "md_equilibrate", {"T_K": 772})
    st.transition("J2", "submitted"); st.transition("J2", "running", owner_pid=dead.pid)
    st.update_progress("J2", 57.5, "step 1150/2000", 1150, 2000)
    st.create("J3", "md_equilibrate", {"T_K": 772})                    # legacy row: no owner recorded
    st.transition("J3", "submitted"); st.transition("J3", "running", owner_pid=None)
    st.create("J4", "relax", {})
    st.transition("J4", "submitted"); st.transition("J4", "running")   # owned by THIS live process
    reaped = st.reap_orphans()
    assert set(reaped) == {"J2", "J3"}, reaped
    j2 = st.get("J2")
    assert j2.status == "interrupted" and j2.finished_at is not None
    assert "1150/2000" in str(j2.error) and "resubmit" in str(j2.error).lower(), j2.error
    assert st.get("J3").status == "interrupted"
    assert st.get("J4").status == "running", "a job owned by a live process is left alone"
    assert st.reap_orphans() == [], "idempotent"


def test_the_runner_reaps_on_start(tmp_path):
    import subprocess
    from app.tools.simulation.mace.jobs.runner import JobRunner
    st = _fresh_store(tmp_path)
    dead = subprocess.Popen(["/usr/bin/true"]); dead.wait()
    st.create("J5", "md_equilibrate", {}); st.transition("J5", "submitted"); st.transition("J5", "running", owner_pid=dead.pid)
    JobRunner(st, backends={}, cache_root=tmp_path / "cache")
    assert st.get("J5").status == "interrupted"
