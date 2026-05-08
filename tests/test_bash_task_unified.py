"""Tests for the unified `bash_task` tool.

After Round 5 batch A: list_bash_tasks + read_bash_task collapsed into
bash_task(action='list'|'read'). stop_bash_task stays standalone because
it's destructive (requires_approval=True) and per-action approval gating
isn't supported in the harness yet.
"""
import pytest
from unittest.mock import patch

from app.tools.base import ToolRegistry
from app.tools.bash import _bash_task, create_bash_tools


class TestRegistration:
    def test_bash_task_registered(self):
        reg = ToolRegistry()
        create_bash_tools(reg)
        names = [t.name for t in reg.list_tools()]
        assert "bash_task" in names

    def test_old_names_removed(self):
        reg = ToolRegistry()
        create_bash_tools(reg)
        names = [t.name for t in reg.list_tools()]
        assert "list_bash_tasks" not in names
        assert "read_bash_task" not in names

    def test_destructive_op_isolated(self):
        """stop_bash_task stays separate (destructive, approval-gated)."""
        reg = ToolRegistry()
        create_bash_tools(reg)
        names = [t.name for t in reg.list_tools()]
        assert "stop_bash_task" in names
        assert reg.get("stop_bash_task").requires_approval is True

    def test_execute_bash_kept(self):
        """execute_bash is a different concept (runner) — stays standalone."""
        reg = ToolRegistry()
        create_bash_tools(reg)
        names = [t.name for t in reg.list_tools()]
        assert "execute_bash" in names

    def test_action_enum(self):
        reg = ToolRegistry()
        create_bash_tools(reg)
        tool = reg.get("bash_task")
        actions = tool.input_schema["properties"]["action"]["enum"]
        assert set(actions) == {"list", "read"}

    def test_no_approval_required_for_read_only(self):
        reg = ToolRegistry()
        create_bash_tools(reg)
        # bash_task is read-only — approval would defeat the polling-loop UX
        assert reg.get("bash_task").requires_approval is False


class TestDispatch:
    def test_missing_action(self):
        result = _bash_task()
        assert "error" in result
        assert "Missing 'action'" in result["error"]

    def test_unknown_action(self):
        result = _bash_task(action="bogus")
        assert "error" in result
        assert "Unknown action" in result["error"]

    def test_action_list_no_args(self):
        result = _bash_task(action="list")
        assert "tasks" in result
        assert "count" in result
        assert isinstance(result["tasks"], list)

    def test_action_read_requires_task_id(self):
        result = _bash_task(action="read")
        assert "error" in result
        assert "task_id" in result["error"]

    def test_action_read_unknown_task(self):
        """When task_id doesn't exist, the underlying _read_bash_task
        returns its own error shape (success=False), not the dispatcher's."""
        result = _bash_task(action="read", task_id="nonexistent_xyz")
        assert result.get("success") is False
        assert "Unknown bash task" in result.get("error", "")
