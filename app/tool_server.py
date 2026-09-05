# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Tool execution server — stdin/stdout JSON-line protocol.

Run as: python3 -m app.tool_server

Reads one JSON object per line from stdin, writes one JSON object per line
to stdout.  Methods: list_tools, call_tool, set_session_id.
"""
import json
import os
from pathlib import Path
import sys

# CRITICAL: this worker writes line-delimited JSON-RPC to stdout. Some tools
# import heavy ML libraries (e.g. MACE → mace/tools/cg.py) that print() a
# banner to stdout at import time, which corrupts the protocol's first line
# and makes the Rust side fail with "expected value at line 1 column 1".
# Save the real stdout for the protocol, then point sys.stdout at stderr so any
# library banner lands on stderr instead of the JSON channel.
_PROTOCOL_OUT = sys.stdout
sys.stdout = sys.stderr

from app.tools._offline import install_external_network_guard

install_external_network_guard()

from app.plugins.bootstrap import build_full_registry


def _env_flag(name: str, default: bool) -> bool:
    value = os.getenv(name)
    if value is None:
        return default
    return value.strip().lower() not in {"0", "false", "no", "off"}


def hydrate_env_from_saved_api_keys() -> list[str]:
    """Load `~/.prism/api_keys.json` (written by the TUI's API-key window) into
    the environment, without overriding anything already exported. Returns
    the names newly set, so the caller can say what changed."""
    path = Path(os.environ.get("HOME", str(Path.home()))) / ".prism" / "api_keys.json"
    try:
        saved = json.loads(path.read_text())
    except (OSError, ValueError):
        return []
    loaded: list[str] = []
    for name, value in sorted(saved.items()) if isinstance(saved, dict) else []:
        if isinstance(value, str) and value and not os.environ.get(name):
            os.environ[name] = value
            loaded.append(name)
    return loaded


def _handle(state: dict, request: dict) -> dict:
    registry = state["registry"]
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

    if method == "set_session_id":
        session_id = request.get("session_id")
        if not isinstance(session_id, str) or not session_id:
            return {"error": "'session_id' must be a non-empty string"}
        try:
            from app.tools.memory import configure as configure_memory

            configure_memory(session_id=session_id)
        except Exception as exc:
            return {"error": str(exc)}
        return {"status": "ok", "session_id": session_id}

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

    if method == "reload_tools":
        # The agent can WRITE a tool (a plugin in ~/.prism/plugins) and then
        # use it in the same session. Without this, a tool it just authored is
        # invisible until the kernel is restarted, which throws away every
        # variable, every loaded dataset and the whole notebook state — an
        # absurd price for the harness to learn that a file appeared.
        #
        # Discovery already re-reads the directory on every call, so a rebuild
        # picks up new AND edited plugins. Session state is configured on a
        # module, not held by the registry, so it survives the swap.
        before = {t.name for t in registry.list_tools()}
        # Keys the TUI saved since this process was spawned. The environment
        # is fixed at spawn; a key pasted mid-session reached no tool until a
        # restart. A shell export still outranks the saved file.
        keys_loaded = hydrate_env_from_saved_api_keys()
        try:
            rebuilt, _, _ = build_full_registry(
                enable_mcp=state["enable_mcp"],
                enable_plugins=state["enable_plugins"],
            )
        except Exception as exc:  # keep serving the OLD registry
            return {
                "error": f"tool reload failed, keeping the previous catalog: {exc}"
            }
        state["registry"] = rebuilt
        after = {t.name for t in rebuilt.list_tools()}
        return {
            "status": "ok",
            "keys_loaded": keys_loaded,
            "count": len(after),
            "added": sorted(after - before),
            "removed": sorted(before - after),
            "plugins_enabled": state["enable_plugins"],
        }

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
    # Held in a dict so `reload_tools` can swap the registry in place without
    # tearing down the process.
    state = {
        "registry": tool_reg,
        "enable_mcp": enable_mcp,
        "enable_plugins": enable_plugins,
    }

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            response = {"error": f"invalid JSON: {exc}"}
        else:
            response = _handle(state, request)

        _PROTOCOL_OUT.write(json.dumps(response) + "\n")
        _PROTOCOL_OUT.flush()


if __name__ == "__main__":
    main()
