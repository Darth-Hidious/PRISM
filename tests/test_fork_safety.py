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

THE TWO PROOFS, AND WHAT EACH IS WORTH
    `poisoned_process` reproduces the real thing: a real search, then the real
    call site, in one process. It is only meaningful on macOS -- the atfork
    handler is Network.framework's -- and only when the search can actually
    reach the network, so it skips rather than lie.

    `no_fork` proves the same call sites a second way, without a network and
    on any platform: it replaces CPython's fork+exec entry point
    (subprocess._fork_exec) so that "this site would have forked" becomes a
    deterministic exception instead of a platform-dependent crash. That is
    what makes this file worth running on the Linux CI runner, where the
    macOS-only crash cannot happen at all.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

from app.tools import spawn
from app.tools.bash import _execute_bash, _read_bash_task, _stop_bash_task
from app.tools.code import _execute_python

_SEGV = -11
_REPO_ROOT = Path(__file__).resolve().parents[1]


class ForkAttempted(RuntimeError):
    """Raised in place of the fork() a migrated call site must never reach."""


@pytest.fixture
def no_fork():
    """Make any fork()-based spawn inside the test fail loudly.

    CPython reaches fork() only through `subprocess._fork_exec`; the
    posix_spawn fast path never touches it (subprocess.py:1874 vs 1919).
    Replacing it therefore separates the two paths exactly, with no reliance
    on the macOS-only crash -- see `test_the_fork_detector_is_not_vacuous`,
    which fails this fixture's own premise if the hook ever stops working.

    It also pins _HAVE_POSIX_SPAWN_CLOSEFROM False, because otherwise this
    would prove LESS on Linux than on macOS and quietly look the same. macOS
    has no posix_spawn_file_actions_addclosefrom_np, so close_fds=True (the
    default, and what capture_output=True implies) rules out posix_spawn
    there; glibc >= 2.34 has it, so on Linux those same call sites would
    posix_spawn even unmigrated and three of the exercises below would pass
    while still broken. Pinning it reproduces the macOS constraint set exactly.
    The migrated sites are unaffected -- spawn.run passes close_fds=False.

    Sites invoking a bare command name (`git`, `hf`, `uv`) fork on every
    platform regardless: subprocess.py:1860 requires os.path.dirname(executable).
    """
    if not hasattr(subprocess, "_fork_exec"):
        pytest.skip("CPython build has no subprocess._fork_exec to intercept")

    real_fork_exec = subprocess._fork_exec
    real_closefrom = subprocess._HAVE_POSIX_SPAWN_CLOSEFROM

    def _refuse(args, *rest, **kwargs):
        raise ForkAttempted(f"this call site forked: {args!r}")

    subprocess._fork_exec = _refuse
    subprocess._HAVE_POSIX_SPAWN_CLOSEFROM = False
    try:
        yield
    finally:
        subprocess._fork_exec = real_fork_exec
        subprocess._HAVE_POSIX_SPAWN_CLOSEFROM = real_closefrom


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

    def test_the_trampoline_never_appears_in_a_callers_error(self):
        """Callers report failures by interpolating the exception -- _sidecar
        answers {"error": f"...: {exc}"}. If `cmd` still named the trampoline,
        "the venv died with SIGSEGV" would be replaced by 600 characters of
        this module's own bootstrap source, which is the same kind of
        undiagnosable output the whole file exists to eliminate."""
        real = ["/bin/sh", "-c", "exit 3"]

        with pytest.raises(subprocess.CalledProcessError) as failed:
            spawn.run(real, check=True, capture_output=True)
        assert failed.value.cmd == real
        assert "os.execvp" not in str(failed.value)

        with pytest.raises(subprocess.TimeoutExpired) as timed_out:
            spawn.run(["/bin/sleep", "5"], timeout=0.2, capture_output=True)
        assert timed_out.value.cmd == ["/bin/sleep", "5"]
        assert "os.execvp" not in str(timed_out.value)

        ok = spawn.run(real[:2] + ["exit 0"], capture_output=True)
        assert ok.args == real[:2] + ["exit 0"]

        proc = spawn.popen(["/bin/sleep", "5"], stdout=subprocess.DEVNULL)
        try:
            assert proc.args == ["/bin/sleep", "5"]
        finally:
            proc.kill()
            proc.wait(timeout=5)

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


# ---------------------------------------------------------------------------
# The rest of app/: every other place PRISM starts a process.
#
# Each exercise below drives ONE migrated call site through its real public
# entry point and asserts on something the site actually produces. That matters
# because all of these sites swallow their own failures -- a crashed spawn
# comes back as "unknown", False, "" or a plausible-looking error string, never
# as an exception. Asserting "it did not raise" would pass on every one of
# them while they were still broken.
# ---------------------------------------------------------------------------


