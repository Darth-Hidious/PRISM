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


def test_a_tool_written_during_a_session_becomes_callable_without_a_restart(tmp_path):
    """The agent writes a tool, then uses it. No kernel restart.

    Restarting to pick up a new tool throws away every variable, every loaded
    dataset and the notebook the human is working in — an absurd price for the
    harness to learn that a file appeared. This is the whole point of
    `reload_tools`, so it is asserted end to end: the tool must be ABSENT
    first, present after the reload, and actually CALLABLE.
    """
    import time

    # The loader globs $HOME/.prism/plugins/*.py, so HOME is redirected below
    # and the directory must sit exactly there.
    plugin_dir = tmp_path / ".prism" / "plugins"
    plugin_dir.mkdir(parents=True)
    env = {
        **os.environ,
        "PYTHONPATH": REPO_ROOT,
        "PRISM_ENABLE_PLUGINS": "1",
        "PRISM_ENABLE_MCP": "0",
        "HOME": str(tmp_path),
    }
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
    try:
        before = _send(proc, {"method": "list_tools"})
        names_before = {t["name"] for t in before["tools"]}
        assert "probe_written_at_runtime" not in names_before

        # The agent authors a tool mid-session.
        (plugin_dir / "prism_runtime_probe.py").write_text(
            "from app.tools.base import Tool\n"
            "\n"
            "def register(registry):\n"
            "    def _run(**kwargs):\n"
            "        return {'ok': True, 'echo': kwargs.get('text', '')}\n"
            "    registry.tool_registry.register(Tool(\n"
            "        name='probe_written_at_runtime',\n"
            "        description='Written by the agent during a live session.',\n"
            "        input_schema={'type': 'object',"
            " 'properties': {'text': {'type': 'string'}}},\n"
            "        func=_run,\n"
            "        requires_approval=False,\n"
            "    ))\n"
        )

        reloaded = _send(proc, {"method": "reload_tools"})
        assert reloaded.get("status") == "ok", reloaded
        assert "probe_written_at_runtime" in reloaded["added"], reloaded
        assert reloaded["count"] == len(names_before) + 1

        called = _send(
            proc,
            {
                "method": "call_tool",
                "tool": "probe_written_at_runtime",
                "args": {"text": "hello"},
            },
        )
        assert called.get("result") == {"ok": True, "echo": "hello"}, called
    finally:
        proc.stdin.close()
        proc.terminate()
        proc.wait(timeout=10)
