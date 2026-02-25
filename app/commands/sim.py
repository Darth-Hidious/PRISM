"""Sim CLI command group: atomistic simulation commands (pyiron)."""
import click
from rich.console import Console


@click.group("sim")
def sim_group():
    """Atomistic simulation commands (pyiron)."""
    pass


@sim_group.command("status")
def sim_status():
    """Show pyiron configuration, available codes, and job counts."""
    console = Console(force_terminal=True, width=120)

    from app.simulation.bridge import check_pyiron_available
    if not check_pyiron_available():
        console.print("[yellow]pyiron_atomistics is not installed.[/yellow]")
        console.print("Install with: [cyan]pip install prism-platform[simulation][/cyan]")
        return

    from app.simulation.bridge import get_bridge
    bridge = get_bridge()

    console.print("[bold cyan]Pyiron Simulation Status[/bold cyan]")
    console.print(f"  pyiron available: [green]yes[/green]")

    try:
        pr = bridge.get_project()
        console.print(f"  project: [green]{pr.path}[/green]")
    except Exception as e:
        console.print(f"  project: [red]error — {e}[/red]")

    # Show HPC config
    hpc = bridge.load_hpc_config()
    if hpc:
        console.print(f"  HPC: [green]{hpc.get('queue_system', 'N/A')} / {hpc.get('queue_name', 'N/A')}[/green]")
    else:
        console.print("  HPC: [dim]not configured[/dim]")

    # Show job counts
    summaries = bridge.jobs.to_summary_list()
    console.print(f"  jobs in memory: [cyan]{len(summaries)}[/cyan]")
    structs = bridge.structures.to_summary_list()
    console.print(f"  structures in memory: [cyan]{len(structs)}[/cyan]")


@sim_group.command("jobs")
@click.option("--status", default=None, help="Filter by status")
def sim_jobs(status):
    """List recent simulation jobs."""
    console = Console(force_terminal=True, width=120)

    from app.simulation.bridge import check_pyiron_available
    if not check_pyiron_available():
        console.print("[yellow]pyiron_atomistics is not installed.[/yellow]")
        return

    from app.simulation.bridge import get_bridge
    bridge = get_bridge()
    summaries = bridge.jobs.to_summary_list()
    if status:
        summaries = [s for s in summaries if s["status"] == status]

    if not summaries:
        console.print("[dim]No simulation jobs found.[/dim]")
        return

    from rich.table import Table
    table = Table(show_header=True, header_style="bold magenta")
    table.add_column("Job ID")
    table.add_column("Code")
    table.add_column("Status")
    for s in summaries:
        table.add_row(s["id"], s["code"], s["status"])
    console.print(table)


@sim_group.command("init")
@click.option("--name", default="prism_default", help="Project name")
def sim_init(name):
    """Initialize a pyiron project directory."""
    console = Console(force_terminal=True, width=120)

    from app.simulation.bridge import check_pyiron_available
    if not check_pyiron_available():
        console.print("[yellow]pyiron_atomistics is not installed.[/yellow]")
        console.print("Install with: [cyan]pip install prism-platform[simulation][/cyan]")
        return

    from app.simulation.bridge import PyironBridge
    bridge = PyironBridge(project_name=name)
    try:
        pr = bridge.get_project()
        console.print(f"[green]Pyiron project initialised:[/green] {pr.path}")
    except Exception as e:
        console.print(f"[red]Failed to initialise project: {e}[/red]")
