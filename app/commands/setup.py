"""Setup CLI command: interactive workflow preferences wizard."""
import click
from rich.console import Console
from rich.panel import Panel
from rich.prompt import Prompt, IntPrompt


@click.command()
def setup():
    """Interactive wizard to configure workflow preferences."""
    from app.config.preferences import UserPreferences

    console = Console(force_terminal=True, width=120)
    console.print(Panel("[bold cyan]PRISM Workflow Setup[/bold cyan]\nConfigure defaults for skills and workflows.", expand=False))

    prefs = UserPreferences.load()

    # Output format
    fmt = Prompt.ask(
        "Output format",
        choices=["csv", "parquet", "both"],
        default=prefs.output_format,
    )
    prefs.output_format = fmt

    # Default providers
    prov_str = Prompt.ask(
        "Default data providers (comma-separated)",
        default=",".join(prefs.default_providers),
    )
    prefs.default_providers = [p.strip() for p in prov_str.split(",") if p.strip()]

    # Max results
    prefs.max_results_per_source = IntPrompt.ask(
        "Max results per source", default=prefs.max_results_per_source
    )

    # ML algorithm
    algo = Prompt.ask(
        "Default ML algorithm",
        choices=["random_forest", "gradient_boosting", "linear"],
        default=prefs.default_algorithm,
    )
    prefs.default_algorithm = algo

    # Report format
    rfmt = Prompt.ask(
        "Report format",
        choices=["markdown", "pdf"],
        default=prefs.report_format,
    )
    prefs.report_format = rfmt

    # Compute budget
    budget = Prompt.ask(
        "Compute budget",
        choices=["local", "hpc"],
        default=prefs.compute_budget,
    )
    prefs.compute_budget = budget

    if budget == "hpc":
        prefs.hpc_queue = Prompt.ask("HPC queue name", default=prefs.hpc_queue)
        prefs.hpc_cores = IntPrompt.ask("HPC cores", default=prefs.hpc_cores)

    path = prefs.save()
    console.print(f"\n[green]Preferences saved to {path}[/green]")
