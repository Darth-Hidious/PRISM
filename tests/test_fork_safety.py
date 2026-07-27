"""fork() safety of the execution tools, and the honesty of a signal death.

THE DEFECT THIS PINS DOWN
    A materials search loads Apple's Network.framework, which registers a
    pthread_atfork child handler. From then on every fork() in the process
    SIGSEGVs inside that handler before it can exec:

        fork -> _pthread_atfork_child_handlers -> nw_settings_child_has_forked
             -> nw_path_release_globals -> NEFlowDirectorDestroy -> SIGSEGV

    So every execute_bash / execute_python call after a search returned
    exit_code -11 with empty stdout and empty stderr -- a failure with no
    diagnostic at all. This is the owner's "it fails constantly".

WHY THIS FILE OWNS ITS OWN SEARCH
    The old reproduction needed two test files in the wrong order, and the
    alphabetical default happened to put them in the right one, which hid the
    bug completely (`pytest tests/` green, the same files reversed 19 red).
    Every test here triggers the poison itself, in-process, through a
    module-scoped fixture. There is no ordering under which it can pass
    vacuously.
"""

from __future__ import annotations

import subprocess
import time

import pytest

from app.tools import spawn
from app.tools.bash import _execute_bash, _read_bash_task, _stop_bash_task
from app.tools.code import _execute_python

_SEGV = -11


@pytest.fixture(scope="module")
def poisoned_process():
    """Run a real materials search, the way the agent does, in this process.

    Not a mock: only the genuine search loads the framework whose atfork
    handler is the fault. If the search cannot run (no network), the
    precondition for the whole file is absent and we say so rather than
    reporting a green that proves nothing.
    """
    from app.tools.data import _search_materials

    result = _search_materials(elements=["Ti", "Al"], limit=3)
    if result.get("error") or not result.get("count"):
        pytest.skip(
            "materials search did not return results "
            f"({result.get('error') or 'empty'}) — without it the "
            "Network.framework atfork handler is never installed and this "
            "file cannot exercise what it exists to test"
        )
    return result


def _assert_not_fork_death(result: dict, tool: str) -> None:
    assert result.get("exit_code") != _SEGV, (
        f"{tool} died at SIGSEGV after a materials search: the spawn took "
        f"fork() and crashed in Network.framework's atfork handler. "
        f"stdout={result.get('stdout')!r} stderr={result.get('stderr')!r}"
    )


class TestToolsSurviveAMaterialsSearch:
    def test_execute_bash_still_runs(self, poisoned_process):
        result = _execute_bash(command='printf "still alive"')
        _assert_not_fork_death(result, "execute_bash")
        assert result["success"] is True
        assert result["exit_code"] == 0
        assert result["stdout"] == "still alive"

    def test_execute_python_still_runs(self, poisoned_process):
        result = _execute_python(code='print("still alive")')
        _assert_not_fork_death(result, "execute_python")
        assert result["success"] is True
        assert result["exit_code"] == 0
        assert "still alive" in result["stdout"]

    def test_background_bash_still_runs_and_stops(self, poisoned_process):
        """Covers the piece with no second chance: a background task must land
        in its own process group, or the group kill behind stop_bash_task has
        nothing to signal and the command outlives PRISM."""
        started = _execute_bash(command="sleep 30", run_in_background=True)
        assert started["success"] is True
        task_id = started["task"]["task_id"]

        stopped = _stop_bash_task(task_id)
        assert stopped["success"] is True, stopped.get("error")
        assert stopped["task"]["status"] == "stopped"

        for _ in range(40):
            if _read_bash_task(task_id)["task"]["status"] == "stopped":
                break
            time.sleep(0.05)
        assert _read_bash_task(task_id)["task"]["status"] == "stopped"


class TestSignalDeathIsReported:
    """A spawn killed by a signal must name the signal and admit the output is
    gone. `success: False` with two empty strings is the other half of this
    defect: it is indistinguishable from a command that printed nothing."""

    def test_bash_signal_death_names_the_signal(self, poisoned_process):
        result = _execute_bash(command="kill -9 $$")

        assert result["success"] is False
        assert result["exit_code"] == -9
        error = result.get("error", "")
        assert "SIGKILL" in error, f"signal not named: {error!r}"
        assert "lost" in error.lower(), f"lost output not admitted: {error!r}"

    def test_python_signal_death_names_the_signal(self, poisoned_process):
        result = _execute_python(
            code="import os, signal; os.kill(os.getpid(), signal.SIGSEGV)"
        )

        assert result["success"] is False
        assert result["exit_code"] == _SEGV
        error = result.get("error", "")
        assert "SIGSEGV" in error, f"signal not named: {error!r}"
        assert "lost" in error.lower(), f"lost output not admitted: {error!r}"

    def test_background_signal_death_names_the_signal(self, poisoned_process):
        started = _execute_bash(command="kill -9 $$", run_in_background=True)
        task_id = started["task"]["task_id"]

        for _ in range(40):
            task = _read_bash_task(task_id)["task"]
            if task["status"] != "running":
                break
            time.sleep(0.05)

        assert task["status"] == "failed"
        assert task["exit_code"] == -9
        assert "SIGKILL" in task.get("error", "")


