"""Tests for the execute_bash tool."""

import time
from unittest.mock import patch

from app.tools import bash as bash_module
from app.tools.base import ToolRegistry
from app.tools.bash import (
    _execute_bash,
    _list_bash_tasks,
    _read_bash_task,
    _stop_bash_task,
    create_bash_tools,
)


class TestExecuteBash:
    def setup_method(self):
        with bash_module._BASH_TASKS_LOCK:
            for task in bash_module._BASH_TASKS.values():
                process = task.get("process")
                if process is not None:
                    try:
                        bash_module._terminate_process(process)
                        process.wait(timeout=1)
                    except Exception:
                        pass
            bash_module._BASH_TASKS.clear()

    def test_simple_command(self):
        result = _execute_bash(command='printf "hello"')
        assert result["success"] is True
        assert result["exit_code"] == 0
        assert result["stdout"] == "hello"

    def test_semantic_no_matches_is_not_error(self, tmp_path):
        sample = tmp_path / "sample.txt"
        sample.write_text("alpha\nbeta\n")
        with patch("app.tools.bash._ALLOWED_BASE", tmp_path.resolve()):
            result = _execute_bash(command='grep "gamma" sample.txt')
        assert result["success"] is True
        assert result["exit_code"] == 1
        assert result["return_code_interpretation"] == "No matches found"

    def test_blocks_command_substitution(self):
        result = _execute_bash(command='echo $(pwd)')
        assert result["success"] is False
        assert "not supported" in result["error"]

    def test_blocks_network_commands(self):
        result = _execute_bash(command="curl -I https://example.com")
        assert result["success"] is False
        assert "Network command" in result["error"]

    def test_blocks_paths_outside_project(self, tmp_path):
        with patch("app.tools.bash._ALLOWED_BASE", tmp_path.resolve()):
            result = _execute_bash(command="cat /etc/hosts")
        assert result["success"] is False
        assert "must stay within" in result["error"]

    def test_write_inside_project(self, tmp_path):
        output = tmp_path / "note.txt"
        with patch("app.tools.bash._ALLOWED_BASE", tmp_path.resolve()):
            result = _execute_bash(command='printf "ok" > note.txt')
        assert result["success"] is True
        assert output.read_text() == "ok"

    def test_timeout(self):
        result = _execute_bash(command="sleep 5", timeout=1)
        assert result["success"] is False
        assert result["exit_code"] == 124

    def test_blocks_unsupported_git_subcommands(self):
        result = _execute_bash(command="git commit -m test")
        assert result["success"] is False
        assert "git subcommand" in result["error"]

    def test_background_task_lifecycle(self, tmp_path):
        with patch("app.tools.bash._ALLOWED_BASE", tmp_path.resolve()):
            started = _execute_bash(
                command='printf "hello from background"',
                description="Write a greeting",
                run_in_background=True,
            )
            assert started["success"] is True
            task_id = started["task"]["task_id"]

            for _ in range(20):
                task = _read_bash_task(task_id)
                if task["task"]["status"] != "running":
                    break
                time.sleep(0.05)

            assert task["task"]["status"] == "completed"
            assert "hello from background" in task["task"]["stdout_tail"]

            listed = _list_bash_tasks()
            assert listed["count"] == 1
            assert listed["tasks"][0]["task_id"] == task_id

    def test_stop_background_task(self, tmp_path):
        with patch("app.tools.bash._ALLOWED_BASE", tmp_path.resolve()):
            started = _execute_bash(
                command="sleep 30",
                description="Long running sleep",
                run_in_background=True,
            )
            task_id = started["task"]["task_id"]

            stopped = _stop_bash_task(task_id)
            assert stopped["success"] is True
            assert stopped["task"]["status"] == "stopped"


class TestBashToolRegistration:
    """After Round 5 batch A: list_bash_tasks + read_bash_task collapsed
    into bash_task(action='list'|'read'). stop_bash_task stays standalone."""

    def test_tool_registered(self):
        registry = ToolRegistry()
        create_bash_tools(registry)
        tool = registry.get("execute_bash")
        assert tool.name == "execute_bash"
        assert tool.requires_approval is True
        # Read-only inspection — no approval (would break polling-loop UX)
        assert registry.get("bash_task").requires_approval is False
        # Destructive op stays separate, approval-gated
        assert registry.get("stop_bash_task").requires_approval is True

    def test_tool_in_bootstrap(self):
        from app.plugins.bootstrap import build_full_registry

        tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in tool_reg.list_tools()}
        assert "execute_bash" in names
        assert "bash_task" in names
        # Old names must be gone
        assert "list_bash_tasks" not in names
        assert "read_bash_task" not in names
