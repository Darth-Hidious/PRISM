"""Tests for the tool execution server (app.tool_server).

Spawns the server as a subprocess and communicates via stdin/stdout JSON lines.

Spawn mechanism — IMPORTANT: this uses ``os.posix_spawn``, NOT
``subprocess.Popen``. ``subprocess.Popen`` on macOS uses ``fork()`` +
``exec()``; once an earlier test has fully initialized Apple's
Accelerate/libdispatch (GCD) by importing the torch/MACE stack (e.g.
``test_materials_discovery_flow`` calling ``build_full_registry()``), a
subsequent ``fork()`` makes the pre-``exec`` child SIGSEGV — the
documented GCD-after-fork hazard (root-caused via task #22). The argv
list is passed directly to ``posix_spawn`` (no shell), and it never
copies the parent address space, so the crash is structurally
impossible. This also mirrors production: the Rust forge process spawns
the tool server without a Python fork (clean parent).
"""
import json
import os
import pathlib
import sys

import pytest

_REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]


def _send(proc, obj):
    """Send a JSON object as a line to the server's stdin and read the response."""
    line = json.dumps(obj) + "\n"
    proc.stdin.write(line)
    proc.stdin.flush()
    resp_line = proc.stdout.readline()
    return json.loads(resp_line)


class _SpawnedServer:
    """Popen-compatible handle for a posix_spawn'd tool server.

    Exposes the subset the tests use: ``stdin`` (text), ``stdout``
    (text), ``returncode``, ``poll``, ``wait``, ``kill``. Child stderr is
    sent to /dev/null — the protocol only uses stdout, and an undrained
    stderr pipe would itself deadlock (the same bug fixed product-side in
    crates/python-bridge/src/tool_server.rs).
    """

    def __init__(self):
        c_stdin_r, p_stdin_w = os.pipe()
        p_stdout_r, c_stdout_w = os.pipe()
        devnull = os.open(os.devnull, os.O_WRONLY)
        # cwd is set via a tiny runpy bootstrap (posix_spawn has no
        # portable chdir file-action). run_name='__main__' fires the
        # server's `if __name__ == "__main__": main()`.
        boot = (
            f"import os,runpy;os.chdir({str(_REPO_ROOT)!r});"
            "runpy.run_module('app.tool_server',run_name='__main__')"
        )
        file_actions = [
            (os.POSIX_SPAWN_DUP2, c_stdin_r, 0),
            (os.POSIX_SPAWN_DUP2, c_stdout_w, 1),
            (os.POSIX_SPAWN_DUP2, devnull, 2),
            (os.POSIX_SPAWN_CLOSE, p_stdin_w),
            (os.POSIX_SPAWN_CLOSE, p_stdout_r),
        ]
        self.pid = os.posix_spawn(
            sys.executable,
            [sys.executable, "-c", boot],
            os.environ,
            file_actions=file_actions,
        )
        os.close(c_stdin_r)
        os.close(c_stdout_w)
        os.close(devnull)
        self.stdin = os.fdopen(p_stdin_w, "w")
        self.stdout = os.fdopen(p_stdout_r, "r")
        self.returncode = None

    def poll(self):
        if self.returncode is not None:
            return self.returncode
        pid, status = os.waitpid(self.pid, os.WNOHANG)
        if pid == 0:
            return None
        self.returncode = (
            -os.WTERMSIG(status) if os.WIFSIGNALED(status) else os.WEXITSTATUS(status)
        )
        return self.returncode

    def wait(self, timeout=None):
        import time

        deadline = None if timeout is None else time.monotonic() + timeout
        while self.poll() is None:
            if deadline is not None and time.monotonic() > deadline:
                raise TimeoutError("server did not exit")
            time.sleep(0.02)
        return self.returncode

    def kill(self):
        if self.returncode is None:
            try:
                os.kill(self.pid, 9)
            except ProcessLookupError:
                pass


@pytest.fixture()
def server():
    proc = _SpawnedServer()
    yield proc
    try:
        proc.stdin.close()
    except Exception:
        pass
    proc.kill()
    try:
        proc.wait(timeout=5)
    except Exception:
        pass
    try:
        proc.stdout.close()
    except Exception:
        pass


def test_list_tools(server):
    resp = _send(server, {"method": "list_tools"})
    assert "tools" in resp
    assert isinstance(resp["tools"], list)
    assert len(resp["tools"]) > 0
    first = resp["tools"][0]
    assert "name" in first
    assert "description" in first
    assert "input_schema" in first
    assert "requires_approval" in first


def test_call_tool_unknown(server):
    resp = _send(server, {"method": "call_tool", "tool": "__nonexistent_tool__", "args": {}})
    assert "error" in resp


def test_missing_method(server):
    resp = _send(server, {"tool": "search", "args": {}})
    assert "error" in resp


def test_invalid_json(server):
    server.stdin.write("this is not json\n")
    server.stdin.flush()
    resp_line = server.stdout.readline()
    resp = json.loads(resp_line)
    assert "error" in resp