def _fake_on_path(tmp_path: Path, monkeypatch, name: str, body: str = "") -> Path:
    """Put an executable stub first on PATH. Returns the file it records argv into.

    The recorded argv is what most of these exercises assert on. It is the one
    thing only a spawn that actually reached exec can produce: a fork that dies
    in the atfork handler, and a fork the no_fork fixture refuses, both leave
    this file absent.
    """
    bindir = tmp_path / "bin"
    bindir.mkdir(parents=True, exist_ok=True)
    calls = tmp_path / f"{name}-calls.log"
    stub = bindir / name
    stub.write_text(f'#!/bin/sh\necho "$@" >> "{calls}"\n{body}')
    stub.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bindir}{os.pathsep}{os.environ['PATH']}")
    return calls


# What the stub `hf` prints: a status word for _parse_status, and a job URL on
# the last line for _parse_job_id.
_FAKE_HF_OUTPUT = "echo completed\necho https://huggingface.co/jobs/fakejob123\n"


def _exercise_git_provenance(tmp_path, monkeypatch) -> None:
    """app/tools/simulation/mace/ids.py — the commit stamped on every MACE job."""
    from app.tools.simulation.mace import ids

    ids.git_sha.cache_clear()
    sha = ids.git_sha(str(_REPO_ROOT))

    assert re.fullmatch(r"[0-9a-f]{7,40}", sha), (
        f"git_sha() answered {sha!r}. git never ran, and this function turns "
        f"that into the string 'unknown', so the failure lands in the "
        f"provenance record looking like an answer instead of an error."
    )
    # git_dirty shares ids._git with git_sha, so the assertion above covers
    # both spawns; call it anyway to keep the second entry point exercised.
    assert isinstance(ids.git_dirty(str(_REPO_ROOT)), bool)


def _exercise_sidecar_provisioning(tmp_path, monkeypatch) -> None:
    """app/tools/_sidecar.py — `python -m venv` then `pip install` for the science venv."""
    from app.tools import _sidecar

    monkeypatch.setattr(_sidecar, "SIDECAR_VENV", tmp_path / "venv-sci")
    monkeypatch.setattr(_sidecar, "find_base_python", lambda: sys.executable)
    # Nothing to install, so pip refuses in its own words. Keeps this offline
    # and ~1s while still running both real spawns end to end.
    monkeypatch.setattr(_sidecar, "SIDECAR_PACKAGES", [])

    err = _sidecar.ensure_sidecar(install=True)

    assert (tmp_path / "venv-sci" / "bin" / "python3").exists(), (
        f"`python -m venv` never ran, so the first spawn died; ensure_sidecar "
        f"reported it as {err!r}"
    )
    assert err and "requirement" in err.lower(), (
        f"expected pip's own complaint about an empty install list, got {err!r}"
    )


def _exercise_sidecar_server(tmp_path, monkeypatch) -> None:
    """app/tools/_sidecar.py — the long-lived `app.sidecar_server` process."""
    from app.tools import _sidecar

    monkeypatch.setattr(_sidecar, "_sidecar_python", lambda: Path(sys.executable))
    handle = _sidecar._SidecarProcess()

    err = handle._spawn()
    assert err is None, err
    proc = handle._proc
    assert proc is not None

    try:
        deadline = time.time() + 2.0
        while time.time() < deadline and proc.poll() is None:
            time.sleep(0.05)
        assert proc.poll() != _SEGV, (
            "the sidecar server died at SIGSEGV. Every proxied pyiron/pycalphad "
            "tool call then reports 'science sidecar timed out', which names "
            "the wrong cause entirely."
        )
    finally:
        proc.kill()
        proc.wait(timeout=5)


def _exercise_pyiron_auto_provision(tmp_path, monkeypatch) -> None:
    """app/tools/simulation/bridge.py — pip install pyiron on the first sim tool call."""
    from app.tools.simulation import bridge

    monkeypatch.setattr(bridge, "_AUTO_PROVISION_ATTEMPTED", False)
    # `pip install --help` exits 0 and touches no network. What is under test
    # is that the spawn completes and its status comes back, not what pip does.
    monkeypatch.setattr(bridge, "_PYIRON_SPEC", ["--help"])

    assert bridge._try_auto_provision() is True, (
        "the pip spawn never completed. _try_auto_provision swallows that and "
        "returns False, which the caller reports to the model as "
        "'automatic installation failed (offline?)' — the wrong diagnosis."
    )


