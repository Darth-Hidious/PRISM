"""Process-wide external-network guard for the Python tool worker."""
from __future__ import annotations

import errno
import ipaddress
import os
import socket
from typing import Any


_REAL_CONNECT = socket.socket.connect
_REAL_CONNECT_EX = socket.socket.connect_ex
_REAL_GETADDRINFO = socket.getaddrinfo


def _is_loopback_host(host: Any) -> bool:
    if host is None:
        return True
    text = str(host).strip().lower().rstrip(".")
    if text in {"localhost", "localhost.localdomain"}:
        return True
    try:
        return ipaddress.ip_address(text.split("%", 1)[0]).is_loopback
    except ValueError:
        return False


def install_external_network_guard() -> None:
    """Block non-loopback sockets when ``PRISM_OFFLINE=1``.

    Local model and service endpoints remain usable. DNS resolution for a
    remote hostname is blocked before libc can issue a resolver request, so
    an offline compute node neither leaks a query nor waits for a dead DNS
    server.
    """
    if os.environ.get("PRISM_OFFLINE") != "1":
        return

    def guarded_getaddrinfo(host: Any, *args: Any, **kwargs: Any):
        if not _is_loopback_host(host):
            raise OSError("offline mode: external DNS/network access blocked")
        return _REAL_GETADDRINFO(host, *args, **kwargs)

    def guarded_connect(sock: socket.socket, address: Any):
        host = address[0] if isinstance(address, tuple) and address else address
        if not _is_loopback_host(host):
            raise OSError("offline mode: external network access blocked")
        return _REAL_CONNECT(sock, address)

    def guarded_connect_ex(sock: socket.socket, address: Any) -> int:
        host = address[0] if isinstance(address, tuple) and address else address
        if not _is_loopback_host(host):
            return errno_network_unreachable()
        return _REAL_CONNECT_EX(sock, address)

    socket.getaddrinfo = guarded_getaddrinfo
    socket.socket.connect = guarded_connect
    socket.socket.connect_ex = guarded_connect_ex


def errno_network_unreachable() -> int:
    return errno.ENETUNREACH
