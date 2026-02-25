"""Data pipeline CLI commands: collect, status, import."""
import asyncio

import click
from rich.console import Console
from rich.table import Table


def _materials_to_dataframe(materials):
    """Convert list[Material] to pandas DataFrame, preserving sources."""
    import pandas as pd

    rows = []
    for m in materials:
        row = {
            "id": m.id,
            "formula": m.formula,
            "elements": ",".join(sorted(m.elements)),
            "n_elements": m.n_elements,
            "sources": ",".join(m.sources),
        }
        for prop_name in ("space_group", "crystal_system", "band_gap",
                          "formation_energy", "energy_above_hull",
                          "bulk_modulus", "debye_temperature"):
            pv = getattr(m, prop_name, None)
            if pv and pv.value is not None:
                row[prop_name] = pv.value
                row[f"{prop_name}_source"] = pv.source
        rows.append(row)
    return pd.DataFrame(rows)


@click.group()
def data():
    """Manage materials data collection and storage."""
    pass


@data.command()
@click.option("--elements", default=None, help="Elements to search, e.g. 'Si,O'")
@click.option("--formula", default=None, help="Chemical formula, e.g. 'SiO2'")
@click.option("--providers", default=None, help="Comma-separated provider IDs")
@click.option("--limit", default=100, type=int, help="Maximum results (default: 100)")
@click.option("--name", default=None, help="Dataset name (auto-generated if not given)")
def collect(elements, formula, providers, limit, name):
    """Collect materials data from federated OPTIMADE search and save as a dataset."""
    console = Console()
    from app.search import SearchEngine, MaterialSearchQuery
    from app.search.providers.registry import build_registry
    from app.data.store import DataStore

    if not elements and not formula:
        console.print("[red]Provide --elements or --formula[/red]")
        return

    elems = [e.strip() for e in elements.split(",")] if elements else None
    prov_list = [p.strip() for p in providers.split(",")] if providers else None

    query = MaterialSearchQuery(
        elements=elems,
        formula=formula,
        providers=prov_list,
        limit=limit,
    )
    console.print(f"[bold]Query:[/bold] elements={elems}, formula={formula}, limit={limit}")

    with console.status("[bold green]Searching federated providers..."):
        registry = build_registry()
        engine = SearchEngine(registry=registry)
        result = asyncio.run(engine.search(query))

    if not result.materials:
        console.print("[yellow]No results found.[/yellow]")
        if result.warnings:
            for w in result.warnings:
                console.print(f"[dim]  {w}[/dim]")
        return

    df = _materials_to_dataframe(result.materials)
    dataset_name = name or f"collect_{elements or formula}".replace(",", "_")
    store = DataStore()
    path = store.save(df, dataset_name)

    console.print(f"[green]Collected {len(df)} materials ({len(result.query_log)} providers) -> {path}[/green]")
    if result.warnings:
        for w in result.warnings:
            console.print(f"[dim]  {w}[/dim]")


@data.command("import")
@click.argument("file_path", type=click.Path(exists=True))
@click.option("--name", default=None, help="Dataset name (defaults to filename stem)")
@click.option("--format", "file_format", default=None, help="File format override (csv, json, parquet)")
def import_cmd(file_path, name, file_format):
    """Import a local CSV, JSON, or Parquet file as a PRISM dataset."""
    console = Console()
    from app.tools.data import _import_dataset

    result = _import_dataset(
        file_path=file_path, dataset_name=name, file_format=file_format
    )
    if "error" in result:
        console.print(f"[red]{result['error']}[/red]")
    else:
        console.print(
            f"[green]Imported {result['rows']} rows as '{result['dataset_name']}'[/green]"
        )
        console.print(f"  Columns: {', '.join(result['columns'])}")


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
