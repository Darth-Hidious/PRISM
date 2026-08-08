"""Offline policy tests for the Python worker."""
from __future__ import annotations

import socket

from app.tools import _offline


def test_loopback_is_allowed_but_private_and_public_hosts_are_not() -> None:
    assert _offline._is_loopback_host("localhost")
    assert _offline._is_loopback_host("127.0.0.1")
    assert _offline._is_loopback_host("::1")
    assert not _offline._is_loopback_host("10.0.0.7")
    assert not _offline._is_loopback_host("api.example.invalid")


def test_offline_guard_blocks_dns_without_touching_localhost(monkeypatch) -> None:
    monkeypatch.setenv("PRISM_OFFLINE", "1")
    original_getaddrinfo = socket.getaddrinfo
    original_connect = socket.socket.connect
    original_connect_ex = socket.socket.connect_ex
    try:
        _offline.install_external_network_guard()
        try:
            socket.getaddrinfo("api.example.invalid", 443)
        except OSError as exc:
            assert "offline mode" in str(exc)
        else:  # pragma: no cover - assertion branch
            raise AssertionError("remote DNS was not blocked")

        # Resolution is local and must not be rejected by the hard offline
        # policy. The connection itself may be refused; that is unrelated.
        assert socket.getaddrinfo("localhost", 1)
    finally:
        socket.getaddrinfo = original_getaddrinfo
        socket.socket.connect = original_connect
        socket.socket.connect_ex = original_connect_ex


def test_every_python_entry_point_installs_the_network_guard() -> None:
    """The guard is only worth having if every entry point actually calls it.

    `app/mcp_server.py` did not. That was invisible to both audits run against
    this tree: the socket layer here is correct and present, and this process is
    spawned by an EXTERNAL MCP host (Claude Desktop, forge), so no `Command::new`
    in the Rust tree points at it. The tests only exercised
    `install_external_network_guard()` in isolation — nothing asserted that any
    real entry point invoked it, which is precisely why the gap survived.

    Several tools in the registry these entry points build carry credentials and
    have no `PRISM_OFFLINE` check of their own — `platform_jobs(action='events')`
    sends the live X-API-Key/Bearer, `tools/web.py` sends FIRECRAWL_API_KEY — so
    they depend entirely on this call being made.

    Source-level assertion on purpose: importing these modules starts servers.
    """
    import pathlib

    root = pathlib.Path(__file__).resolve().parent.parent / "app"
    entry_points = ["tool_server.py", "sidecar_server.py", "mcp_server.py"]
    for name in entry_points:
        source = (root / name).read_text()
        assert "install_external_network_guard()" in source, (
            f"app/{name} builds the tool registry but never installs the offline "
            "network guard — PRISM_OFFLINE=1 would not be enforced in that process"
        )


def test_external_mcp_is_refused_offline_at_both_registration_and_call(monkeypatch) -> None:
    """The Python MCP client had no offline check; its Rust sibling has one.

    Guarded in TWO places on purpose. Discovery runs once at boot, but the
    handlers it registers live for the whole session — gating only registration
    would leave every already-registered server reachable after PRISM_OFFLINE=1
    is set on a running process.

    The process-wide socket patch is not cover here either: a stdio-transport
    server spawns an arbitrary command from ~/.prism/mcp_servers.json as a CHILD
    process, which does its own networking outside the parent's monkeypatch.
    """
    import asyncio

    from app import mcp_client
    from app.tools.base import ToolRegistry

    # A structurally valid stdio server that does nothing. `call_mcp_tool`
    # builds its `Client` OUTSIDE its try block, so an invalid config raises
    # instead of returning an error dict — an empty `{}` here would fail for
    # that reason rather than on policy, and prove nothing.
    server = {"command": "true", "args": []}

    monkeypatch.setenv("PRISM_OFFLINE", "1")
    assert mcp_client.discover_and_register_mcp_tools(ToolRegistry()) == []
    result = asyncio.run(mcp_client.call_mcp_tool("srv", server, "tool", {}))
    assert "offline mode" in result.get("error", ""), result

    # Inert when the policy is off, or the assertions above would pass against
    # a client that refused unconditionally. The call still fails — `true`
    # speaks no MCP — but it must fail as TRANSPORT, never as policy.
    monkeypatch.setenv("PRISM_OFFLINE", "0")
    result = asyncio.run(mcp_client.call_mcp_tool("srv", server, "tool", {}))
    assert "offline mode" not in result.get("error", ""), result


def test_hf_jobs_backend_refuses_offline_and_only_offline(monkeypatch) -> None:
    """`hf jobs` reaches huggingface.co itself and is launched with --secrets HF_TOKEN.

    Nothing chose this backend deliberately: `select_backend`'s "auto" heuristic
    picks it whenever HF_TOKEN is in the environment and no project id is set.
    """
    pytest = __import__("pytest")
    hf_jobs = pytest.importorskip(
        "app.tools.simulation.mace.backends.hf_jobs",
        reason="MACE deps (ulid) live in the PRISM venv, not the bare interpreter",
    )

    monkeypatch.setenv("PRISM_OFFLINE", "1")
    assert hf_jobs._offline()
    with pytest.raises(RuntimeError) as excinfo:
        hf_jobs._refuse_if_offline("run a job on")
    assert "offline mode" in str(excinfo.value)
    assert "HF_TOKEN" in str(excinfo.value)

    # Only "1" — same contract as the Rust side, where PRISM_OFFLINE=true reads
    # as OFF and warns rather than silently sealing nothing.
    for not_offline in ("0", "", "true", "yes"):
        monkeypatch.setenv("PRISM_OFFLINE", not_offline)
        assert not hf_jobs._offline(), not_offline
        hf_jobs._refuse_if_offline("run a job on")
