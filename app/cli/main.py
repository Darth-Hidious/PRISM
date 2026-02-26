#!/usr/bin/env python3
"""
PRISM Platform Enhanced CLI Tool

A comprehensive command-line interface for materials discovery and database management.
Supports NOMAD, JARVIS, OQMD, COD and custom databases with advanced filtering,
visualization, and export capabilities.
"""

import os
import re
from pathlib import Path
from dotenv import load_dotenv

from app.config.settings import get_env_path
load_dotenv(get_env_path())

import click
from rich.console import Console

from app.config.branding import PRISM_BRAND
from app.agent.factory import create_backend
from app.agent.repl import AgentREPL

# ==============================================================================
# Setup
# ==============================================================================
console = Console(force_terminal=True, width=120)

# ==============================================================================
# Main CLI Group
# ==============================================================================
@click.group(invoke_without_command=True)
@click.pass_context
@click.option('--version', is_flag=True, help='Show version information')
@click.option('--verbose', '-v', is_flag=True, help='Enable verbose output')
@click.option('--quiet', '-q', is_flag=True, help='Suppress non-essential output')
@click.option('--mp-api-key', help='Set Materials Project API key for enhanced properties')
@click.option('--resume', default=None, help='Resume a saved session by SESSION_ID')
@click.option('--no-mcp', is_flag=True, help='Disable loading tools from external MCP servers')
@click.option('--dangerously-accept-all', is_flag=True, help='Auto-approve all tool calls (skip consent prompts)')
@click.option('--classic', is_flag=True, help='Use classic Rich terminal UI')
def cli(ctx, version, verbose, quiet, mp_api_key, resume, no_mcp, dangerously_accept_all, classic):
    f"""
{PRISM_BRAND}
AI-Native Autonomous Materials Discovery

MARC27 — ESA SPARK Prime Contractor | ITER Supplier

PRISM combines LLMs, CALPHAD thermodynamics, ML property prediction,
and federated data access into an autonomous research agent.

  prism                           Interactive agent REPL
  prism run "goal"                Autonomous agent mode
  prism run "goal" --confirm      Autonomous with tool consent
  prism serve                     Start as an MCP server
  prism search --elements Fe,Ni   Structured OPTIMADE search
  prism ask "battery cathodes"    Natural-language query
  prism update                    Check for updates

Documentation: https://github.com/Darth-Hidious/PRISM
    """
    ctx.ensure_object(dict)
    ctx.obj["no_mcp"] = no_mcp
    ctx.obj["dangerously_accept_all"] = dangerously_accept_all

    # Handle MP API key if provided
    if mp_api_key:
        # Store the API key in environment
        os.environ['MATERIALS_PROJECT_API_KEY'] = mp_api_key

        # Update .env file
        env_path = Path('.env')
        if not env_path.exists():
            env_path = Path(__file__).parent.parent / '.env'

        if env_path.exists():
            # Read existing .env content
            content = env_path.read_text()

            # Update or add MP API key
            if 'MATERIALS_PROJECT_API_KEY=' in content:
                # Replace existing key
                content = re.sub(r'MATERIALS_PROJECT_API_KEY=.*', f'MATERIALS_PROJECT_API_KEY={mp_api_key}', content)
            else:
                # Add new key
                content += f'\nMATERIALS_PROJECT_API_KEY={mp_api_key}\n'

            env_path.write_text(content)
            console.print(f"[green]✓ Materials Project API key updated in {env_path}[/green]")
        else:
            console.print(f"[yellow]⚠ No .env file found, but API key set for this session[/yellow]")

    if version:
        console.print(f"[bold cyan]{PRISM_BRAND}[/bold cyan]")
        console.print("[dim]Platform for Research in Intelligent Synthesis of Materials[/dim]")
        from app import __version__
        console.print(f"[dim]Version: {__version__}[/dim]")
        ctx.exit()

    elif ctx.invoked_subcommand is None:
        from app.config.preferences import UserPreferences, run_onboarding

        # First-run onboarding: ask for API keys
        if UserPreferences.is_first_run() and not UserPreferences.has_llm_key():
            try:
                run_onboarding(console)
            except (EOFError, KeyboardInterrupt):
                console.print("\n[dim]Skipped setup. Run 'prism setup' later.[/dim]")
                # Mark onboarding done so we don't keep asking
                prefs = UserPreferences.load()
                prefs.onboarding_complete = True
                prefs.save()

        # Check for updates (reads from settings.json > preferences)
        try:
            from app.config.settings_schema import get_settings
            settings = get_settings()
            if settings.updates.check_on_startup:
                from app import __version__
                from app.update import check_for_updates
                update_info = check_for_updates(__version__)
                if update_info:
                    console.print(
                        f"[yellow]Update available:[/yellow] "
                        f"v{update_info['current']} -> v{update_info['latest']}  "
                        f"[dim]({update_info['upgrade_cmd']})[/dim]"
                    )
        except Exception:
            pass

        # Try Ink TUI binary (unless --classic or binary not found)
        from app.cli._binary import has_tui_binary, tui_binary_path

        if not classic and has_tui_binary():
            import sys
            binary = tui_binary_path()
            args = [str(binary), "--python", sys.executable]
            if dangerously_accept_all:
                args.append("--auto-approve")
            if resume:
                args.extend(["--resume", resume])
            os.execvp(str(binary), args)

        # Launch classic Rich REPL
        try:
            backend = create_backend()
            repl = AgentREPL(backend=backend, enable_mcp=not no_mcp,
                             auto_approve=dangerously_accept_all)
            if resume:
                try:
                    repl._load_session(resume)
                    console.print(f"[green]Resumed session: {resume}[/green]")
                except FileNotFoundError:
                    console.print(f"[red]Session not found: {resume}[/red]")
                    return
            repl.run()
        except ValueError as e:
            # No LLM key configured — offer to set one up
            console.print()
            console.print(f"[yellow]{e}[/yellow]")
            console.print("\nRun [cyan]prism setup[/cyan] to configure an LLM provider, or [cyan]prism --help[/cyan] for all commands.")


