"""MCP client: connect to external MCP servers and import their tools."""
import asyncio
import json
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
    if config is None:
        config = load_mcp_config()
    if not config.servers:
        return []

    registered = []
    for server_name, server_config in config.servers.items():
        try:
            tools = asyncio.run(
                discover_tools_from_server(server_name, server_config)
            )
        except Exception:
            continue

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
            )
            registry.register(tool)
            registered.append(tool_info["name"])

    return registered
