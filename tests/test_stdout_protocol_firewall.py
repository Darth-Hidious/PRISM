"""Regression: the stdin/stdout JSON wire protocol is inviolable.

A third-party library (``mace.tools.cg``) does a bare ``print()`` at
import time when cuequivariance is absent; ``build_full_registry()``
triggers that import. Before the firewall, that byte landed on the real
stdout as "line 1" and broke every client (the 4 test_tool_server
failures). These tests prove the firewall holds against BOTH import-time
noise and noise emitted after the firewall is installed — at the fd
level, so even a bare ``print`` / C-extension write cannot leak.

Run as child processes on purpose: pytest's own stdout capture would
mask the very fd-level redirection we are verifying. Spawned via
``os.posix_spawn`` (see tests/_posix_spawn.py) — NOT subprocess — because
the torch-heavy pytest parent makes ``fork()`` SIGSEGV (task #22).
"""

from __future__ import annotations

import json
import sys
import textwrap

from tests._posix_spawn import spawn_run


def test_tool_server_first_stdout_line_is_valid_json():
    """The mace import print must NOT precede the protocol response."""
    proc = spawn_run(
        [sys.executable, "-m", "app.tool_server"],
        input=json.dumps({"method": "list_tools"}) + "\n",
        timeout=120,
    )
    assert proc.returncode == 0, proc.stderr[-2000:]
    lines = [ln for ln in proc.stdout.splitlines() if ln.strip()]
    assert lines, f"no stdout lines; stderr tail:\n{proc.stderr[-1000:]}"
    # Line 1 must parse as the protocol response, not a library banner.
    payload = json.loads(lines[0])
    assert "tools" in payload and isinstance(payload["tools"], list)
    # The known offender must have been routed to stderr instead.
    assert "cuequivariance" not in proc.stdout


def test_firewall_routes_all_noise_to_stderr_only():
    """After importing app.tool_server, ONLY explicit protocol writes
    reach the real stdout — bare print() and the mace banner do not."""
    script = textwrap.dedent(
        """
        import sys
        import app.tool_server as ts  # installs the fd-level firewall
        print("NOISE_VIA_PRINT")                     # -> must go to stderr
        sys.stdout.write("NOISE_VIA_SYS_STDOUT\\n")   # -> must go to stderr
        ts._PROTOCOL_OUT.write("PROTOCOL_SENTINEL\\n")
        ts._PROTOCOL_OUT.flush()
        """
    )
    proc = spawn_run([sys.executable, "-c", script], timeout=120)
    assert proc.returncode == 0, proc.stderr[-2000:]
    assert proc.stdout == "PROTOCOL_SENTINEL\n", (
        f"stdout contaminated: {proc.stdout!r}"
    )
    # The noise + library banner are present, but on stderr.
    assert "NOISE_VIA_PRINT" in proc.stderr
    assert "NOISE_VIA_SYS_STDOUT" in proc.stderr


def test_mcp_build_registry_leaves_stdout_pristine_and_restored():
    """mcp_server redirects stdout only DURING registry build, then
    restores it so FastMCP gets a clean JSON-RPC channel."""
    script = textwrap.dedent(
        """
        import sys
        from app.mcp_server import _build_registry
        reg = _build_registry()              # mace banner must NOT leak here
        assert reg.list_tools(), "empty registry"
        # stdout is restored after the context manager — this MUST land
        # on the real stdout for FastMCP to work.
        print("STDOUT_RESTORED_OK")
        """
    )
    proc = spawn_run([sys.executable, "-c", script], timeout=120)
    assert proc.returncode == 0, proc.stderr[-2000:]
    assert proc.stdout.strip() == "STDOUT_RESTORED_OK", (
        f"stdout not pristine/restored: {proc.stdout!r}"
    )
    assert "cuequivariance" not in proc.stdout
