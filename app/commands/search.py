"""Search CLI command: structured federated materials search via SearchEngine."""
import asyncio

import click
from rich.console import Console
from rich.panel import Panel
from rich.table import Table

from app.search import SearchEngine, MaterialSearchQuery, PropertyRange
from app.search.providers.registry import ProviderRegistry


@click.command()
@click.option('--elements', help='Comma-separated list of elements (e.g., "Si,O").')
@click.option('--formula', help='Chemical formula (e.g., "SiO2").')
@click.option('--nelements', type=int, help='Number of elements in the material.')
@click.option('--providers', help='Comma-separated list of provider IDs (e.g., "mp,oqmd,cod").')
@click.option('--limit', type=int, default=100, help='Maximum number of results (default: 100).')
@click.option('--band-gap-min', type=float, help='Minimum band gap in eV.')
@click.option('--band-gap-max', type=float, help='Maximum band gap in eV.')
@click.option('--space-group', help='Space group symbol (e.g., "Fm-3m").')
@click.option('--crystal-system', type=click.Choice(
    ["cubic", "hexagonal", "tetragonal", "orthorhombic", "monoclinic", "triclinic", "trigonal"],
    case_sensitive=False,
), help='Crystal system filter.')
def search(elements, formula, nelements, providers, limit, band_gap_min, band_gap_max, space_group, crystal_system):
    """Search materials databases via the PRISM federated search engine."""
    console = Console(force_terminal=True, width=120)

    if not any([elements, formula, nelements, band_gap_min, band_gap_max, space_group, crystal_system]):
        console.print("[red]Error: Please provide at least one search criterion.[/red]")
        return

    # Build structured query
    band_gap = None
    if band_gap_min is not None or band_gap_max is not None:
        band_gap = PropertyRange(min=band_gap_min, max=band_gap_max)

    n_elements = None
    if nelements is not None:
        n_elements = PropertyRange(min=nelements, max=nelements)

    query = MaterialSearchQuery(
        elements=[e.strip() for e in elements.split(",")] if elements else None,
        formula=formula,
        n_elements=n_elements,
        band_gap=band_gap,
        space_group=space_group,
        crystal_system=crystal_system,
        providers=[p.strip() for p in providers.split(",")] if providers else None,
        limit=limit,
    )

    # Show query info
    from app.search.translator import QueryTranslator
    optimade_filter = QueryTranslator.to_optimade(query)
    if optimade_filter:
        console.print(Panel(f"[bold]Filter:[/bold] [cyan]{optimade_filter}[/cyan]", title="Search Query", border_style="blue"))

    try:
        with console.status("[bold green]Searching materials databases...[/bold green]"):
            registry = ProviderRegistry.from_registry_json()
            engine = SearchEngine(registry=registry)
            result = asyncio.run(engine.search(query))

        # Show audit trail
        if result.query_log:
            audit = Table(show_header=True, header_style="bold dim")
            audit.add_column("Provider")
            audit.add_column("Status")
            audit.add_column("Results")
            audit.add_column("Latency")
            for log in result.query_log:
                status_style = "green" if log.status == "success" else "red"
                audit.add_row(
                    log.provider_id,
                    f"[{status_style}]{log.status}[/{status_style}]",
                    str(log.result_count),
                    f"{log.latency_ms:.0f}ms",
                )
            console.print(Panel(audit, title="Provider Audit Trail", border_style="dim"))

        # Show warnings
        for w in result.warnings:
            console.print(f"[yellow]Warning: {w}[/yellow]")

        if result.materials:
            console.print(f"[green]Found {result.total_count} materials ({result.search_time_ms:.0f}ms)[/green]")

            table = Table(show_header=True, header_style="bold magenta")
            table.add_column("ID")
            table.add_column("Formula")
            table.add_column("Elements")
            table.add_column("Sources")
            table.add_column("Band Gap (eV)")
            table.add_column("Space Group")

            for m in result.materials[:20]:
                table.add_row(
                    m.id,
                    m.formula,
                    ", ".join(m.elements),
                    ", ".join(m.sources),
                    f"{m.band_gap.value:.3f}" if m.band_gap and isinstance(m.band_gap.value, (int, float)) else "N/A",
                    str(m.space_group.value) if m.space_group else "N/A",
                )
            console.print(Panel(table, title=f"Top {min(20, len(result.materials))} Results", border_style="green"))

            if result.cached:
                console.print("[dim]Results served from cache[/dim]")
        else:
            console.print("[red]No materials found for the given criteria.[/red]")

    except Exception as e:
        console.print(f"[bold red]Search error: {e}[/bold red]")
