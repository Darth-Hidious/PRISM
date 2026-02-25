"""Optimade CLI command group: interact with the OPTIMADE network."""
import click
from rich.console import Console
from rich.table import Table

from app.search.providers.registry import build_registry


@click.group()
def optimade():
    """Commands for interacting with the OPTIMADE network."""
    pass


@optimade.command("list-dbs")
def list_databases():
    """Lists all available OPTIMADE provider databases."""
    console = Console(force_terminal=True, width=120)

    try:
        reg = build_registry()
        providers = reg.get_all()

        table = Table(show_header=True, header_style="bold magenta", title="PRISM Provider Registry")
        table.add_column("ID", style="cyan")
        table.add_column("Name")
        table.add_column("Base URL")

        for p in sorted(providers, key=lambda p: p.id):
            base_url = getattr(p, '_endpoint', None) and p._endpoint.base_url or "N/A"
            table.add_row(p.id, p.name, base_url)
        console.print(table)
        console.print(f"\n[dim]Total: {len(providers)} providers[/dim]")

    except Exception as e:
        console.print(f"[bold red]Error loading registry: {e}[/bold red]")
