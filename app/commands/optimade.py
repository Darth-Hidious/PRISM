"""Optimade CLI command group: interact with the OPTIMADE network."""
import click
from rich.console import Console
from rich.table import Table

from app.config.providers import FALLBACK_PROVIDERS
from app.commands.search import _make_optimade_client


@click.group()
def optimade():
    """Commands for interacting with the OPTIMADE network."""
    pass


@optimade.command("list-dbs")
def list_databases():
    """Lists all available OPTIMADE provider databases."""
    console = Console(force_terminal=True, width=120)

    with console.status("[bold green]Fetching all registered OPTIMADE providers...[/bold green]"):
        try:
            # Use our curated list to avoid noisy discovery
            client = _make_optimade_client()

            table = Table(show_header=True, header_style="bold magenta", title="Live OPTIMADE Providers")
            table.add_column("ID", style="cyan")
            table.add_column("Name")
            table.add_column("Description")
            table.add_column("Base URL")

            if hasattr(client, 'info') and client.info and hasattr(client.info, 'providers'):
                for provider in client.info.providers:
                    table.add_row(
                        provider.id,
                        provider.name,
                        provider.description,
                        str(provider.base_url) if provider.base_url else "N/A"
                    )
                console.print(table)
            else:
                raise Exception("Could not retrieve live provider information from client.")

        except Exception as e:
            # If the live fetch fails, fall back to a hardcoded list
            console.print(f"[yellow]Warning: Could not fetch the live list of OPTIMADE providers ({e}). Displaying a fallback list of known providers.[/yellow]")

            table = Table(show_header=True, header_style="bold magenta", title="Fallback List of Known Providers")
            table.add_column("ID", style="cyan")
            table.add_column("Name")
            table.add_column("Description")
            table.add_column("Base URL")

            for provider in FALLBACK_PROVIDERS:
                table.add_row(
                    provider["id"],
                    provider["name"],
                    provider["description"],
                    provider["base_url"]
                )
            console.print(table)
