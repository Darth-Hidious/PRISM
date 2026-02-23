"""Data pipeline CLI commands: collect, status, export."""
import click
from rich.console import Console
from rich.table import Table


@click.group()
def data():
    """Manage materials data collection and storage."""
    pass


@data.command()
@click.option("--elements", default=None, help="Elements to search, e.g. 'Si,O'")
@click.option("--formula", default=None, help="Chemical formula, e.g. 'SiO2'")
@click.option("--providers", default=None, help="Comma-separated provider IDs")
@click.option("--max-results", default=100, help="Max results per provider")
@click.option("--name", default=None, help="Dataset name (auto-generated if not given)")
def collect(elements, formula, providers, max_results, name):
    """Collect materials data from OPTIMADE databases."""
    console = Console()
    from app.data.collector import OPTIMADECollector
    from app.data.normalizer import normalize_records
    from app.data.store import DataStore
    if not elements and not formula:
        console.print("[red]Provide --elements or --formula[/red]")
        return
    filter_parts = []
    if elements:
        elems = [e.strip() for e in elements.split(",")]
        quoted = ", ".join(f'"{e}"' for e in elems)
        filter_parts.append(f"elements HAS ALL {quoted}")
    if formula:
        filter_parts.append(f'chemical_formula_descriptive="{formula}"')
    filter_string = " AND ".join(filter_parts)
    provider_ids = [p.strip() for p in providers.split(",")] if providers else None
    console.print(f"[bold]Filter:[/bold] {filter_string}")
    with console.status("[bold green]Collecting data..."):
        collector = OPTIMADECollector()
        records = collector.collect(filter_string=filter_string, max_per_provider=max_results, provider_ids=provider_ids)
    if not records:
        console.print("[yellow]No results found.[/yellow]")
        return
    df = normalize_records(records)
    dataset_name = name or f"collect_{elements or formula}".replace(",", "_")
    store = DataStore()
    path = store.save(df, dataset_name)
    console.print(f"[green]Collected {len(df)} materials -> {path}[/green]")


@data.command()
def status():
    """Show available datasets and their metadata."""
    console = Console()
    from app.data.store import DataStore
    store = DataStore()
    datasets = store.list_datasets()
    if not datasets:
        console.print("[dim]No datasets found. Run 'prism data collect' first.[/dim]")
        return
    table = Table(title="Available Datasets")
    table.add_column("Name")
    table.add_column("Rows", justify="right")
    table.add_column("Columns", justify="right")
    table.add_column("Saved At")
    for ds in datasets:
        table.add_row(ds.get("name", "?"), str(ds.get("rows", "?")), str(len(ds.get("columns", []))), ds.get("saved_at", "?")[:19])
    console.print(table)