def _exercise_hf_launch_and_poll(tmp_path, monkeypatch) -> None:
    """mace/backends/hf_jobs.py — `hf jobs uv run` (launch) and `hf jobs status` (poll)."""
    from app.tools.simulation.mace.backends import hf_jobs

    calls = _fake_on_path(tmp_path, monkeypatch, "hf", _FAKE_HF_OUTPUT)
    monkeypatch.setattr(hf_jobs, "get_hf_token", lambda: "not-a-real-token")
    # An empty results repo stops execute() on a deterministic error the moment
    # both spawns are done, so this needs no HF account and no network.
    monkeypatch.setattr(hf_jobs, "get_results_repo", lambda: "")

    # SEPARATE, PRE-EXISTING BUG, not this branch's to fix: PAYLOAD_MODULES
    # still names `mace_mcp.payloads.*`, but those files were vendored to
    # app/tools/simulation/mace/payloads/ and the map was never re-pointed. So
    # _materialise_payload raises ModuleNotFoundError and execute() dies before
    # it reaches either spawn -- which also means the launch spawn is currently
    # unreachable in production. Stub that one call so the spawns under test
    # still run. The assert makes the stub self-deleting: it fails the day the
    # drift is fixed, so this workaround cannot quietly outlive the bug.
    assert hf_jobs.PAYLOAD_MODULES["relax_structure"].startswith("mace_mcp."), (
        "PAYLOAD_MODULES no longer points at the absent `mace_mcp` package, so "
        "the drift this stub works around is fixed — delete the stub."
    )

    def _stub_payload(self, tool: str, tmpd: Path) -> Path:
        path = tmpd / f"{tool}.py"
        path.write_text("# stub; the fake `hf` CLI never runs it\n")
        return path

    monkeypatch.setattr(hf_jobs.HfJobsBackend, "_materialise_payload", _stub_payload)

    # A healthy _poll returns on its FIRST status read, before it ever sleeps,
    # so a long interval costs the green path nothing. It costs the RED path a
    # great deal: the thread abandoned below would otherwise re-spawn `hf`
    # every 10ms for the rest of the session.
    backend = hf_jobs.HfJobsBackend(poll_interval_s=5.0)
    job = hf_jobs.BackendJob(
        tool_name="relax_structure",
        input_payload={"composition": {"atoms": {"Ti": 1.0}}},
        cache_key="fork-safety-probe",
    )

    # Bounded, because _poll cannot fail fast on its own: `except Exception:
    # status = "unknown"` swallows both a SIGSEGV and the no_fork fixture's
    # refusal, and "unknown" is not a terminal status, so a regression here
    # spins to _poll's own deadline -- tool timeout + 300s, over 2100s -- before
    # raising anything. That is half an hour of a shared two-slot runner per
    # test to learn what this bound reports in seconds.
    outcome: list[BaseException | None] = []

    def _execute() -> None:
        try:
            backend.execute(job)
            outcome.append(None)
        except BaseException as exc:  # noqa: BLE001 - inspected below, not swallowed
            outcome.append(exc)

    worker = threading.Thread(target=_execute, daemon=True)
    worker.start()
    worker.join(60)
    assert not worker.is_alive(), (
        "execute() had not returned after 60s. Either the launch spawn never "
        "produced a job id, or _poll is reading 'unknown' forever because its "
        "status spawn is dying."
    )

    failure = outcome[0]
    assert isinstance(failure, RuntimeError) and "MACE_MCP_RESULTS_REPO" in str(failure), (
        f"expected execute() to stop at the results-repo check, which is the "
        f"first thing past both spawns; got {failure!r}"
    )

    log = calls.read_text()
    assert "jobs uv run" in log, f"the launch spawn never reached `hf`: {log!r}"
    assert "jobs status fakejob123" in log, f"the poll spawn never reached `hf`: {log!r}"


def _exercise_hf_cancel_and_logs(tmp_path, monkeypatch) -> None:
    """mace/backends/hf_jobs.py — `hf jobs cancel` and `hf jobs logs`."""
    from app.tools.simulation.mace.backends import hf_jobs

    calls = _fake_on_path(tmp_path, monkeypatch, "hf", _FAKE_HF_OUTPUT)

    backend = hf_jobs.HfJobsBackend()
    backend._active["cache-key"] = "fakejob123"
    backend.cancel("cache-key")
    assert "jobs cancel fakejob123" in calls.read_text(), (
        "cancel never reached `hf`, and it swallows the failure, so a GPU job "
        "the user asked to stop keeps billing with nothing reported."
    )

    tail = hf_jobs._fetch_logs("fakejob123")
    assert "completed" in tail, (
        f"`hf jobs logs` produced nothing: {tail!r}. _fetch_logs answers '' on "
        f"failure, so a dead job is explained with an empty tail and no hint "
        f"that fetching the log is what actually broke."
    )


