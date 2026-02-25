"""Optimade CLI command group: interact with the OPTIMADE network."""
import click
from rich.console import Console
from rich.table import Table

from app.search.providers.endpoint import load_registry


@click.group()
def optimade():
    """Commands for interacting with the OPTIMADE network."""
    pass


@optimade.command("list-dbs")
def list_databases():
    """Lists all available OPTIMADE provider databases."""
    console = Console(force_terminal=True, width=120)

    try:
        endpoints = load_registry()

        table = Table(show_header=True, header_style="bold magenta", title="PRISM Provider Registry")
        table.add_column("ID", style="cyan")
        table.add_column("Name")
        table.add_column("Tier")
        table.add_column("Status")
        table.add_column("Structures")
        table.add_column("Base URL")

        for ep in sorted(endpoints, key=lambda e: (e.tier, e.id)):
            status_style = "green" if ep.enabled else ("yellow" if ep.status == "namespace_reserved" else "red")
            table.add_row(
                ep.id,
                ep.name,
                str(ep.tier),
                f"[{status_style}]{ep.status}[/{status_style}]",
                f"{ep.structures_approx:,}" if ep.structures_approx else "N/A",
                ep.base_url or "[dim]no endpoint[/dim]",
            )
        console.print(table)
        console.print(f"\n[dim]Total: {len(endpoints)} providers, {sum(1 for e in endpoints if e.enabled)} enabled[/dim]")

    except Exception as e:
        console.print(f"[bold red]Error loading registry: {e}[/bold red]")
