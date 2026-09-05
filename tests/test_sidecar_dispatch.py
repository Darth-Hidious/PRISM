"""The science sidecar answers a request instead of crashing on it.

Run 2 of the SX500 research (2026-09-05): `calphad list_databases` came back
as "science sidecar exited (code 1)" with a TypeError from tool_server._handle —
the sidecar passed its ToolRegistry where a state dict was expected. Every
sidecar tool (pyiron, CALPHAD) was unreachable, and the error read like a
missing database rather than a bug."""
import io
import json
import sys

from app import sidecar_server
from app.tools.base import ToolRegistry


def test_a_list_tools_request_gets_a_tool_list_not_a_crash(monkeypatch):
    monkeypatch.setattr(sidecar_server, "build_sidecar_registry", lambda: ToolRegistry())
    out = io.StringIO()
    monkeypatch.setattr(sidecar_server, "_PROTOCOL_OUT", out)
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps({"method": "list_tools"}) + "\n"))
    sidecar_server.main()
    response = json.loads(out.getvalue().strip().splitlines()[-1])
    assert "error" not in response, response
    assert isinstance(response.get("tools"), list), response