# ==============================================================================
# CLI Entry Point — register all subcommands from app/commands/
# ==============================================================================
from app.commands.advanced import advanced
cli.add_command(advanced)

from app.commands.docs import docs
cli.add_command(docs)

from app.commands.optimade import optimade
cli.add_command(optimade)

from app.commands.mcp import mcp_group
cli.add_command(mcp_group, "mcp")

from app.commands.sim import sim_group
cli.add_command(sim_group, "sim")

from app.commands.plugin import plugin_group
cli.add_command(plugin_group, "plugin")

# calphad is deprecated — now under 'prism model calphad'
import click as _click_calphad

@_click_calphad.group("calphad", hidden=True, deprecated=True)
def _calphad_deprecated():
    """Deprecated: use 'prism model calphad' instead."""
    _click_calphad.echo("Warning: 'prism calphad' is deprecated. Use 'prism model calphad' instead.", err=True)

from app.commands.calphad import calphad_group as _old_calphad
for cmd_name, cmd in _old_calphad.commands.items():
    _calphad_deprecated.add_command(cmd, cmd_name)
cli.add_command(_calphad_deprecated, "calphad")

from app.commands.labs import labs_group
cli.add_command(labs_group, "labs")

from app.commands.data import data as data_group
cli.add_command(data_group, "data")

from app.commands.predict import predict as predict_cmd
cli.add_command(predict_cmd, "predict")

from app.commands.model import model as model_group
cli.add_command(model_group, "model")

from app.commands.search import search as search_cmd
cli.add_command(search_cmd, "search")

from app.commands.serve import serve as serve_cmd
cli.add_command(serve_cmd, "serve")

from app.commands.run import run_goal as run_cmd
cli.add_command(run_cmd, "run")

# ask is deprecated — redirect to run
import click as _click

@_click.command("ask", hidden=True, deprecated=True)
@_click.argument("query", required=False)
@_click.pass_context
def _ask_deprecated(ctx, query):
    """Deprecated: use 'prism run' instead."""
    _click.echo("Warning: 'prism ask' is deprecated. Use 'prism run \"query\"' instead.", err=True)
    if query:
        ctx.invoke(run_cmd, goal=query)
    else:
        _click.echo("Usage: prism run \"your question here\"", err=True)
cli.add_command(_ask_deprecated, "ask")

from app.commands.setup import setup as setup_cmd
cli.add_command(setup_cmd, "setup")

from app.commands.update import update as update_cmd
cli.add_command(update_cmd, "update")

from app.commands.configure import configure as configure_cmd
cli.add_command(configure_cmd, "configure")

if __name__ == "__main__":
    cli()
