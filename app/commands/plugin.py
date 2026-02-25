"""Plugin CLI command group: manage PRISM plugins."""
from pathlib import Path

import click
from rich.console import Console


@click.group("plugin")
def plugin_group():
    """Manage PRISM plugins."""
    pass


@plugin_group.command("list")
def plugin_list():
    """List installed plugins (entry points + local directory)."""
    console = Console(force_terminal=True, width=120)

    from app.plugins.registry import PluginRegistry
    from app.plugins.loader import discover_entry_point_plugins, discover_local_plugins

    reg = PluginRegistry()
    ep_names = discover_entry_point_plugins(reg)
    local_names = discover_local_plugins(reg)

    if not ep_names and not local_names:
        console.print("[dim]No plugins found.[/dim]")
        console.print("[dim]Install a plugin via pip or place a .py file in ~/.prism/plugins/[/dim]")
        return

    if ep_names:
        console.print("[cyan]Entry-point plugins:[/cyan]")
        for name in ep_names:
            console.print(f"  [green]{name}[/green]")
    if local_names:
        console.print("[cyan]Local plugins (~/.prism/plugins/):[/cyan]")
        for name in local_names:
            console.print(f"  [green]{name}[/green]")


@plugin_group.command("init")
@click.argument("name")
def plugin_init(name):
    """Create a plugin template in ~/.prism/plugins/."""
    console = Console(force_terminal=True, width=120)

    plugin_dir = Path.home() / ".prism" / "plugins"
    plugin_dir.mkdir(parents=True, exist_ok=True)
    plugin_file = plugin_dir / f"{name}.py"

    if plugin_file.exists():
        console.print(f"[yellow]Plugin already exists: {plugin_file}[/yellow]")
        return

    template = f'''"""PRISM plugin: {name}"""


def register(registry):
    """Called by PRISM plugin loader.

    registry attributes:
      - tool_registry: register custom tools
      - skill_registry: register custom skills
      - collector_registry: register custom data collectors
      - algorithm_registry: register custom ML algorithms
    """
    # Example: register a custom tool
    # from app.tools.base import Tool
    # registry.tool_registry.register(Tool(
    #     name="{name}_tool",
    #     description="My custom tool",
    #     input_schema={{"type": "object", "properties": {{}}}},
    #     func=lambda **kwargs: {{"result": "hello"}},
    # ))
    pass
'''
    plugin_file.write_text(template)
    console.print(f"[green]Created plugin template: {plugin_file}[/green]")
    console.print("[dim]Edit the file and add your custom tools, skills, collectors, or algorithms.[/dim]")
