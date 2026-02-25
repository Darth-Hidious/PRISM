"""Calphad CLI command group: CALPHAD thermodynamic calculations and database management."""
import click
from rich.console import Console


@click.group("calphad")
def calphad_group():
    """CALPHAD thermodynamic calculations and database management."""
    pass


@calphad_group.command("status")
def calphad_status():
    """Show pycalphad installation status and available databases."""
    console = Console(force_terminal=True, width=120)

    from app.simulation.calphad_bridge import check_calphad_available
    console.print("[bold cyan]CALPHAD Status[/bold cyan]")
    if not check_calphad_available():
        console.print("  pycalphad available: [yellow]no[/yellow]")
        console.print("  Install with: [cyan]pip install prism-platform[calphad][/cyan]")
        return

    console.print("  pycalphad available: [green]yes[/green]")
    from app.simulation.calphad_bridge import get_calphad_bridge
    bridge = get_calphad_bridge()
    databases = bridge.databases.list_databases()
    console.print(f"  databases: [cyan]{len(databases)}[/cyan]")
    for db in databases:
        console.print(f"    [green]{db['name']}[/green] ({db['size_kb']} KB)")


@calphad_group.command("databases")
def calphad_databases():
    """List available TDB thermodynamic database files."""
    console = Console(force_terminal=True, width=120)

    from app.simulation.calphad_bridge import get_calphad_bridge
    bridge = get_calphad_bridge()
    databases = bridge.databases.list_databases()

    if not databases:
        console.print("[dim]No TDB databases found.[/dim]")
        console.print("[dim]Import one with: prism calphad import <path.tdb>[/dim]")
        return

    from rich.table import Table
    table = Table(show_header=True, header_style="bold magenta")
    table.add_column("Name")
    table.add_column("Size (KB)")
    table.add_column("Path")
    for db in databases:
        table.add_row(db["name"], str(db["size_kb"]), db["path"])
    console.print(table)


@calphad_group.command("import")
@click.argument("tdb_path")
@click.option("--name", default=None, help="Name for the database (default: filename)")
def calphad_import(tdb_path, name):
    """Import a TDB thermodynamic database file."""
    console = Console(force_terminal=True, width=120)

    from app.simulation.calphad_bridge import get_calphad_bridge
    bridge = get_calphad_bridge()
    result = bridge.databases.import_database(tdb_path, name)
    if "error" in result:
        console.print(f"[red]{result['error']}[/red]")
    else:
        console.print(f"[green]Imported database: {result['name']}[/green]")
        console.print(f"  Path: {result['path']}")
