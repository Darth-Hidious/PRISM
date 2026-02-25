"""Labs CLI command group — premium marketplace tools.

Rarely-used, high-cost, high-value materials science services available
through the PRISM marketplace. All labs tools are plugin-backed — vendors
register on the MARC27 platform, users subscribe and run via CLI.

Categories: A-Labs (autonomous discovery), DfM (design for manufacturing),
Cloud DFT, Quantum Compute, Synchrotron, HT Screening.
"""
import json
import click
from pathlib import Path
from rich.console import Console
from rich.table import Table


_LABS_CATALOG_PATH = Path(__file__).parent.parent / "plugins" / "labs_catalog.json"


def _load_labs_catalog() -> dict:
    """Load the labs service catalog."""
    if not _LABS_CATALOG_PATH.exists():
        return {"services": {}}
    try:
        return json.loads(_LABS_CATALOG_PATH.read_text())
    except Exception:
        return {"services": {}}


@click.group("labs")
def labs_group():
    """Premium marketplace tools — A-Labs, DfM, Cloud DFT, Quantum, and more."""
    pass


@labs_group.command("list")
@click.option("--category", default=None, help="Filter by category (a-labs, dfm, cloud-dft, quantum, synchrotron, ht-screening)")
def labs_list(category):
    """Browse available premium lab services."""
    console = Console()
    catalog = _load_labs_catalog()
    services = catalog.get("services", {})

    if category:
        services = {k: v for k, v in services.items() if v.get("category") == category}

    if not services:
        console.print("[dim]No lab services found for this filter.[/dim]")
        console.print("[dim]Check the PRISM marketplace: https://prism.marc27.com/labs[/dim]")
        return

    table = Table(title="PRISM Labs — Premium Services", show_lines=True)
    table.add_column("Service", style="bold")
    table.add_column("Category")
    table.add_column("Provider")
    table.add_column("Cost Model")
    table.add_column("Status")

    for sid, svc in services.items():
        status = svc.get("status", "coming_soon")
        status_str = {
            "available": "[green]available[/green]",
            "beta": "[yellow]beta[/yellow]",
            "coming_soon": "[dim]coming soon[/dim]",
        }.get(status, f"[dim]{status}[/dim]")
        table.add_row(
            svc.get("name", sid),
            svc.get("category", "?"),
            svc.get("provider", "?"),
            svc.get("cost_model", "?"),
            status_str,
        )

    console.print(table)
    console.print()
    console.print("[dim]Subscribe: prism labs subscribe <service-id>[/dim]")
    console.print("[dim]Marketplace: https://prism.marc27.com/labs[/dim]")


@labs_group.command("status")
def labs_status():
    """Show subscribed lab services and usage."""
    console = Console()

    config_path = Path.home() / ".prism" / "labs_subscriptions.json"
    if not config_path.exists():
        console.print("[dim]No active lab subscriptions.[/dim]")
        console.print("[dim]Browse services: prism labs list[/dim]")
        return

    try:
        subs = json.loads(config_path.read_text())
    except Exception:
        console.print("[red]Failed to read subscriptions.[/red]")
        return

    table = Table(title="Active Lab Subscriptions")
    table.add_column("Service")
    table.add_column("Plan")
    table.add_column("API Key")
    table.add_column("Usage")

    for sub in subs.get("subscriptions", []):
        key_display = sub.get("api_key", "")[:8] + "..." if sub.get("api_key") else "[dim]not set[/dim]"
        table.add_row(
            sub.get("service", "?"),
            sub.get("plan", "?"),
            key_display,
            sub.get("usage_summary", "0 calls"),
        )

    console.print(table)


@labs_group.command("subscribe")
@click.argument("service_id")
@click.option("--plan", default="pay-per-use", help="Subscription plan (pay-per-use, monthly, annual)")
@click.option("--api-key", default=None, help="API key from marketplace")
def labs_subscribe(service_id, plan, api_key):
    """Subscribe to a premium lab service."""
    console = Console()
    catalog = _load_labs_catalog()
    services = catalog.get("services", {})

    if service_id not in services:
        console.print(f"[red]Service '{service_id}' not found.[/red]")
        console.print("[dim]Browse available: prism labs list[/dim]")
        return

    svc = services[service_id]
    if svc.get("status") == "coming_soon":
        console.print(f"[yellow]{svc['name']} is not yet available.[/yellow]")
        console.print(f"[dim]Register interest at: https://prism.marc27.com/labs/{service_id}[/dim]")
        return

    # Save subscription
    config_path = Path.home() / ".prism" / "labs_subscriptions.json"
    config_path.parent.mkdir(parents=True, exist_ok=True)

    existing = {"subscriptions": []}
    if config_path.exists():
        try:
            existing = json.loads(config_path.read_text())
        except Exception:
            pass

    # Check for duplicate
    for sub in existing["subscriptions"]:
        if sub.get("service") == service_id:
            console.print(f"[yellow]Already subscribed to {svc['name']}.[/yellow]")
            return

    existing["subscriptions"].append({
        "service": service_id,
        "name": svc["name"],
        "plan": plan,
        "api_key": api_key,
        "usage_summary": "0 calls",
    })

    config_path.write_text(json.dumps(existing, indent=2))
    console.print(f"[green]Subscribed to {svc['name']}![/green]")
    console.print(f"  Plan: {plan}")
    if not api_key:
        console.print(f"  [yellow]Set API key: prism labs subscribe {service_id} --api-key YOUR_KEY[/yellow]")
        console.print(f"  [dim]Get key at: https://prism.marc27.com/labs/{service_id}[/dim]")


@labs_group.command("info")
@click.argument("service_id")
def labs_info(service_id):
    """Show detailed information about a lab service."""
    console = Console()
    catalog = _load_labs_catalog()
    services = catalog.get("services", {})

    if service_id not in services:
        console.print(f"[red]Service '{service_id}' not found.[/red]")
        return

    svc = services[service_id]
    console.print(f"[bold]{svc.get('name', service_id)}[/bold]")
    console.print(f"  Category: {svc.get('category', '?')}")
    console.print(f"  Provider: {svc.get('provider', '?')}")
    console.print(f"  Cost: {svc.get('cost_model', '?')}")
    console.print()
    console.print(svc.get("description", "No description available."))

    if svc.get("capabilities"):
        console.print()
        console.print("[bold]Capabilities:[/bold]")
        for cap in svc["capabilities"]:
            console.print(f"  - {cap}")

    if svc.get("requirements"):
        console.print()
        console.print("[bold]Requirements:[/bold]")
        for req in svc["requirements"]:
            console.print(f"  - {req}")

    console.print()
    console.print(f"[dim]Marketplace: https://prism.marc27.com/labs/{service_id}[/dim]")
