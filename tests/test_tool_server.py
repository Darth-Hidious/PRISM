"""Tests for the tool execution server (app.tool_server).

Spawns the server as a subprocess and communicates via stdin/stdout JSON lines.

The spawn MUST take CPython's ``posix_spawn`` path, not ``fork``. Once a
materials search has run in this process (``tests/test_materials_discovery_flow``
does one), macOS frameworks pulled in by the Materials Project provider make
``fork()`` unsafe: every fork-based ``Popen`` then dies with SIGSEGV in the
child *before it reaches exec*, so even ``/bin/echo`` crashes. ``posix_spawn``
is unaffected. CPython only takes that path when ``preexec_fn`` is None,
``close_fds`` is False, ``start_new_session`` is False and **cwd is None** —
hence PYTHONPATH below instead of ``cwd=``.

(The earlier version of this file blamed torch and skipped itself via a
``_torch_loaded()`` check. Torch is not loaded when this fails, so that gate
never fired and it would have hidden a real defect if it had. The same
fork-unsafety breaks the ``execute_bash`` tool in production — see
``app/tools/bash.py``, which still passes ``preexec_fn=os.setsid``.)
"""
import json
import os
import pathlib
import subprocess
import sys

import pytest

REPO_ROOT = str(pathlib.Path(__file__).resolve().parents[1])
SERVER_CMD = [sys.executable, "-m", "app.tool_server"]


def _send(proc, obj):
    """Send a JSON object as a line to the server's stdin and read the response."""
    line = json.dumps(obj) + "\n"
    proc.stdin.write(line)
    proc.stdin.flush()
    resp_line = proc.stdout.readline()
    return json.loads(resp_line)


@pytest.fixture()
def server():
    import time

    # No cwd= and close_fds=False so CPython uses posix_spawn (see module
    # docstring); PYTHONPATH makes `app` importable in the child instead.
    env = {**os.environ, "PYTHONPATH": REPO_ROOT}
    proc = subprocess.Popen(
        SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
        close_fds=False,
    )
    time.sleep(0.5)
    if proc.poll() is not None:
        rc = proc.returncode
        try:
            err = proc.stderr.read() or ""
        except Exception:
            err = ""
        raise RuntimeError(
            f"tool_server exited early (rc={rc}) stderr={err[:300]!r}"
        )
    yield proc
    try:
        proc.stdin.close()
    except BrokenPipeError:
        pass
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


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


def test_set_session_id(server):
    resp = _send(
        server,
        {"method": "set_session_id", "session_id": "session-from-rust"},
    )
    assert resp == {"status": "ok", "session_id": "session-from-rust"}


@pytest.mark.parametrize("session_id", [None, "", 42])
def test_set_session_id_rejects_invalid_values(server, session_id):
    resp = _send(server, {"method": "set_session_id", "session_id": session_id})
    assert resp == {"error": "'session_id' must be a non-empty string"}


def test_missing_method(server):
    resp = _send(server, {"tool": "search", "args": {}})
    assert "error" in resp


def test_invalid_json(server):
    server.stdin.write("this is not json\n")
    server.stdin.flush()
    resp_line = server.stdout.readline()
    resp = json.loads(resp_line)
    assert "error" in resp