def _exercise_update_install_detection(tmp_path, monkeypatch) -> None:
    """app/update.py — `uv tool list` AND `pipx list` behind detect_install_method()."""
    from app import update

    # detect_install_method returns on the first branch that matches, so a `uv`
    # reporting prism-platform would leave the pipx spawn unexercised and this
    # test would pass with that line reverted to bare subprocess. Have `uv`
    # answer WITHOUT prism-platform: detection falls through, and one call
    # drives both spawns.
    uv_calls = _fake_on_path(tmp_path, monkeypatch, "uv", "echo some-other-tool\n")
    pipx_calls = _fake_on_path(tmp_path, monkeypatch, "pipx", "echo prism-platform\n")

    update.detect_install_method()

    # Assert the commands REACHED exec, not what detect_install_method returned.
    # The return value is gated on a production timeout=5 this test does not
    # own: on a loaded machine a slow-but-successful spawn also returns
    # something else, and then a timing problem would be reported as a fork
    # death. What only a fork death can do is leave these files unwritten.
    assert uv_calls.exists() and "tool list" in uv_calls.read_text(), (
        "`uv tool list` never reached the shell. detect_install_method "
        "swallows that and falls through to another install method, printing "
        "an upgrade command that will not work."
    )
    assert pipx_calls.exists() and "list --short" in pipx_calls.read_text(), (
        "`pipx list` never reached the shell — same swallow, same wrong "
        "upgrade command."
    )


def _exercise_update_run_upgrade(tmp_path, monkeypatch) -> None:
    """app/update.py — the spawn that actually runs the upgrade."""
    from app import update

    # A bare command name, like the real `uv` / `pipx` / `pip` this splits.
    # Not an absolute path: subprocess.py:1860 refuses posix_spawn for an
    # executable with no dirname, so an absolute path here would be an easier
    # case than production ever is.
    calls = _fake_on_path(tmp_path, monkeypatch, "prism-fake-upgrade")
    monkeypatch.setattr(
        update, "upgrade_command", lambda method=None: "prism-fake-upgrade --self"
    )

    ok = update.run_upgrade(method="pip")

    assert calls.exists(), (
        "the upgrade spawn never reached exec. run_upgrade reports False for "
        "that, which is indistinguishable from an upgrade that ran and failed."
    )
    assert ok is True, f"upgrade spawn ran but reported failure; argv was {calls.read_text()!r}"


_MIGRATED_SITES = [
    ("mace/ids.py:git_sha+git_dirty", _exercise_git_provenance),
    ("_sidecar.py:ensure_sidecar", _exercise_sidecar_provisioning),
    ("_sidecar.py:_SidecarProcess._spawn", _exercise_sidecar_server),
    ("simulation/bridge.py:_try_auto_provision", _exercise_pyiron_auto_provision),
    ("hf_jobs.py:execute+_poll", _exercise_hf_launch_and_poll),
    ("hf_jobs.py:cancel+_fetch_logs", _exercise_hf_cancel_and_logs),
    ("update.py:detect_install_method", _exercise_update_install_detection),
    ("update.py:run_upgrade", _exercise_update_run_upgrade),
]


@pytest.mark.parametrize(
    "exercise", [site for _, site in _MIGRATED_SITES], ids=[n for n, _ in _MIGRATED_SITES]
)
class TestEveryOtherSpawnSiteInApp:
    def test_survives_a_materials_search(
        self, exercise, poisoned_process, tmp_path, monkeypatch
    ):
        """The real reproduction: macOS, real search, real call site, one process."""
        exercise(tmp_path, monkeypatch)

    def test_never_forks(self, exercise, no_fork, tmp_path, monkeypatch):
        """The portable half: no network, no macOS, no fork() reached."""
        exercise(tmp_path, monkeypatch)


class TestTheForkDetectorItself:
    """A guard that reports OK when the thing is broken is worse than none.
    These two say what `no_fork` is worth, in both directions."""

    def test_the_fork_detector_is_not_vacuous(self, no_fork):
        """cwd= rules out posix_spawn on every platform (subprocess.py:1859),
        so this spawn MUST trip the hook. If it stops doing so, every
        `test_never_forks` above is passing for no reason."""
        with pytest.raises(ForkAttempted):
            subprocess.run([sys.executable, "-c", "pass"], cwd=str(_REPO_ROOT))

    def test_spawn_run_clears_the_same_bar(self, no_fork):
        result = spawn.run(
            [sys.executable, "-c", "print('ok')"],
            cwd=str(_REPO_ROOT),
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0
        assert result.stdout.strip() == "ok"
