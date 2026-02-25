"""Configure CLI command: manage PRISM API keys and settings."""
import os
from pathlib import Path

import click
from rich.console import Console
from rich.table import Table


# Key name → env var mapping
_KEY_MAP = {
    "anthropic": "ANTHROPIC_API_KEY",
    "openai": "OPENAI_API_KEY",
    "openrouter": "OPENROUTER_API_KEY",
    "mp": "MATERIALS_PROJECT_API_KEY",
    "labs": "PRISM_LABS_API_KEY",
}


def _get_env_path() -> Path:
    """Find or create the .env file."""
    from app.config.settings import get_env_path
    return get_env_path()


def _read_env(env_path: Path) -> dict:
    """Read .env file into a dict."""
    result = {}
    if env_path.exists():
        for line in env_path.read_text().splitlines():
            stripped = line.strip()
            if stripped and not stripped.startswith("#") and "=" in stripped:
                k, v = stripped.split("=", 1)
                result[k.strip()] = v.strip()
    return result


def _write_env(env_path: Path, data: dict):
    """Write dict back to .env file."""
    lines = ["# PRISM Environment Configuration"]
    for k, v in data.items():
        lines.append(f"{k}={v}")
    env_path.parent.mkdir(parents=True, exist_ok=True)
    env_path.write_text("\n".join(lines) + "\n")


def _set_key(env_path: Path, env_var: str, value: str, console: Console):
    """Set a key in .env and os.environ."""
    data = _read_env(env_path)
    data[env_var] = value
    _write_env(env_path, data)
    os.environ[env_var] = value


def _mask(value: str) -> str:
    """Mask a secret value for display."""
    if len(value) > 8:
        return value[:4] + "*" * (len(value) - 8) + value[-4:]
    return "*" * len(value)


@click.command()
@click.option("--anthropic-key", help="Set Anthropic (Claude) API key")
@click.option("--openai-key", help="Set OpenAI API key")
@click.option("--openrouter-key", help="Set OpenRouter API key")
@click.option("--mp-api-key", help="Set Materials Project API key")
@click.option("--labs-key", help="Set PRISM Labs marketplace API key")
@click.option("--model", "default_model", help="Set default LLM model (e.g. claude-sonnet-4-20250514)")
@click.option("--show", "list_config", is_flag=True, help="Show current configuration")
@click.option("--list-config", "list_config", is_flag=True, hidden=True, help="Alias for --show")
@click.option("--reset", is_flag=True, help="Reset configuration to defaults")
def configure(anthropic_key, openai_key, openrouter_key, mp_api_key, labs_key,
              default_model, list_config, reset):
    """Configure PRISM API keys, model defaults, and settings.

    \b
    Examples:
      prism configure --show
      prism configure --anthropic-key sk-ant-...
      prism configure --mp-api-key YOUR_KEY
      prism configure --model claude-sonnet-4-20250514
      prism configure --labs-key YOUR_KEY
      prism configure --reset
    """
    console = Console()
    env_path = _get_env_path()

    if reset:
        from rich.prompt import Confirm
        if Confirm.ask("[yellow]Reset all configuration? (backup will be saved)[/yellow]"):
            if env_path.exists():
                backup = env_path.with_suffix(".env.backup")
                env_path.rename(backup)
                console.print(f"[dim]Backup saved to {backup}[/dim]")
            _write_env(env_path, {})
            # Reset preferences
            from app.config.preferences import UserPreferences
            prefs = UserPreferences()
            prefs.onboarding_complete = True  # don't re-trigger onboarding
            prefs.save()
            console.print("[green]Configuration reset to defaults.[/green]")
        return

    if list_config:
        _show_config(console, env_path)
        return

    # Handle key setting
    any_set = False
    keys_to_set = [
        (anthropic_key, "ANTHROPIC_API_KEY", "Anthropic"),
        (openai_key, "OPENAI_API_KEY", "OpenAI"),
        (openrouter_key, "OPENROUTER_API_KEY", "OpenRouter"),
        (mp_api_key, "MATERIALS_PROJECT_API_KEY", "Materials Project"),
        (labs_key, "PRISM_LABS_API_KEY", "PRISM Labs"),
    ]

    for value, env_var, name in keys_to_set:
        if value:
            _set_key(env_path, env_var, value, console)
            console.print(f"[green]{name} API key configured.[/green]")
            any_set = True

    if default_model:
        _set_key(env_path, "PRISM_DEFAULT_MODEL", default_model, console)
        console.print(f"[green]Default model set to: {default_model}[/green]")
        any_set = True

    # Validate MP key if set
    if mp_api_key:
        _validate_mp_key(mp_api_key, console)

    if any_set:
        console.print(f"[dim]Saved to {env_path}[/dim]")
        return

    # No flags — show current config
    _show_config(console, env_path)


