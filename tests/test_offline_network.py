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