class TestSpawnHelperCannotSilentlyRegress:
    """The previous fix carried a docstring saying it used posix_spawn and did
    not — nothing checked, so it stayed inert through a release. These assert
    the conditions instead of describing them."""

    def test_fork_forcing_kwargs_are_rejected(self):
        import os

        for kwargs in (
            {"preexec_fn": os.setsid},
            {"close_fds": True},
            {"start_new_session": True},
            {"pass_fds": (3,)},
            {"process_group": 0},
        ):
            with pytest.raises(TypeError, match="fork"):
                spawn.run(["/bin/echo", "x"], **kwargs)
            with pytest.raises(TypeError, match="fork"):
                spawn.popen(["/bin/echo", "x"], **kwargs)

    def test_underlying_popen_meets_the_posix_spawn_conditions(self, monkeypatch):
        """CPython takes the posix_spawn fast path only if close_fds is False
        (macOS has no POSIX_SPAWN_CLOSEFROM), cwd is None, and there is no
        preexec_fn / start_new_session. Assert what we hand to Popen."""
        seen: dict = {}
        real_popen = subprocess.Popen

        class Recorder(real_popen):  # type: ignore[misc, valid-type]
            def __init__(self, args, **kwargs):
                seen["args"] = args
                seen["kwargs"] = kwargs
                super().__init__(args, **kwargs)

        monkeypatch.setattr(subprocess, "Popen", Recorder)
        proc = spawn.popen(
            ["/bin/echo", "x"],
            cwd="/tmp",
            new_session=True,
            stdout=subprocess.PIPE,
        )
        proc.communicate(timeout=30)

        assert seen["kwargs"]["close_fds"] is False
        assert seen["kwargs"].get("cwd") is None
        assert seen["kwargs"].get("preexec_fn") is None
        assert seen["kwargs"].get("start_new_session") in (None, False)
        # cwd and setsid moved into the trampoline, which runs after exec.
        assert seen["args"][0].endswith("python3") or "python" in seen["args"][0]
        assert "/tmp" in seen["args"]

    def test_unrunnable_command_says_so_instead_of_tracebacking(self):
        """The trampoline is a process the caller never asked for. When exec
        fails it must not leak its own Python traceback as the diagnosis."""
        result = spawn.run(
            ["/bin/does-not-exist", "arg"], capture_output=True, text=True
        )

        assert result.returncode == 127
        assert "/bin/does-not-exist" in result.stderr
        assert "Traceback" not in result.stderr

    def test_process_group_wait_kills_rather_than_abandons(self):
        """If the group never appears, the caller gets an error and PRISM is
        left holding no untracked, unkillable child."""
        # new_session=False, so this child never becomes a group leader and
        # the wait is guaranteed to hit its deadline.
        proc = spawn.popen(["/bin/sleep", "45"], stdout=subprocess.DEVNULL)
        try:
            with pytest.raises(RuntimeError, match="process group"):
                spawn._await_own_process_group(proc, timeout=0.0)
            assert proc.poll() is not None, "child was abandoned, not killed"
        finally:
            if proc.poll() is None:  # belt and braces; the assert above owns it
                proc.kill()
                proc.wait()

    def test_helper_never_leaks_an_inheritable_fd(self, poisoned_process):
        """Network.framework leaves inheritable sockets behind that close_fds
        would otherwise have caught. The trampoline closes them explicitly."""
        import os

        leaked = os.open(__file__, os.O_RDONLY)
        os.set_inheritable(leaked, True)
        try:
            result = _execute_python(
                code=(
                    "import os\n"
                    "stray = []\n"
                    "for name in os.listdir('/dev/fd'):\n"
                    "    if not name.isdigit():\n"
                    "        continue\n"
                    "    fd = int(name)\n"
                    "    if fd <= 2:\n"
                    "        continue\n"
                    "    try:\n"
                    "        os.fstat(fd)\n"
                    "    except OSError:\n"
                    "        continue\n"
                    "    stray.append(fd)\n"
                    "print(stray)\n"
                )
            )
        finally:
            os.close(leaked)

        assert result["success"] is True, result
        assert result["stdout"].strip() == "[]", (
            f"descriptors leaked into model-authored code: {result['stdout']!r}"
        )
