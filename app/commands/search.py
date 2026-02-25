"""Search CLI command: structured OPTIMADE network search."""
import os

import click
from rich.console import Console
from rich.panel import Panel
from rich.prompt import Confirm
from rich.table import Table
from optimade.client import OptimadeClient

from app.config.providers import FALLBACK_PROVIDERS

# Make database imports optional
try:
    from app.db.database import Base, engine, get_db
    from app.db.models import Material
    DB_AVAILABLE = True
except ImportError:
    DB_AVAILABLE = False

try:
    from mp_api.client import MPRester
    MP_API_AVAILABLE = True
except ImportError:
    MP_API_AVAILABLE = False


def _make_optimade_client(providers=None, max_results=1000):
    """Create OptimadeClient using explicit base_urls to avoid noisy discovery."""
    if providers:
        ids = [p.strip() for p in providers] if isinstance(providers, list) else [p.strip() for p in providers.split(",")]
        base_urls = [p["base_url"] for p in FALLBACK_PROVIDERS if p["id"] in ids]
        if not base_urls:
            base_urls = [p["base_url"] for p in FALLBACK_PROVIDERS]
    else:
        base_urls = [p["base_url"] for p in FALLBACK_PROVIDERS]
    return OptimadeClient(base_urls=base_urls, max_results_per_provider=max_results)


def enrich_materials_with_mp_data(materials, console=None, mp_api_key=None):
    """
    Enrich OPTIMADE materials with Materials Project native API data.
    Returns the enriched materials with formation energy and band gap data.
    """
    if not MP_API_AVAILABLE:
        if console:
            console.print("[yellow]Materials Project API not available. Using OPTIMADE data only.[/yellow]")
        return materials

    # Use provided key or fall back to environment variable
    if not mp_api_key:
        mp_api_key = os.getenv('MATERIALS_PROJECT_API_KEY')

    if console:
        console.print(f"[dim]Checking for MP API key... {'Found' if mp_api_key else 'Not found'}[/dim]")

    if not mp_api_key:
        if console:
            console.print("[yellow]No Materials Project API key found. Using OPTIMADE data only.[/yellow]")
        return materials

    try:
        with MPRester(mp_api_key) as mpr:
            # Extract MP IDs from the materials
            mp_ids = []
            for material in materials:
                material_id = material.get('id', '')
                # Convert to string and check if it's a Materials Project ID
                material_id_str = str(material_id)
                if material_id_str.startswith('mp-'):
                    mp_ids.append(material_id_str)

            if not mp_ids:
                return materials

            if console:
                console.print(f"[dim]Enriching {len(mp_ids)} Materials Project entries with native API data...[/dim]")

            # Fetch properties from MP native API
            mp_data = mpr.materials.summary.search(
                material_ids=mp_ids,
                fields=['material_id', 'formation_energy_per_atom', 'band_gap', 'energy_above_hull']
            )

            # Create a lookup dictionary
            mp_lookup = {doc.material_id: doc for doc in mp_data}

            # Enrich the materials
            enriched_materials = []
            for material in materials:
                enriched_material = material.copy()
                material_id = str(material.get('id', ''))

                if material_id in mp_lookup:
                    mp_doc = mp_lookup[material_id]
                    attrs = enriched_material.setdefault('attributes', {})

                    # Add MP native API data
                    if mp_doc.formation_energy_per_atom is not None:
                        attrs['_mp_formation_energy_per_atom'] = mp_doc.formation_energy_per_atom
                    if mp_doc.band_gap is not None:
                        attrs['_mp_band_gap'] = mp_doc.band_gap
                    if mp_doc.energy_above_hull is not None:
                        attrs['_mp_e_above_hull'] = mp_doc.energy_above_hull

                enriched_materials.append(enriched_material)

            return enriched_materials

    except Exception as e:
        if console:
            console.print(f"[yellow]Warning: Could not fetch MP native data. Error: {str(e)[:100]}[/yellow]")
        return materials


