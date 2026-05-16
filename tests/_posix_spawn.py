"""posix_spawn-based child runner for tests that must launch a Python child.

WHY THIS EXISTS: ``subprocess.Popen``/``subprocess.run`` use ``fork()`` +
``exec()`` on macOS. Once any earlier test fully initialises Apple's
Accelerate/libdispatch (GCD) by importing the torch/MACE stack (e.g.
``test_materials_discovery_flow`` -> ``build_full_registry()``), a later
``fork()`` makes the pre-``exec`` child SIGSEGV (rc=-11) — the documented
GCD-after-fork hazard (root-caused in task #22). ``os.posix_spawn`` never
copies the parent address space, so the crash is structurally
impossible. Any test that launches a Python child from the (torch-heavy)
pytest process MUST use this, never raw subprocess. The argv list is
passed straight through (no shell) so there is no injection surface.

Drains stdout AND stderr concurrently so a child that floods either pipe
can't deadlock on a full ~64 KB OS buffer.
"""

from __future__ import annotations

import os
import select
import time
from types import SimpleNamespace


def spawn_run(argv: list[str], input: str | None = None, timeout: float = 120.0):
    """Run ``argv`` to completion. Returns ns(stdout, stderr, returncode).

    ``returncode`` is negative (``-signal``) if the child was killed by a
    signal — e.g. ``-11`` for SIGSEGV, ``-9`` for SIGKILL.
    """
    in_r, in_w = os.pipe()
    out_r, out_w = os.pipe()
    err_r, err_w = os.pipe()
    file_actions = [
        (os.POSIX_SPAWN_DUP2, in_r, 0),
        (os.POSIX_SPAWN_DUP2, out_w, 1),
        (os.POSIX_SPAWN_DUP2, err_w, 2),
        (os.POSIX_SPAWN_CLOSE, in_w),
        (os.POSIX_SPAWN_CLOSE, out_r),
        (os.POSIX_SPAWN_CLOSE, err_r),
    ]
    pid = os.posix_spawn(argv[0], argv, os.environ, file_actions=file_actions)
    os.close(in_r)
    os.close(out_w)
    os.close(err_w)

    if input:
        os.write(in_w, input.encode())
    os.close(in_w)  # EOF -> child stdin loop ends, child exits

    bufs = {out_r: b"", err_r: b""}
    fds = [out_r, err_r]
    deadline = time.monotonic() + timeout
    killed = False
    while fds:
        rlist, _, _ = select.select(fds, [], [], 0.5)
        if not rlist:
            if time.monotonic() > deadline:
                os.kill(pid, 9)
                killed = True
                break
            continue
        for fd in rlist:
            chunk = os.read(fd, 65536)
            if chunk:
                bufs[fd] += chunk
            else:
                os.close(fd)
                fds.remove(fd)
    for fd in fds:  # timeout path: close leftovers
        os.close(fd)

    _, status = os.waitpid(pid, 0)
    rc = -os.WTERMSIG(status) if os.WIFSIGNALED(status) else os.WEXITSTATUS(status)
    if killed:
        rc = -9
    return SimpleNamespace(
        stdout=bufs[out_r].decode(errors="replace"),
        stderr=bufs[err_r].decode(errors="replace"),
        returncode=rc,
    )
