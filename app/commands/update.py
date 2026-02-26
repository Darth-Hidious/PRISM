"""Update CLI command: check for PRISM updates and upgrade."""
import click
from rich.console import Console
from rich.panel import Panel


@click.command()
@click.option("--check-only", is_flag=True, help="Only check, don't upgrade")
@click.option("--yes", "-y", is_flag=True, help="Skip confirmation prompt")
def update(check_only, yes):
    """Check for PRISM updates and upgrade if available."""
    from app import __version__
    from app.update import (
        check_for_updates, detect_install_method, upgrade_command,
        download_tui_binary, run_upgrade, CACHE_PATH,
    )

    console = Console()
    method = detect_install_method()
    console.print(f"[dim]Current version: v{__version__}[/dim]")
    console.print(f"[dim]Install method: {method}[/dim]")
    console.print("[dim]Checking for updates...[/dim]")

    # Clear cache to force a fresh check
    try:
        CACHE_PATH.unlink(missing_ok=True)
    except Exception:
        pass

    update_info = check_for_updates(__version__)
    if not update_info:
        console.print(f"[green]You are running the latest version (v{__version__}).[/green]")
        # Still update the TUI binary in case it's missing
        _ensure_tui_binary(console, download_tui_binary)
        return

    if check_only:
        console.print(f"[yellow]Update available: v{update_info['latest']}[/yellow]")
        console.print(f"Upgrade with: [cyan]{update_info['upgrade_cmd']}[/cyan]")
        return

    # Show what's available and offer to upgrade
    console.print(
        Panel(
            f"[bold yellow]New version available: v{update_info['latest']}[/bold yellow]\n\n"
            f"You are running v{update_info['current']}.",
            title="Update Available",
            border_style="yellow",
        )
    )

    if not yes:
        if not click.confirm("Upgrade now?", default=True):
            console.print(f"[dim]Manual upgrade: {update_info['upgrade_cmd']}[/dim]")
            return

    # Run the upgrade
    console.print(f"[dim]Running: {update_info['upgrade_cmd']}[/dim]")
    if run_upgrade(method):
        console.print(f"[green]Upgraded to v{update_info['latest']}[/green]")
    else:
        console.print("[red]Upgrade failed.[/red]")
        console.print(f"[dim]Try manually: {update_info['upgrade_cmd']}[/dim]")
        return

    # Download/update the TUI binary
    _ensure_tui_binary(console, download_tui_binary)


def _ensure_tui_binary(console, download_fn):
    """Download the TUI binary if not already present."""
    from app.cli._binary import has_tui_binary

    if has_tui_binary():
        # Re-download to get latest version
        console.print("[dim]Updating TUI frontend...[/dim]")
        path = download_fn()
        if path:
            console.print(f"[green]TUI frontend updated ({path})[/green]")
        else:
            console.print("[dim]TUI frontend already up to date.[/dim]")
    else:
        console.print("[dim]Downloading TUI frontend...[/dim]")
        path = download_fn()
        if path:
            console.print(f"[green]TUI frontend installed ({path})[/green]")
        else:
            console.print("[dim]TUI binary not available for this platform (Rich UI will be used).[/dim]")
