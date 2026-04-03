"""Tests for the tool execution server (app.tool_server).

Spawns the server as a subprocess and communicates via stdin/stdout JSON lines.
"""
import json
import subprocess
import sys

import pytest

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
    proc = subprocess.Popen(
        SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        cwd=str(__import__("pathlib").Path(__file__).resolve().parents[1]),
    )
    yield proc
    proc.stdin.close()
    proc.wait(timeout=5)


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
