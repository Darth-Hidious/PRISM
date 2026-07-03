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
