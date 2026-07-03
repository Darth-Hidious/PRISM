# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""Tool execution server — stdin/stdout JSON-line protocol.

Run as: python3 -m app.tool_server

Reads one JSON object per line from stdin, writes one JSON object per line
to stdout.  Methods: list_tools, call_tool.
"""
import json
import os
import sys

# CRITICAL: this worker writes line-delimited JSON-RPC to stdout. Some tools
# import heavy ML libraries (e.g. MACE → mace/tools/cg.py) that print() a
# banner to stdout at import time, which corrupts the protocol's first line
# and makes the Rust side fail with "expected value at line 1 column 1".
# Save the real stdout for the protocol, then point sys.stdout at stderr so any
# library banner lands on stderr instead of the JSON channel.
_PROTOCOL_OUT = sys.stdout
sys.stdout = sys.stderr

from app.plugins.bootstrap import build_full_registry


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
