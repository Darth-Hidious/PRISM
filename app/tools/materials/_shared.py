# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Shared process-level registry access for the materials tools (SCI-9/C3 fix).

The screening/informatics/structure tools previously called
``build_full_registry()`` on EVERY tool call — a full bootstrap (including MCP
re-discovery and plugin loads) that minted a fresh SearchEngine each time, so
result caches were always cold and circuit-breaker state reset per call,
bypassing the S1-S7 resilience work. This module builds the registry once per
process and reuses it, sharing cache + breaker state across calls.

TODO(deep refactor): the host tool_server already builds its own registry at
startup; these tools should be handed THAT registry (dependency injection)
instead of building a parallel one. Until then, one shared build per process
is the honest middle ground.
"""

from __future__ import annotations

import threading

_lock = threading.Lock()
_registry = None


def get_shared_registry():
    """Return the process-wide ToolRegistry (built once, reused across calls)."""
    global _registry
    with _lock:
        if _registry is None:
            from app.plugins.bootstrap import build_full_registry

            _registry, _, _ = build_full_registry()
        return _registry
