"""Update CLI command: check for PRISM updates."""
import click
from rich.console import Console
from rich.panel import Panel


@click.command()
def update():
    """Check for PRISM updates and show upgrade instructions."""
    from app import __version__
    from app.update import check_for_updates, CACHE_PATH

    console = Console(force_terminal=True, width=120)
    console.print(f"[dim]Current version: {__version__}[/dim]")
    console.print("[dim]Checking for updates...[/dim]")

    # Clear cache to force a fresh check
    try:
        CACHE_PATH.unlink(missing_ok=True)
    except Exception:
        pass

    update_info = check_for_updates(__version__)
    if update_info:
        console.print(
            Panel(
                f"[bold yellow]New version available: v{update_info['latest']}[/bold yellow]\n\n"
                f"You are running v{update_info['current']}.\n\n"
                f"Upgrade with:\n  [cyan]{update_info['upgrade_cmd']}[/cyan]",
                title="Update Available",
                border_style="yellow",
            )
        )
    else:
        console.print(f"[green]You are running the latest version (v{__version__}).[/green]")
