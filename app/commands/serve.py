"""Serve CLI command: start PRISM as an MCP server."""
import json

import click
from rich.console import Console


@click.command("serve")
@click.option("--transport", default="stdio", type=click.Choice(["stdio", "http"]),
              help="MCP transport (stdio for Claude Desktop, http for web)")
@click.option("--port", default=8000, type=int, help="HTTP port (only for http transport)")
@click.option("--install", is_flag=True, help="Print Claude Desktop configuration JSON and exit")
def serve(transport, port, install):
    """Start PRISM as an MCP server for external LLM hosts."""
    console = Console(force_terminal=True, width=120)

    if install:
        from app.mcp_server import generate_claude_desktop_config
        config = generate_claude_desktop_config()
        console.print(json.dumps(config, indent=2))
        return

    from app.mcp_server import create_mcp_server
    server = create_mcp_server()
    if transport == "http":
        console.print(f"[bold cyan]PRISM MCP Server[/bold cyan] starting on http://localhost:{port}/mcp", err=True)
        server.run(transport="streamable-http", port=port)
    else:
        console.print("[bold cyan]PRISM MCP Server[/bold cyan] starting on stdio", err=True)
        server.run(transport="stdio")
