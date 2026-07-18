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


class TestReadTailTruncationMarker:
    """VS1/F4: when a background-bash task log exceeds the 16k tail window,
    the leading bytes are dropped silently. The model has no idea an early
    error scrolled out. The fix prepends an explicit byte-count marker."""

    def test_marker_present_when_output_exceeds_tail_window(self, tmp_path):
        from app.tools.bash import _TASK_TAIL_BYTES, _read_tail

        log = tmp_path / "task.stdout.log"
        # Write strictly more than the tail window so truncation kicks in.
        # Put a sentinel near the END (the real tail) and a distinct sentinel
        # at the very START (which should be dropped from the tail body).
        early_sentinel = "EARLY_COMPILER_ERROR_at_start"
        late_sentinel = "FINAL_SIM_RESULT_at_end"
        body = ("x" * (_TASK_TAIL_BYTES + 4096)).encode()
        # Compose: [early sentinel][padding][late sentinel]
        log.write_bytes(
            early_sentinel.encode() + body + late_sentinel.encode()
        )

        text = _read_tail(log)

        assert text.startswith("[earlier output truncated"), (
            "truncated tail must begin with the marker: %r" % text[:80]
        )
        # The dropped count = total size - tail window. Here the padding alone
        # is 4096 bytes over the window; the early sentinel adds more. The
        # marker must announce SOME positive dropped byte count.
        assert "bytes before this point" in text, (
            "marker must announce how many bytes were dropped: %r" % text[:120]
        )
        # The real tail survives.
        assert late_sentinel in text, (
            "the actual tail content must survive: %r" % text[-80:]
        )
        # The dropped early sentinel is NOT in the returned body (it was before
        # the tail window). Strip the marker line first so we only inspect the
        # body. (The sentinel is non-numeric, so it can't sneak in via the byte
        # count.)
        body_only = text.split("\n", 1)[1] if "\n" in text else text
        assert early_sentinel not in body_only, (
            "dropped early content must not appear in the tail body"
        )

    def test_no_marker_when_output_fits_in_window(self, tmp_path):
        from app.tools.bash import _TASK_TAIL_BYTES, _read_tail

        log = tmp_path / "task.stderr.log"
        small = "just a few lines of output\n" * 3
        log.write_text(small)

        text = _read_tail(log)

        assert not text.startswith("[earlier"), (
            "small output must NOT carry a truncation marker: %r" % text[:80]
        )
        assert text == small, "small output returned verbatim"

    def test_marker_byte_count_matches_dropped_bytes(self, tmp_path):
        """The marker's byte count must equal size - max_bytes exactly."""
        from app.tools.bash import _TASK_TAIL_BYTES, _read_tail

        log = tmp_path / "exact.log"
        # Total size = window + exactly 1000 bytes.
        log.write_bytes(b"A" * (_TASK_TAIL_BYTES + 1000))

        text = _read_tail(log)
        assert "1000 bytes before this point" in text, (
            "marker must report exactly the dropped byte count: %r" % text[:120]
        )
