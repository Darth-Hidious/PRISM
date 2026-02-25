"""Configure CLI command: manage PRISM settings and API keys."""
from pathlib import Path

import click
from rich.console import Console
from rich.prompt import Confirm


try:
    from mp_api.client import MPRester
    MP_API_AVAILABLE = True
except ImportError:
    MP_API_AVAILABLE = False


@click.command()
@click.option('--mp-api-key', help='Set Materials Project API key')
@click.option('--list-config', is_flag=True, help='List current configuration')
@click.option('--reset', is_flag=True, help='Reset configuration to defaults')
def configure(mp_api_key: str, list_config: bool, reset: bool):
    """
    Configure PRISM settings and API keys.

    This command allows you to set API keys and other configuration options
    for PRISM. Configuration is stored in the .env file.
    """
    console = Console(force_terminal=True, width=120)
    env_file = Path('.env')

    if reset:
        if Confirm.ask("[yellow]Are you sure you want to reset all configuration?[/yellow]"):
            # Create backup
            if env_file.exists():
                backup_file = Path('.env.backup')
                env_file.rename(backup_file)
                console.print(f"[green]Configuration backed up to {backup_file}[/green]")

            # Create new minimal .env
            with open(env_file, 'w') as f:
                f.write("# PRISM Configuration\n")
                f.write("# Add your API keys and settings here\n\n")
            console.print("[green]Configuration reset to defaults[/green]")
        return

    if list_config:
        console.print("[bold cyan]Current PRISM Configuration:[/bold cyan]")

        if env_file.exists():
            with open(env_file, 'r') as f:
                content = f.read()

            # Extract and display key settings (mask sensitive values)
            lines = content.split('\n')
            config_found = False

            for line in lines:
                if line.strip() and not line.startswith('#'):
                    if '=' in line:
                        key, value = line.split('=', 1)
                        key = key.strip()
                        value = value.strip()

                        # Mask sensitive values
                        if 'api_key' in key.lower() or 'password' in key.lower() or 'secret' in key.lower():
                            if value:
                                masked_value = value[:4] + '*' * (len(value) - 8) + value[-4:] if len(value) > 8 else '*' * len(value)
                                console.print(f"  {key}: {masked_value}")
                            else:
                                console.print(f"  {key}: [red]Not set[/red]")
                        else:
                            console.print(f"  {key}: {value}")
                        config_found = True

            if not config_found:
                console.print("[yellow]No configuration found[/yellow]")
        else:
            console.print("[yellow]No .env file found[/yellow]")
        return

    # Set MP API key
    if mp_api_key:
        # Read existing .env file
        config_lines = []
        if env_file.exists():
            with open(env_file, 'r') as f:
                config_lines = f.readlines()

        # Update or add MP API key
        mp_key_found = False
        for i, line in enumerate(config_lines):
            if line.startswith('MATERIALS_PROJECT_API_KEY='):
                config_lines[i] = f'MATERIALS_PROJECT_API_KEY={mp_api_key}\n'
                mp_key_found = True
                break

        if not mp_key_found:
            config_lines.append(f'MATERIALS_PROJECT_API_KEY={mp_api_key}\n')

        # Write back to file
        with open(env_file, 'w') as f:
            f.writelines(config_lines)

        console.print("[green]Materials Project API key configured successfully[/green]")

        # Test the API key
        try:
            if MP_API_AVAILABLE:
                with MPRester(mp_api_key) as mpr:
                    # Simple test query
                    test_data = mpr.materials.summary.search(
                        material_ids=['mp-1'],
                        fields=['material_id']
                    )
                    if test_data:
                        console.print("[green]✓ API key validated successfully[/green]")
                    else:
                        console.print("[yellow]⚠ API key set but validation failed[/yellow]")
            else:
                console.print("[yellow]⚠ Materials Project API not available for validation[/yellow]")
        except Exception as e:
            console.print(f"[yellow]⚠ API key set but validation failed: {str(e)[:50]}[/yellow]")

        return

    # If no options provided, show help
    console.print("[cyan]Use --help to see available configuration options[/cyan]")
    console.print("[cyan]Examples:[/cyan]")
    console.print("  prism configure --mp-api-key YOUR_KEY_HERE")
    console.print("  prism configure --list-config")
    console.print("  prism configure --reset")
