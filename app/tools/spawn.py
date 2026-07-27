"""fork()-free process spawning for the execution tools.

WHY THIS EXISTS
    A PRISM materials search loads Apple's Network.framework, which registers a
    pthread_atfork child handler. From that moment on, any fork() in this
    process dies before it can exec:

        fork -> _pthread_atfork_child_handlers -> nw_settings_child_has_forked
             -> nw_path_release_globals -> NEFlowDirectorDestroy -> SIGSEGV

    The child is killed at -11 with no stdout and no stderr, so every
    execute_bash / execute_python call after a search fails blank.
    posix_spawn does not run atfork handlers and is unaffected.

WHAT CPYTHON REQUIRES
    Popen only takes the posix_spawn fast path when every condition in
    subprocess.Popen._execute_child holds (CPython 3.14, subprocess.py:1859).
    Measured on this platform (darwin, CPython 3.14.3) the ones we used to
    violate are:

        preexec_fn is None                      -- ruled out preexec_fn=os.setsid
        cwd is None                             -- ruled out cwd=..., including
                                                   cwd=os.getcwd(), which looks
                                                   like a no-op but still forks
        not close_fds or _HAVE_POSIX_SPAWN_CLOSEFROM
                                                -- macOS has no
                                                   posix_spawn_file_actions_addclosefrom_np,
                                                   so _HAVE_POSIX_SPAWN_CLOSEFROM
                                                   is False (subprocess.py:755)
                                                   and close_fds must be False
        not start_new_session and process_group == -1
                                                -- rules out both of Popen's
                                                   own ways to make a new
                                                   process group

    So chdir, setsid and fd hygiene cannot happen in the parent. They move into
    a trampoline: a short `python -I -S -c` program that is posix_spawn'ed and
    then, already past the fork in a fresh process, closes inherited
    descriptors, chdirs, optionally setsid()s, and execs the real command.
    exec keeps the pid, so os.killpg(proc.pid, ...) still reaches the whole
    process group and proc.returncode is still the real command's status.

ABOUT close_fds=False
    Narrower than it sounds but NOT empty. Since PEP 446 every descriptor
    CPython opens is O_CLOEXEC, so nothing Python created is inherited.
    Descriptors opened by native libraries are another matter: measured on
    macOS immediately after a materials search, Network.framework leaves two
    *inheritable* sockets behind (os.get_inheritable() is True on both). Those
    would reach model-authored code. The trampoline's explicit close loop is
    what shuts them, and it runs after the last instant anything could be
    opened behind our back, so it has no race.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from typing import Any

_IS_WINDOWS = os.name == "nt"

# Directory that lists this process's open descriptors, or "" if neither the
# BSD nor the Linux form exists (then the trampoline falls back to a sweep).
_FD_DIR = next((d for d in ("/dev/fd", "/proc/self/fd") if os.path.isdir(d)), "")

# Runs *after* exec, so nothing here is subject to the fork hazard it avoids.
# setsid() goes first so the window in which the process is not yet its own
# group leader is as short as possible; popen() then closes that window for
# good by waiting the group into existence.
# argv: <fd_dir> <cwd or ""> <"1"|"0" setsid> <real argv...>
_TRAMPOLINE = (
    "import os,sys\n"
    "if sys.argv[3] == '1': os.setsid()\n"
    "d=sys.argv[1]\n"
    "fds=[int(n) for n in os.listdir(d) if n.isdigit()] if d else range(3,4096)\n"
    "for fd in fds:\n"
    "    if fd > 2:\n"
    "        try: os.close(fd)\n"
    "        except OSError: pass\n"
    "if sys.argv[2]: os.chdir(sys.argv[2])\n"
    # execvp, not execv, so a bare command name still resolves through PATH the
    # way subprocess.Popen would. An exec failure must not surface as a Python
    # traceback from a process the caller never asked for: say what could not
    # run and use the conventional 127.
    "try: os.execvp(sys.argv[4], sys.argv[4:])\n"
    "except OSError as exc:\n"
    "    sys.stderr.write('cannot execute %s: %s\\n' % (sys.argv[4], exc))\n"
    "    sys.exit(127)\n"
)

# Knobs that would silently drag us back onto the fork path. Passing one is a
# programming error, not a runtime condition -- the last "fix" for this bug was
# inert precisely because nothing checked.
_FORK_FORCING_KWARGS = ("preexec_fn", "close_fds", "cwd", "start_new_session",
                        "pass_fds", "process_group")


def signal_name(returncode: int) -> str:
    """Map a negative subprocess return code (killed-by-signal) to its name."""
    try:
        return signal.Signals(-returncode).name
    except (ValueError, TypeError):
        return f"signal {-returncode}"


def _argv(argv: list[str], cwd: str | None, new_session: bool) -> list[str]:
    return [
        sys.executable, "-I", "-S", "-c", _TRAMPOLINE,
        _FD_DIR, cwd or "", "1" if new_session else "0",
        *argv,
    ]


def _check(kwargs: dict[str, Any]) -> None:
    for name in _FORK_FORCING_KWARGS:
        if name in kwargs:
            raise TypeError(
                f"{name}= would force subprocess back onto the fork() path, "
                f"which SIGSEGVs after a materials search; use this module's "
                f"own cwd= argument, and popen(new_session=True) for a process "
                f"group"
            )


def _await_own_process_group(proc: subprocess.Popen, timeout: float = 5.0) -> None:
    """Block until `proc` leads its own process group.

    setsid() moved from a parent-side preexec_fn into the trampoline, which
    opens a window -- one interpreter startup wide -- where os.killpg(proc.pid)
    would raise ESRCH because the group does not exist yet. Callers stop
    commands by process group, so returning inside that window would hand them
    a process they cannot kill. Wait it out instead.
    """
    deadline = time.monotonic() + timeout
    while True:
        try:
            if os.getpgid(proc.pid) == proc.pid:
                return
        except OSError:
            return  # already gone; no group left to signal
        if proc.poll() is not None:
            return  # died before setsid (trampoline failed); nothing to signal
        if time.monotonic() >= deadline:
            # There is no group to signal, but abandoning a live child that
            # nothing holds a handle to is worse than the error being raised.
            # Kill the one pid we do have, and reap it.
            proc.kill()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass  # unreapable after SIGKILL; still better than not trying
            raise RuntimeError(
                f"spawned pid {proc.pid} did not enter its own process group "
                f"within {timeout}s, so it could not be stopped by group kill; "
                f"it was killed directly instead and produced no output"
            )
        time.sleep(0.002)


def popen(
    argv: list[str],
    *,
    cwd: str | None = None,
    new_session: bool = False,
    **kwargs: Any,
) -> subprocess.Popen:
    """Popen `argv` without ever calling fork().

    cwd:         working directory for the command (chdir'd after exec).
    new_session: put the command in its own session/process group so
                 os.killpg(proc.pid, ...) reaches it and its children. Returns
                 only once that group actually exists.
    """
    _check(kwargs)
    if _IS_WINDOWS:  # no fork, no posix_spawn, no setsid -- nothing to work around
        return subprocess.Popen(argv, cwd=cwd, **kwargs)
    proc = subprocess.Popen(_argv(argv, cwd, new_session), close_fds=False, **kwargs)
    if new_session:
        _await_own_process_group(proc)
    return proc


def run(
    argv: list[str],
    *,
    cwd: str | None = None,
    **kwargs: Any,
) -> subprocess.CompletedProcess:
    """subprocess.run() without ever calling fork(). See popen().

    Deliberately no new_session: subprocess.run's timeout kills only the
    direct child, so a command in its own group could leave its children
    running. Callers that need a group must use popen() and kill the group
    themselves.
    """
    _check(kwargs)
    if _IS_WINDOWS:
        return subprocess.run(argv, cwd=cwd, **kwargs)
    return subprocess.run(_argv(argv, cwd, False), close_fds=False, **kwargs)
