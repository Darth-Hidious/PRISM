"""MCP client: connect to external MCP servers and import their tools."""
import asyncio
import json
import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional

from app.tools.base import Tool, ToolRegistry


@dataclass
class MCPClientConfig:
    """Configuration for external MCP server connections."""

    config_path: Path = field(
        default_factory=lambda: Path.home() / ".prism" / "mcp_servers.json"
    )
    servers: Dict[str, Dict[str, Any]] = field(default_factory=dict)


def load_mcp_config(path: Optional[str] = None) -> MCPClientConfig:
    """Load MCP server configuration from JSON file."""
    config = MCPClientConfig()
    if path:
        config.config_path = Path(path)
    if config.config_path.exists():
        data = json.loads(config.config_path.read_text())
        config.servers = data.get("mcpServers", {})
    return config


async def discover_tools_from_server(
    server_name: str, server_config: Dict[str, Any]
) -> List[Dict]:
    """Connect to an MCP server and discover its tools.

    Returns list of dicts with: name, description, input_schema, server_name.
    """
    # Self-guarded, like `call_mcp_tool`, rather than relying on its caller.
    # Today the only caller is `discover_and_register_mcp_tools`, which is
    # guarded — so this is defence in depth, not a live hole. But the asymmetry
    # was the finding: a future direct import (a "test this MCP server" button)
    # would bypass the policy with nothing to notice it.
    if os.environ.get("PRISM_OFFLINE", "").strip() == "1":
        return []

    from fastmcp import Client

    # FastMCP Client expects {"mcpServers": {"name": config}} format
    mcp_config = {"mcpServers": {server_name: server_config}}
    client = Client(mcp_config)
    tools = []
    try:
        async with client:
            mcp_tools = await client.list_tools()
            for t in mcp_tools:
                tools.append(
                    {
                        "name": f"{server_name}_{t.name}",
                        "description": f"[{server_name}] {t.description or t.name}",
                        "input_schema": t.inputSchema if hasattr(t, "inputSchema") else {},
                        "server_name": server_name,
                        "original_name": t.name,
                    }
                )
    except Exception:
        pass  # Server unavailable, skip silently
    return tools


async def call_mcp_tool(
    server_name: str,
    server_config: Dict[str, Any],
    tool_name: str,
    arguments: dict,
) -> dict:
    """Call a tool on an external MCP server."""
    # Guarded separately from discovery, not just alongside it. Discovery runs
    # once at boot; these handlers live for the whole session. A registry built
    # while online keeps working after PRISM_OFFLINE=1 is set, so gating only
    # registration would leave every already-registered server reachable.
    if os.environ.get("PRISM_OFFLINE", "").strip() == "1":
        return {"error": "offline mode: external MCP servers are unreachable"}

    from fastmcp import Client

    mcp_config = {"mcpServers": {server_name: server_config}}
    client = Client(mcp_config)
    try:
        async with client:
            result = await client.call_tool(tool_name, arguments)
            if hasattr(result, "content") and result.content:
                texts = [c.text for c in result.content if hasattr(c, "text")]
                return {"result": "\n".join(texts)}
            return {"result": str(result)}
    except Exception as e:
        return {"error": str(e)}


def discover_and_register_mcp_tools(
    registry: ToolRegistry, config: Optional[MCPClientConfig] = None
) -> List[str]:
    """Discover tools from all configured MCP servers and register them.

    Returns list of registered tool names.
    """
    # Hard offline: no external MCP servers. Mirrors the Rust client exactly —
    # `crates/agent/src/mcp.rs:177` returns an empty client under the same
    # condition — and this one had no check at all.
    #
    # The process-wide socket guard is not sufficient cover here: a stdio
    # transport spawns an arbitrary `command`/`args` from
    # `~/.prism/mcp_servers.json` as a CHILD process, which does its own
    # networking outside the parent's monkeypatch. The config's own documented
    # example carries an `env` block, so a credential can ride along.
    if os.environ.get("PRISM_OFFLINE", "").strip() == "1":
        return []

    if config is None:
        config = load_mcp_config()
    if not config.servers:
        return []

    # Discover ALL servers concurrently — serially this was the single
    # biggest boot cost (each npx server spawn takes 30-90s; sum over ~8
    # servers held "Igniting core" for minutes). Wall-clock is now the
    # slowest server, capped by a per-server timeout so one broken entry
    # can never hang boot.
    async def _discover_all():
        async def one(name, cfg):
            try:
                tools = await asyncio.wait_for(
                    discover_tools_from_server(name, cfg), timeout=45
                )
            except Exception:
                tools = []
            return name, cfg, tools

        return await asyncio.gather(
            *(one(n, c) for n, c in config.servers.items())
        )

    results = asyncio.run(_discover_all())

    registered = []
    for server_name, server_config, tools in results:
        for tool_info in tools:
            # Create a closure that calls the remote MCP tool
            def make_handler(sname, sconfig, orig_name):
                def handler(**kwargs) -> dict:
                    return asyncio.run(
                        call_mcp_tool(sname, sconfig, orig_name, kwargs)
                    )

                return handler

            tool = Tool(
                name=tool_info["name"],
                description=tool_info["description"],
                input_schema=tool_info["input_schema"],
                func=make_handler(
                    server_name, server_config, tool_info["original_name"]
                ),
                requires_approval=True,
                source="mcp",
                source_detail=server_name,
            )
            registry.register(tool)
            registered.append(tool_info["name"])

    return registered
