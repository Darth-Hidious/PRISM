"""MCP CLI command group: manage MCP server connections."""
import json
from pathlib import Path

import click
from rich.console import Console


@click.group("mcp")
def mcp_group():
    """Manage MCP server connections."""
    pass


@mcp_group.command("init")
def mcp_init():
    """Create a template mcp_servers.json config file."""
    console = Console(force_terminal=True, width=120)

    config_dir = Path.home() / ".prism"
    config_dir.mkdir(parents=True, exist_ok=True)
    config_path = config_dir / "mcp_servers.json"

    if config_path.exists():
        console.print(f"[yellow]Config already exists: {config_path}[/yellow]")
        console.print("[dim]Edit it directly to add or remove servers.[/dim]")
        return

    template = {
        "mcpServers": {
            "example-filesystem": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"],
            },
        }
    }
    config_path.write_text(json.dumps(template, indent=2))
    console.print(f"[green]Created MCP config: {config_path}[/green]")
    console.print("[dim]Edit the file to configure your MCP servers.[/dim]")


@mcp_group.command("status")
def mcp_status():
    """Show MCP server configuration and connection status."""
    console = Console(force_terminal=True, width=120)

    from app.mcp_client import load_mcp_config
    config = load_mcp_config()
    console.print(f"[dim]Config: {config.config_path}[/dim]")

    if not config.config_path.exists():
        console.print("[yellow]No config file found. Run 'prism mcp init' to create one.[/yellow]")
        return

    if not config.servers:
        console.print("[dim]No MCP servers configured.[/dim]")
        return

    console.print(f"[cyan]Configured servers:[/cyan] {len(config.servers)}")
    for name, server_config in config.servers.items():
        if "url" in server_config:
            location = server_config["url"]
        elif "command" in server_config:
            location = f"{server_config['command']} {' '.join(server_config.get('args', []))}"
        else:
            location = "unknown"
        console.print(f"  [green]{name}[/green] — {location}")
