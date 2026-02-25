"""Setup CLI command: interactive workflow preferences wizard."""
import click
from rich.console import Console
from rich.panel import Panel
from rich.prompt import Prompt, IntPrompt


@click.command()
def setup():
    """Interactive wizard to configure workflow preferences."""
    from app.config.preferences import UserPreferences

    console = Console()
    console.print(Panel(
        "[bold cyan]PRISM Workflow Setup[/bold cyan]\n"
        "Configure defaults for search, prediction, simulation, and reporting.",
        expand=False,
    ))

    prefs = UserPreferences.load()

    # Show current capabilities
    console.print()
    try:
        from app.tools.capabilities import capabilities_summary
        summary = capabilities_summary()
        console.print("[dim]" + summary + "[/dim]")
        console.print()
    except Exception:
        pass

    # Output format
    fmt = Prompt.ask(
        "Output format",
        choices=["csv", "parquet", "both"],
        default=prefs.output_format,
    )
    prefs.output_format = fmt

    # Default providers
    prov_str = Prompt.ask(
        "Default search providers (comma-separated)",
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
        choices=["random_forest", "gradient_boosting", "linear", "xgboost", "lightgbm"],
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

    # Update check preference
    check_str = Prompt.ask(
        "Check for updates on startup",
        choices=["yes", "no"],
        default="yes" if prefs.check_updates else "no",
    )
    prefs.check_updates = check_str == "yes"

    path = prefs.save()

    # Also sync to settings.json for the unified config system
    try:
        from app.config.settings_schema import get_settings, save_global_settings
        settings = get_settings()
        settings.output.format = fmt
        settings.output.report_format = rfmt
        settings.output.directory = prefs.output_dir
        settings.search.default_providers = prefs.default_providers
        settings.search.max_results_per_source = prefs.max_results_per_source
        settings.ml.algorithm = algo
        settings.compute.budget = budget
        if budget == "hpc":
            settings.compute.hpc_queue = prefs.hpc_queue
            settings.compute.hpc_cores = prefs.hpc_cores
        settings.updates.check_on_startup = prefs.check_updates
        save_global_settings(settings)
    except Exception:
        pass

    console.print(f"\n[green]Preferences saved to {path}[/green]")
    console.print("[dim]API keys: prism configure --show | Settings: ~/.prism/settings.json[/dim]")
