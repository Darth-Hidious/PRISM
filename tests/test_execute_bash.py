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

    def test_blocks_every_output_redirection_spelling(self, tmp_path):
        """`>|`, `&>` and `&>>` name a path exactly the way `>` does.

        The guard enumerated only `< > >> >&`, so those three walked past the
        path check entirely. Measured before the fix: `echo x >| /tmp/f` and
        `echo x &> /tmp/f` both WROTE outside the project and came back
        `success: true`, while the identical `>` form was blocked — the tool
        description promises "paths outside the project are blocked".
        """
        project = tmp_path / "project"
        project.mkdir()
        outside = tmp_path / "outside.txt"
        with patch("app.tools.bash._ALLOWED_BASE", project.resolve()):
            for operator in (">", ">>", ">|", "&>", "&>>"):
                result = _execute_bash(command=f"echo ESCAPED {operator} {outside}")
                assert result["success"] is False, (
                    f"redirection {operator!r} escaped the project sandbox: {result}"
                )
                assert "must stay within" in result["error"], operator
        assert not outside.exists(), "a blocked redirection must not have written"

    def test_fd_duplication_and_in_project_redirection_still_work(self, tmp_path):
        """The fix must not turn `2>&1` or an in-project `&>` into a refusal."""
        with patch("app.tools.bash._ALLOWED_BASE", tmp_path.resolve()):
            dup = _execute_bash(command="ls -d . 2>&1")
            assert dup["success"] is True, dup
            wrote = _execute_bash(command='printf "ok" &> note.txt')
            assert wrote["success"] is True, wrote
        assert (tmp_path / "note.txt").read_text() == "ok"

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


class TestRmSafeguard:
    """The guard judges what a command DOES, not how it is spelled.

    Measured before this class existed: of twenty catastrophic shapes only the
    literal `rm -rf /` was refused. `/bin/rm -rf /`, `env rm -rf /`,
    `echo / | xargs rm -rf`, `find . -exec rm -rf / \\;` and `rm -rf .` all
    passed validation, because the guard matched the first token literally
    and only asked whether paths stayed inside the project.
    """

    @staticmethod
    def _verdict(command: str, base) -> str | None:
        with patch("app.tools.bash._ALLOWED_BASE", base.resolve()):
            return bash_module._validate_command(command)

    def test_rm_never_targets_the_project_root_or_its_record(self, tmp_path):
        (tmp_path / "build").mkdir()
        (tmp_path / ".git").mkdir()
        (tmp_path / ".prism").mkdir()
        for command in (
            "rm -rf .",
            "rm -rf ./",
            "rm -rf *",
            "rm -r -f .",
            "rm --recursive --force .",
            "rm -fr .git",
            "rm .git/HEAD",
            "rm -rf .prism",
            f"rm -rf {tmp_path}",
        ):
            error = self._verdict(command, tmp_path)
            assert error is not None, command
            assert "project root" in error or ".git" in error or ".prism" in error, (command, error)
        assert self._verdict("rm -rf build", tmp_path) is None
        assert self._verdict("rm build/a.o", tmp_path) is None

    def test_the_program_name_is_judged_not_its_spelling(self, tmp_path):
        for command in (
            "/bin/rm -rf /",
            "/bin/bash -c 'rm -rf /'",
            "/usr/bin/sudo rm -rf /",
            "/bin/rm -rf .",
        ):
            assert self._verdict(command, tmp_path) is not None, command

    def test_wrappers_are_peeled_to_the_command_they_run(self, tmp_path):
        (tmp_path / "src").mkdir()
        for command in (
            "env rm -rf /",
            "env -i FOO=1 rm -rf /",
            "command rm -rf /",
            "nice -n 5 rm -rf /",
            "timeout 5 rm -rf /",
            "time rm -rf /",
            "env bash -c 'rm -rf /'",
        ):
            assert self._verdict(command, tmp_path) is not None, command
        assert self._verdict("env FOO=1 ls src", tmp_path) is None
        assert self._verdict("timeout 30 ls src", tmp_path) is None

    def test_xargs_cannot_feed_a_mutating_command(self, tmp_path):
        for command in (
            "echo / | xargs rm -rf",
            "ls | xargs -I{} rm -rf {}",
            "ls | xargs -0 mv -t /tmp",
        ):
            error = self._verdict(command, tmp_path)
            assert error is not None, command
            assert "xargs" in error, (command, error)
        assert self._verdict("ls | xargs wc -l", tmp_path) is None

    def test_find_delete_and_exec_obey_the_rm_rules(self, tmp_path):
        (tmp_path / "build").mkdir()
        (tmp_path / "src").mkdir()
        for command in (
            "find . -delete",
            "find . -name '*.o' -delete",
            "find . -exec rm -rf / \\;",
            "find . -exec rm -rf {} +",
            "find src -exec bash -c 'rm -rf /' \\;",
            "find build -execdir rm -rf / \\;",
        ):
            assert self._verdict(command, tmp_path) is not None, command
        assert self._verdict("find build -name '*.o' -delete", tmp_path) is None
        assert self._verdict("find src -exec grep -l x {} +", tmp_path) is None
        assert self._verdict("find . -name '*.rs'", tmp_path) is None

    def test_awk_cannot_shell_out(self, tmp_path):
        (tmp_path / "x").write_text("a b\n")
        assert self._verdict("awk 'BEGIN{system(\"rm -rf /\")}' x", tmp_path) is not None
        assert self._verdict("awk '{print $1 | \"sh\"}' x", tmp_path) is not None
        assert self._verdict("awk '{print $1}' x", tmp_path) is None

    def test_disk_wipers_are_not_shell_commands_here(self, tmp_path):
        for command in (
            "dd if=/dev/zero of=/dev/disk0",
            "diskutil eraseDisk JHFS+ X disk0",
            "shred -u notes.txt",
            "mkfs.ext4 /dev/sdb",
        ):
            assert self._verdict(command, tmp_path) is not None, command

    def test_a_refusal_says_what_to_do_instead(self, tmp_path):
        error = self._verdict("rm -rf .", tmp_path)
        assert error is not None
        assert "instead" in error.lower() or "name the" in error.lower(), error