def _show_config(console: Console, env_path: Path):
    """Display current configuration."""
    from app.config.preferences import UserPreferences
    from app.ml.features import get_feature_backend

    console.print("[bold]PRISM Configuration[/bold]")
    console.print()

    # API Keys
    table = Table(title="API Keys", show_lines=False)
    table.add_column("Service")
    table.add_column("Key")
    table.add_column("Status")

    data = _read_env(env_path)

    for name, env_var in [
        ("Anthropic (Claude)", "ANTHROPIC_API_KEY"),
        ("OpenAI", "OPENAI_API_KEY"),
        ("OpenRouter", "OPENROUTER_API_KEY"),
        ("Materials Project", "MATERIALS_PROJECT_API_KEY"),
        ("PRISM Labs", "PRISM_LABS_API_KEY"),
    ]:
        val = data.get(env_var) or os.getenv(env_var, "")
        if val:
            table.add_row(name, _mask(val), "[green]set[/green]")
        else:
            table.add_row(name, "[dim]not set[/dim]", "[dim]—[/dim]")

    console.print(table)

    # Default model
    model = data.get("PRISM_DEFAULT_MODEL") or os.getenv("PRISM_DEFAULT_MODEL", "")
    if model:
        console.print(f"\n[bold]Default model:[/bold] {model}")
    console.print(f"[bold]Feature backend:[/bold] {get_feature_backend()}")

    # Preferences
    prefs = UserPreferences.load()
    console.print()
    prefs_table = Table(title="Workflow Preferences", show_lines=False)
    prefs_table.add_column("Setting")
    prefs_table.add_column("Value")
    prefs_table.add_row("Output format", prefs.output_format)
    prefs_table.add_row("Default providers", ", ".join(prefs.default_providers))
    prefs_table.add_row("Max results/source", str(prefs.max_results_per_source))
    prefs_table.add_row("ML algorithm", prefs.default_algorithm)
    prefs_table.add_row("Report format", prefs.report_format)
    prefs_table.add_row("Compute budget", prefs.compute_budget)
    if prefs.compute_budget == "hpc":
        prefs_table.add_row("HPC queue", prefs.hpc_queue)
        prefs_table.add_row("HPC cores", str(prefs.hpc_cores))
    console.print(prefs_table)

    # Capabilities summary
    console.print()
    try:
        from app.tools.capabilities import capabilities_summary
        console.print("[bold]Available Resources[/bold]")
        console.print(f"[dim]{capabilities_summary()}[/dim]")
    except Exception:
        pass

    console.print()
    console.print("[dim]Edit: prism configure --anthropic-key KEY | prism setup[/dim]")
    console.print(f"[dim]Env file: {env_path}[/dim]")


def _validate_mp_key(key: str, console: Console):
    """Attempt to validate the Materials Project API key."""
    try:
        from mp_api.client import MPRester
        with MPRester(key) as mpr:
            test = mpr.materials.summary.search(material_ids=["mp-1"], fields=["material_id"])
            if test:
                console.print("[green]MP key validated.[/green]")
            else:
                console.print("[yellow]MP key set but validation returned empty.[/yellow]")
    except ImportError:
        pass  # mp_api not installed, skip validation
    except Exception as e:
        console.print(f"[yellow]MP key set but validation failed: {str(e)[:60]}[/yellow]")
