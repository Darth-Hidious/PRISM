# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""Tool execution server — stdin/stdout JSON-line protocol.

Run as: python3 -m app.tool_server

Reads one JSON object per line from stdin, writes one JSON object per line
to stdout.  Methods: list_tools, call_tool.
"""
import json
import os
import sys


def _install_stdout_firewall():
    """Reserve the real stdout for the JSON-line protocol ONLY.

    ``build_full_registry()`` (and tool execution) import heavy
    third-party stacks — mace/torch/e3nn — and some emit a bare
    ``print()`` at import time (e.g. ``mace.tools.cg`` when
    cuequivariance is absent). Any byte on the real stdout that is not a
    protocol line corrupts the wire and breaks every client.

    We dup the real stdout file descriptor, then point BOTH OS fd 1 and
    ``sys.stdout`` at stderr for the rest of the process; protocol
    responses are written through the dup'd fd. fd-level (not merely a
    ``sys.stdout`` swap) so C-extension writes and subprocess-inherited
    fd 1 are caught too. Returns the line-buffered protocol stream.
    """
    sys.stdout.flush()
    protocol_fd = os.dup(1)            # the real stdout — protocol owns it
    os.dup2(2, 1)                      # OS fd 1 -> stderr (bare prints, C exts)
    sys.stdout = sys.stderr           # Python-level prints -> stderr
    return os.fdopen(protocol_fd, "w", buffering=1)


# Firewall BEFORE importing bootstrap: the offending print fires during
# the mace import that build_full_registry triggers.
_PROTOCOL_OUT = _install_stdout_firewall()

from app.plugins.bootstrap import build_full_registry  # noqa: E402


def _env_flag(name: str, default: bool) -> bool:
    value = os.getenv(name)
    if value is None:
        return default
    return value.strip().lower() not in {"0", "false", "no", "off"}


def _handle(registry, request: dict) -> dict:
    method = request.get("method")
    if method is None:
        return {"error": "missing 'method' field"}

    if method == "list_tools":
        return {
            "tools": [
                {
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                    "requires_approval": t.requires_approval,
                    "source": t.source,
                    "source_detail": t.source_detail,
                }
                for t in registry.list_tools()
            ]
        }

    if method == "call_tool":
        name = request.get("tool", "")
        try:
            tool = registry.get(name)
        except KeyError:
            return {"error": f"unknown tool: {name}"}
        try:
            result = tool.execute(**(request.get("args") or {}))
            return {"result": result}
        except Exception as exc:
            return {"error": str(exc)}

    return {"error": f"unknown method: {method}"}


def main():
    # External MCP tools are now part of the same runtime catalog as local
    # PRISM tools. Keep a simple env kill-switch so operators can disable them
    # without patching the launcher.
    enable_mcp = _env_flag("PRISM_ENABLE_MCP", True)
    enable_plugins = _env_flag("PRISM_ENABLE_PLUGINS", False)
    tool_reg, _, _ = build_full_registry(
        enable_mcp=enable_mcp,
        enable_plugins=enable_plugins,
    )

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            response = {"error": f"invalid JSON: {exc}"}
        else:
            response = _handle(tool_reg, request)

        _PROTOCOL_OUT.write(json.dumps(response) + "\n")
        _PROTOCOL_OUT.flush()


if __name__ == "__main__":
    main()