@click.command()
@click.option('--elements', help='Comma-separated list of elements (e.g., "Si,O").')
@click.option('--formula', help='Chemical formula (e.g., "SiO2").')
@click.option('--nelements', type=int, help='Number of elements in the material.')
@click.option('--providers', help='Comma-separated list of provider IDs (e.g., "mp,oqmd,cod").')
@click.option('--limit', type=int, default=1000, help='Maximum number of results to retrieve per provider (default: 1000).')
@click.option('--mp-api-key', help='Materials Project API key for enhanced properties (overrides environment variable).')
def search(elements, formula, nelements, providers, limit, mp_api_key):
    """
    Performs a structured search of the OPTIMADE network based on specific criteria.
    """
    console = Console(force_terminal=True, width=120)

    # Ensure at least one search criterion is provided
    if not any([elements, formula, nelements]):
        console.print("[red]Error: Please provide at least one search criterion (e.g., --elements, --formula).[/red]")
        return

    # Construct the filter string directly
    filters = []
    if elements:
        elements_str = ", ".join(f'"{e.strip()}"' for e in elements.split(','))
        filters.append(f"elements HAS ALL {elements_str}")
    if formula:
        filters.append(f'chemical_formula_descriptive="{formula}"')
    if nelements:
        filters.append(f"nelements={nelements}")
    optimade_filter = " AND ".join(filters)

    console.print(Panel(f"[bold]Filter:[/bold] [cyan]{optimade_filter}[/cyan]", title="Search Query", border_style="blue"))

    try:
        with console.status("[bold green]Querying OPTIMADE providers...[/bold green]"):
            # If specific providers are requested, use them. Otherwise, search all.
            client = _make_optimade_client(
                providers=providers.split(',') if providers else None,
                max_results=limit,
            )
            results = client.get(optimade_filter)

        # The optimade-client returns a nested dictionary. We need to extract the actual list of materials.
        all_materials = []
        if "structures" in results:
            for provider_results in results["structures"].get(optimade_filter, {}).values():
                if "data" in provider_results:
                    all_materials.extend(provider_results["data"])

        if all_materials:
            # Enrich Materials Project entries with native API data
            all_materials = enrich_materials_with_mp_data(all_materials, console, mp_api_key)

            console.print(f"[green]SUCCESS:[/green] Found {len(all_materials)} materials. Showing top 10.")

            # Display results in a table with enhanced properties
            table = Table(show_header=True, header_style="bold magenta")
            table.add_column("Source ID")
            table.add_column("Formula")
            table.add_column("Elements")
            table.add_column("Band Gap (eV)")
            table.add_column("Formation Energy (eV/atom)")

            # Show only the first 10 results for brevity
            for material in all_materials[:10]:
                attrs = material.get("attributes", {})

                # Helper to gracefully get potentially missing property values
                def get_prop(keys, default="N/A"):
                    for key in keys:
                        if key in attrs and attrs[key] is not None:
                            val = attrs[key]
                            # Format numbers to a reasonable precision
                            return f"{val:.3f}" if isinstance(val, (int, float)) else str(val)
                    return default

                band_gap = get_prop(["band_gap", "_mp_band_gap", "_oqmd_band_gap"])
                formation_energy = get_prop(["formation_energy_per_atom", "_mp_formation_energy_per_atom", "_oqmd_formation_energy_per_atom"])

                table.add_row(
                    str(material.get("id")),
                    attrs.get("chemical_formula_descriptive", "N/A"),
                    ", ".join(attrs.get("elements", [])),
                    band_gap,
                    formation_energy
                )
            console.print(Panel(table, title="Top 10 Search Results", border_style="green"))

            # Prompt user to save results to the database
            if Confirm.ask("Do you want to save these results to the database?"):
                try:
                    db = next(get_db())
                    # Ensure the table exists before trying to save
                    Base.metadata.create_all(bind=engine, checkfirst=True)

                    saved_ids = set()
                    with console.status("[bold green]Saving to database...[/bold green]"):
                        for material in all_materials:
                            source_id = material["id"]
                            # Skip duplicates within the current result set
                            if source_id in saved_ids:
                                continue

                            attrs = material.get("attributes", {})
                            # Check if the material is already in the database to avoid duplicates
                            existing = db.query(Material).filter_by(source_id=source_id).first()
                            if not existing:
                                db_material = Material(
                                    source_id=source_id,
                                    formula=attrs.get("chemical_formula_descriptive"),
                                    elements=",".join(attrs.get("elements", [])),
                                    provider=material.get("meta", {}).get("provider", {}).get("name", "N/A")
                                )
                                db.add(db_material)
                                saved_ids.add(source_id)
                        db.commit()
                    console.print("[green]SUCCESS:[/green] Results saved to the database.")
                    # Inform the user where the database is located
                    db_path = os.path.abspath(engine.url.database)
                    console.print(f"Database located at: [green]{db_path}[/green]")

                except Exception as e:
                    console.print(f"[bold red]An unexpected error occurred during save: {e}[/bold red]")
        else:
            console.print("[red]ERROR:[/red] No materials found for the given filter.")

    except Exception as e:
        console.print(f"[bold red]An error occurred during search: {e}[/bold red]")
