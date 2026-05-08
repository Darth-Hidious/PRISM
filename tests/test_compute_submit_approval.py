"""Tests for the compute / compute_submit split.

After this PR: compute(action='submit') is removed from the unified
compute tool; submit becomes a standalone `compute_submit` tool with
`requires_approval=True`. The harness will prompt the user before each
call. This test file verifies the split is correct end-to-end.
"""
from unittest.mock import MagicMock, patch

import pytest

from app.tools.base import ToolRegistry
from app.tools.compute import (
    _compute,
    _compute_submit,
    create_compute_tools,
)


class TestRegistration:
    def test_both_tools_registered(self):
        reg = ToolRegistry()
        create_compute_tools(reg)
        names = [t.name for t in reg.list_tools()]
        assert "compute" in names
        assert "compute_submit" in names

    def test_compute_submit_requires_approval(self):
        reg = ToolRegistry()
        create_compute_tools(reg)
        cs = reg.get("compute_submit")
        assert cs.requires_approval is True

    def test_compute_does_not_require_approval(self):
        """Read-only ops must NOT require approval — agent polls
        compute(action='status') in tight loops; approving each one
        would break the UX."""
        reg = ToolRegistry()
        create_compute_tools(reg)
        c = reg.get("compute")
        assert c.requires_approval is False

    def test_submit_removed_from_compute_action_enum(self):
        reg = ToolRegistry()
        create_compute_tools(reg)
        c = reg.get("compute")
        actions = c.input_schema["properties"]["action"]["enum"]
        assert "submit" not in actions
        # The remaining 5 actions are read-only / idempotent
        assert set(actions) == {"list_gpus", "list_providers", "estimate", "status", "cancel"}

    def test_compute_submit_required_args(self):
        reg = ToolRegistry()
        create_compute_tools(reg)
        cs = reg.get("compute_submit")
        assert set(cs.input_schema["required"]) == {"image", "inputs"}


class TestComputeReadOnlyDispatcher:
    def test_missing_action(self):
        r = _compute()
        assert "error" in r
        assert "Missing 'action'" in r["error"]

    def test_unknown_action(self):
        r = _compute(action="bogus")
        assert "error" in r
        assert "Unknown action" in r["error"]

    def test_submit_action_redirects(self):
        """Old code calling compute(action='submit') gets a clear redirect
        to the new standalone tool, not a confusing 'unknown action' error."""
        r = _compute(action="submit", image="x", inputs={})
        assert "error" in r
        assert "compute_submit" in r["error"]
        assert "approval" in r["error"].lower() or "money" in r["error"].lower()

    def test_estimate_requires_image(self):
        with patch("app.tools.compute._get_client", return_value=MagicMock()):
            r = _compute(action="estimate")
        assert "error" in r
        assert "image" in r["error"]

    def test_status_requires_job_id(self):
        with patch("app.tools.compute._get_client", return_value=MagicMock()):
            r = _compute(action="status")
        assert "error" in r
        assert "job_id" in r["error"]

    def test_cancel_requires_job_id(self):
        with patch("app.tools.compute._get_client", return_value=MagicMock()):
            r = _compute(action="cancel")
        assert "error" in r
        assert "job_id" in r["error"]

    def test_list_gpus_dispatches(self):
        client = MagicMock()
        client.compute.list_gpus.return_value = [{"type": "A100"}]
        with patch("app.tools.compute._get_client", return_value=client):
            r = _compute(action="list_gpus")
        assert "error" not in r
        assert r["count"] == 1
        assert r["source"] == "marc27_compute_broker"

    def test_no_client_returns_clean_error(self):
        with patch("app.tools.compute._get_client", return_value=None):
            r = _compute(action="list_gpus")
        assert "error" in r
        assert "MARC27 platform not connected" in r["error"]


class TestComputeSubmit:
    def test_missing_image(self):
        r = _compute_submit()
        assert "error" in r
        assert "image" in r["error"]

    def test_missing_inputs(self):
        r = _compute_submit(image="vasp:6.5.0")
        assert "error" in r
        assert "inputs" in r["error"]

    def test_empty_inputs_dict_is_valid(self):
        """`inputs={}` is a valid value (some images need no inputs).
        We require the KEY to be present, not non-empty."""
        client = MagicMock()
        client.compute.submit.return_value = {"job_id": "job_123"}
        with patch("app.tools.compute._get_client", return_value=client):
            r = _compute_submit(image="x", inputs={})
        assert "error" not in r
        assert r["job"]["job_id"] == "job_123"

    def test_no_client(self):
        with patch("app.tools.compute._get_client", return_value=None):
            r = _compute_submit(image="x", inputs={})
        assert "error" in r
        assert "MARC27 platform not connected" in r["error"]

    def test_happy_path(self):
        client = MagicMock()
        client.compute.submit.return_value = {
            "job_id": "job_xyz",
            "status": "queued",
        }
        with patch("app.tools.compute._get_client", return_value=client):
            r = _compute_submit(
                image="vasp:6.5.0",
                inputs={"structure": "..."},
                gpu_type="A100-80GB",
                budget_max_usd=5.0,
                provider_preference="cheapest",
                timeout_seconds=7200,
                env_vars={"VASP_LICENSE": "..."},
            )
        assert "error" not in r
        # Verify all kwargs were forwarded to the SDK
        client.compute.submit.assert_called_once()
        call_kwargs = client.compute.submit.call_args.kwargs
        assert call_kwargs["image"] == "vasp:6.5.0"
        assert call_kwargs["budget_max_usd"] == 5.0
        assert call_kwargs["timeout_seconds"] == 7200
        assert call_kwargs["env_vars"] == {"VASP_LICENSE": "..."}
