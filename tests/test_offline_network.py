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
