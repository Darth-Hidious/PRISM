#!/usr/bin/env python3
"""
PRISM Platform Enhanced CLI Tool

A comprehensive command-line interface for materials discovery and database management.
Supports NOMAD, JARVIS, OQMD, COD and custom databases with advanced filtering,
visualization, and export capabilities.
"""

import os
import re
import json
import sys
from pathlib import Path
from dotenv import load_dotenv

from app.config.settings import get_env_path
load_dotenv(get_env_path())

# Windows console encoding will be handled by Rich library fallbacks

import click
from rich.console import Console
from rich.panel import Panel
from rich.prompt import Prompt, Confirm, IntPrompt
from rich.table import Table
from sqlalchemy.exc import OperationalError
from optimade.client import OptimadeClient

from app.config.branding import PRISM_BRAND
from app.config.providers import FALLBACK_PROVIDERS

# Make database imports optional
try:
    from app.db.database import Base, engine, get_db
    from app.db.models import Material
    DB_AVAILABLE = True
except ImportError:
    DB_AVAILABLE = False

from app.llm import get_llm_service
from app.mcp import ModelContext, AdaptiveOptimadeFilter
from app.prompts import ROUTER_PROMPT, SUMMARIZATION_PROMPT
from app.agent.factory import create_backend
from app.agent.repl import AgentREPL
from app.agent.autonomous import run_autonomous, run_autonomous_stream

try:
    from mp_api.client import MPRester
    MP_API_AVAILABLE = True
except ImportError:
    MP_API_AVAILABLE = False

# ==============================================================================
# Helper Functions
# ==============================================================================

def enrich_materials_with_mp_data(materials, console=None, mp_api_key=None):
    """
    Enrich OPTIMADE materials with Materials Project native API data.
    Returns the enriched materials with formation energy and band gap data.
    """
    if not MP_API_AVAILABLE:
        if console:
            console.print("[yellow]Materials Project API not available. Using OPTIMADE data only.[/yellow]")
        return materials
    
    # Use provided key or fall back to environment variable
    if not mp_api_key:
        mp_api_key = os.getenv('MATERIALS_PROJECT_API_KEY')
    
    if console:
        console.print(f"[dim]Checking for MP API key... {'Found' if mp_api_key else 'Not found'}[/dim]")
    
    if not mp_api_key:
        if console:
            console.print("[yellow]No Materials Project API key found. Using OPTIMADE data only.[/yellow]")
        return materials
    
    try:
        with MPRester(mp_api_key) as mpr:
            # Extract MP IDs from the materials
            mp_ids = []
            for material in materials:
                material_id = material.get('id', '')
                # Convert to string and check if it's a Materials Project ID
                material_id_str = str(material_id)
                if material_id_str.startswith('mp-'):
                    mp_ids.append(material_id_str)
            
            if not mp_ids:
                return materials
            
            if console:
                console.print(f"[dim]Enriching {len(mp_ids)} Materials Project entries with native API data...[/dim]")
            
            # Fetch properties from MP native API
            mp_data = mpr.materials.summary.search(
                material_ids=mp_ids,
                fields=['material_id', 'formation_energy_per_atom', 'band_gap', 'energy_above_hull']
            )
            
            # Create a lookup dictionary
            mp_lookup = {doc.material_id: doc for doc in mp_data}
            
            # Enrich the materials
            enriched_materials = []
            for material in materials:
                enriched_material = material.copy()
                material_id = str(material.get('id', ''))
                
                if material_id in mp_lookup:
                    mp_doc = mp_lookup[material_id]
                    attrs = enriched_material.setdefault('attributes', {})
                    
                    # Add MP native API data
                    if mp_doc.formation_energy_per_atom is not None:
                        attrs['_mp_formation_energy_per_atom'] = mp_doc.formation_energy_per_atom
                    if mp_doc.band_gap is not None:
                        attrs['_mp_band_gap'] = mp_doc.band_gap
                    if mp_doc.energy_above_hull is not None:
                        attrs['_mp_e_above_hull'] = mp_doc.energy_above_hull
                
                enriched_materials.append(enriched_material)
            
            return enriched_materials
            
    except Exception as e:
        if console:
            console.print(f"[yellow]Warning: Could not fetch MP native data. Error: {str(e)[:100]}[/yellow]")
        return materials

# ==============================================================================
# Setup
# ==============================================================================
console = Console(force_terminal=True, width=120)

# ==============================================================================
# Helper Functions
# ==============================================================================
def build_optimade_filter(elements=None, formula=None, nelements=None):
    """
    Constructs a valid OPTIMADE filter string from the provided search parameters.

    Args:
        elements (list, optional): A list of chemical element symbols.
        formula (str, optional): A chemical formula.
        nelements (int, optional): The number of elements in the material.

    Returns:
        str: A string formatted for use as an OPTIMADE filter.
    """
    filters = []
    if elements:
        elements_str = ", ".join(f'"{e.strip()}"' for e in elements)
        filters.append(f"elements HAS ALL {elements_str}")
    if formula:
        filters.append(f'chemical_formula_descriptive="{formula}"')
    if nelements:
        filters.append(f"nelements={nelements}")
    return " AND ".join(filters)

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
def cli(ctx, version, verbose, quiet, mp_api_key, resume, no_mcp):
    f"""
{PRISM_BRAND}
Platform for Research in Intelligent Synthesis of Materials

A next-generation command-line interface for materials science research, 
powered by the OPTIMADE API network and Large Language Models.

🚀 KEY FEATURES:
• Natural language queries with AI-powered reasoning  
• Access to 15+ materials databases via OPTIMADE
• Enhanced properties from Materials Project API
• Structured searches with advanced filtering
• Rich data visualization and export capabilities

🔧 COMMON USAGE:
• prism ask "Materials for high neutron flux" --limit 10
• prism search --elements Fe,Ni --nelements 5 --mp-api-key YOUR_KEY
• prism --mp-api-key YOUR_API_KEY ask "High entropy alloys"
• prism configure --mp-api-key YOUR_API_KEY
• prism optimade list-dbs

📖 DOCUMENTATION: https://github.com/Darth-Hideous/PRISM

Running PRISM without any subcommands will start the interactive 'ask' mode.
    """
    ctx.ensure_object(dict)
    ctx.obj["no_mcp"] = no_mcp

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
                import re
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
        try:
            backend = create_backend()
            repl = AgentREPL(backend=backend, enable_mcp=not no_mcp)
            if resume:
                try:
                    repl._load_session(resume)
                    console.print(f"[green]Resumed session: {resume}[/green]")
                except FileNotFoundError:
                    console.print(f"[red]Session not found: {resume}[/red]")
                    return
            repl.run()
        except ValueError as e:
            # Fall back to showing help if no agent provider configured
            console.print(PRISM_BRAND)
            console.print(f"[yellow]{e}[/yellow]")
            console.print("\nRun [cyan]prism --help[/cyan] for available commands.")

# ==============================================================================
# 'serve' Command (MCP server mode)
# ==============================================================================
@cli.command("serve")
@click.option("--transport", default="stdio", type=click.Choice(["stdio", "http"]),
              help="MCP transport (stdio for Claude Desktop, http for web)")
@click.option("--port", default=8000, type=int, help="HTTP port (only for http transport)")
@click.option("--install", is_flag=True, help="Print Claude Desktop configuration JSON and exit")
def serve(transport, port, install):
    """Start PRISM as an MCP server for external LLM hosts."""
    if install:
        from app.mcp_server import generate_claude_desktop_config
        config = generate_claude_desktop_config()
        console.print(json.dumps(config, indent=2))
        return

    from app.mcp_server import create_mcp_server
    server = create_mcp_server()
    if transport == "http":
        console.print(f"[bold cyan]PRISM MCP Server[/bold cyan] starting on http://localhost:{port}/mcp", err=True)
        server.run(transport="streamable-http", port=port)
    else:
        console.print("[bold cyan]PRISM MCP Server[/bold cyan] starting on stdio", err=True)
        server.run(transport="stdio")


# ==============================================================================
# 'run' Command (autonomous agent mode)
# ==============================================================================
@cli.command("run")
@click.argument("goal")
@click.option("--provider", default=None, help="LLM provider (anthropic/openai/openrouter)")
@click.option("--model", default=None, help="Model name override")
@click.pass_context
def run_goal(ctx, goal, provider, model):
    """Run PRISM agent autonomously on a research goal."""
    from rich.live import Live
    from rich.markdown import Markdown
    from rich.text import Text
    from app.agent.events import TextDelta, ToolCallStart, ToolCallResult, TurnComplete
    no_mcp = ctx.obj.get("no_mcp", False) if ctx.obj else False
    run_console = Console()
    try:
        backend = create_backend(provider=provider, model=model)
        run_console.print(Panel.fit(f"[bold]Goal:[/bold] {goal}", border_style="cyan"))
        accumulated_text = ""
        with Live("", console=run_console, refresh_per_second=15, vertical_overflow="visible") as live:
            for event in run_autonomous_stream(goal=goal, backend=backend, enable_mcp=not no_mcp):
                if isinstance(event, TextDelta):
                    accumulated_text += event.text
                    live.update(Text(accumulated_text))
                elif isinstance(event, ToolCallStart):
                    live.update("")
                    run_console.print(Panel(
                        f"[dim]Calling...[/dim]",
                        title=f"[bold yellow]{event.tool_name}[/bold yellow]",
                        border_style="yellow",
                        expand=False,
                    ))
                    accumulated_text = ""
                elif isinstance(event, ToolCallResult):
                    run_console.print(Panel(
                        f"[green]{event.summary}[/green]",
                        title=f"[bold green]{event.tool_name}[/bold green]",
                        border_style="green",
                        expand=False,
                    ))
                elif isinstance(event, TurnComplete):
                    live.update("")
        if accumulated_text:
            run_console.print()
            run_console.print(Markdown(accumulated_text))
    except ValueError as e:
        run_console.print(f"[red]Error: {e}[/red]")
    except Exception as e:
        run_console.print(f"[red]Agent error: {e}[/red]")

# ==============================================================================
# 'switch-llm' Command
# ==============================================================================
@cli.command("switch-llm")
def switch_llm():
    """
    Quick switch between configured LLM providers.
    """
    console.print("[bold cyan]LLM Provider Switcher[/bold cyan]")
    
    # Check what providers are configured
    configured_providers = []
    provider_mapping = {}
    
    if os.getenv("OPENAI_API_KEY"):
        configured_providers.append("OpenAI")
        provider_mapping["1"] = ("OPENAI_API_KEY", "OpenAI")
    if os.getenv("GOOGLE_CLOUD_PROJECT"):
        configured_providers.append("Google Vertex AI") 
        provider_mapping[str(len(configured_providers))] = ("GOOGLE_CLOUD_PROJECT", "Google Vertex AI")
    if os.getenv("ANTHROPIC_API_KEY"):
        configured_providers.append("Anthropic")
        provider_mapping[str(len(configured_providers))] = ("ANTHROPIC_API_KEY", "Anthropic")
    if os.getenv("OPENROUTER_API_KEY"):
        configured_providers.append("OpenRouter")
        provider_mapping[str(len(configured_providers))] = ("OPENROUTER_API_KEY", "OpenRouter")
    
    if not configured_providers:
        console.print("[red]No LLM providers are configured.[/red]")
        console.print("[yellow]Run 'prism advanced configure' to set up providers.[/yellow]")
        return
    
    if len(configured_providers) == 1:
        console.print(f"[yellow]Only one provider configured:[/yellow] [cyan]{configured_providers[0]}[/cyan]")
        console.print("[dim]Configure additional providers with 'prism advanced configure' to enable switching.[/dim]")
        return
    
    # Show current provider
    try:
        current_service = get_llm_service()
        if os.getenv("OPENAI_API_KEY"):
            current = f"OpenAI ({current_service.model})"
        elif os.getenv("GOOGLE_CLOUD_PROJECT"):
            current = f"Google Vertex AI ({current_service.model})"
        elif os.getenv("ANTHROPIC_API_KEY"):
            current = f"Anthropic ({current_service.model})"
        elif os.getenv("OPENROUTER_API_KEY"):
            current = f"OpenRouter ({current_service.model})"
        else:
            current = "Unknown"
        console.print(f"[green]Current provider:[/green] [cyan]{current}[/cyan]")
    except:
        console.print("[yellow]Could not determine current provider[/yellow]")
    
    # Show available providers
    console.print(f"\n[bold green]Configured Providers:[/bold green]")
    for i, (choice, (env_var, name)) in enumerate(provider_mapping.items(), 1):
        console.print(f"{choice}. [cyan]{name}[/cyan]")
    
    # Let user choose
    try:
        choice = Prompt.ask(f"\nSelect provider (1-{len(provider_mapping)}) or 'q' to quit", default="q")
        if choice.lower() == 'q':
            return
        
        if choice in provider_mapping:
            env_var, provider_name = provider_mapping[choice]
            console.print(f"[green]✓ Switched to {provider_name}[/green]")
            console.print("[dim]Note: This shows what would happen. Actual switching between configured providers is automatic based on environment variables.[/dim]")
        else:
            console.print("[red]Invalid choice.[/red]")
    
    except KeyboardInterrupt:
        console.print("\n[yellow]Switch cancelled.[/yellow]")

# ==============================================================================
# 'search' Command
# ==============================================================================
@cli.command()
@click.option('--elements', help='Comma-separated list of elements (e.g., "Si,O").')
@click.option('--formula', help='Chemical formula (e.g., "SiO2").')
@click.option('--nelements', type=int, help='Number of elements in the material.')
@click.option('--providers', help='Comma-separated list of provider IDs (e.g., "mp,oqmd,cod").')
@click.option('--limit', type=int, default=1000, help='Maximum number of results to retrieve per provider (default: 1000).')
@click.option('--mp-api-key', help='Materials Project API key for enhanced properties (overrides environment variable).')
def search(elements, formula, nelements, providers, limit, mp_api_key):
    """
    Performs a structured search of the OPTIMADE network based on specific criteria.
    """
    # Ensure at least one search criterion is provided
    if not any([elements, formula, nelements]):
        console.print("[red]Error: Please provide at least one search criterion (e.g., --elements, --formula).[/red]")
        return
    
    # Construct the filter string directly
    filters = []
    if elements:
        elements_str = ", ".join(f'"{e.strip()}"' for e in elements.split(','))
        filters.append(f"elements HAS ALL {elements_str}")
    if formula:
        filters.append(f'chemical_formula_descriptive="{formula}"')
    if nelements:
        filters.append(f"nelements={nelements}")
    optimade_filter = " AND ".join(filters)
    
    console.print(Panel(f"[bold]Filter:[/bold] [cyan]{optimade_filter}[/cyan]", title="Search Query", border_style="blue"))

    try:
        with console.status("[bold green]Querying OPTIMADE providers...[/bold green]"):
            # If specific providers are requested, use them. Otherwise, search all.
            client = OptimadeClient(
                include_providers=providers.split(',') if providers else None,
                max_results_per_provider=limit
            )
            results = client.get(optimade_filter)

        # The optimade-client returns a nested dictionary. We need to extract the actual list of materials.
        all_materials = []
        if "structures" in results:
            for provider_results in results["structures"].get(optimade_filter, {}).values():
                if "data" in provider_results:
                    all_materials.extend(provider_results["data"])

        if all_materials:
            # Enrich Materials Project entries with native API data
            all_materials = enrich_materials_with_mp_data(all_materials, console, mp_api_key)
            
            console.print(f"[green]SUCCESS:[/green] Found {len(all_materials)} materials. Showing top 10.")

            # Display results in a table with enhanced properties
            table = Table(show_header=True, header_style="bold magenta")
            table.add_column("Source ID")
            table.add_column("Formula")
            table.add_column("Elements")
            table.add_column("Band Gap (eV)")
            table.add_column("Formation Energy (eV/atom)")
            
            # Show only the first 10 results for brevity
            for material in all_materials[:10]:
                attrs = material.get("attributes", {})
                
                # Helper to gracefully get potentially missing property values
                def get_prop(keys, default="N/A"):
                    for key in keys:
                        if key in attrs and attrs[key] is not None:
                            val = attrs[key]
                            # Format numbers to a reasonable precision
                            return f"{val:.3f}" if isinstance(val, (int, float)) else str(val)
                    return default

                band_gap = get_prop(["band_gap", "_mp_band_gap", "_oqmd_band_gap"])
                formation_energy = get_prop(["formation_energy_per_atom", "_mp_formation_energy_per_atom", "_oqmd_formation_energy_per_atom"])
                
                table.add_row(
                    str(material.get("id")),
                    attrs.get("chemical_formula_descriptive", "N/A"),
                    ", ".join(attrs.get("elements", [])),
                    band_gap,
                    formation_energy
                )
            console.print(Panel(table, title="Top 10 Search Results", border_style="green"))

            # Prompt user to save results to the database
            if Confirm.ask("Do you want to save these results to the database?"):
                try:
                    db = next(get_db())
                    # Ensure the table exists before trying to save
                    Base.metadata.create_all(bind=engine, checkfirst=True)
                    
                    saved_ids = set()
                    with console.status("[bold green]Saving to database...[/bold green]"):
                        for material in all_materials:
                            source_id = material["id"]
                            # Skip duplicates within the current result set
                            if source_id in saved_ids:
                                continue

                            attrs = material.get("attributes", {})
                            # Check if the material is already in the database to avoid duplicates
                            existing = db.query(Material).filter_by(source_id=source_id).first()
                            if not existing:
                                db_material = Material(
                                    source_id=source_id,
                                    formula=attrs.get("chemical_formula_descriptive"),
                                    elements=",".join(attrs.get("elements", [])),
                                    provider=material.get("meta", {}).get("provider", {}).get("name", "N/A")
                                )
                                db.add(db_material)
                                saved_ids.add(source_id)
                        db.commit()
                    console.print("[green]SUCCESS:[/green] Results saved to the database.")
                    # Inform the user where the database is located
                    db_path = os.path.abspath(engine.url.database)
                    console.print(f"Database located at: [green]{db_path}[/green]")

                except Exception as e:
                    console.print(f"[bold red]An unexpected error occurred during save: {e}[/bold red]")
        else:
            console.print("[red]ERROR:[/red] No materials found for the given filter.")

    except Exception as e:
        console.print(f"[bold red]An error occurred during search: {e}[/bold red]")

# ==============================================================================
# 'ask' Command
# ==============================================================================
@cli.command()
@click.argument("query", required=False)
@click.option('--providers', help='Comma-separated list of provider IDs (e.g., "cod,mp").')
@click.option('--interactive', is_flag=True, help='Enable interactive mode to refine the query.')
@click.option('--reason', is_flag=True, help='Enable reasoning mode for multi-step analysis.')
@click.option('--debug-filter', help='(Dev) Bypass LLM and use this exact OPTIMADE filter.')
@click.option('--limit', type=int, default=1000, help='Maximum number of results to retrieve per provider (default: 1000).')
@click.option('--mp-api-key', help='Materials Project API key for enhanced properties (overrides environment variable).')
def ask(query: str, providers: str, interactive: bool, reason: bool, debug_filter: str, limit: int, mp_api_key: str):
    """
    Asks a question about materials science using natural language.

    This command uses a Large Language Model (LLM) to translate your
    natural language query into a structured OPTIMADE filter. It then
    searches the database network and uses the LLM again to provide a
    summarized, human-readable answer.
    """
    if query is None and not interactive:
        query = Prompt.ask("[bold cyan]Ask a question about materials science[/bold cyan]")

    try:
        llm_service = get_llm_service()
    except ValueError as e:
        console.print(f"[red]Error: {e}[/red]")
        console.print("[yellow]Please run 'prism advanced configure' to set up an LLM provider.[/yellow]")
        return

    # Interactive mode conducts a dynamic conversation
    if interactive:
        console.print("[bold cyan]Entering interactive consultation mode...[/bold cyan]")
        
        # Create adaptive filter generator for conversation
        adaptive_filter = AdaptiveOptimadeFilter(llm_service, FALLBACK_PROVIDERS)
        
        # Conduct the interactive conversation
        keywords, conversation_summary = adaptive_filter.conduct_interactive_conversation(query, console)
        
        if conversation_summary:
            # Generate final filter from conversation
            provider_to_query, optimade_filter = adaptive_filter.generate_final_filter_from_conversation(
                query, keywords, conversation_summary
            )
            
            if not provider_to_query or not optimade_filter:
                console.print("[red]Could not generate filter from conversation. Falling back to direct generation.[/red]")
                # Fall back to normal generation
                temp_client = OptimadeClient()
                provider_to_query, optimade_filter, error = adaptive_filter.generate_filter(query, temp_client)
                if error:
                    console.print(f"[red]Error: {error}[/red]")
                    return
        else:
            console.print("[yellow]No conversation data collected. Using original query.[/yellow]")
            # Fall back to normal generation
            temp_client = OptimadeClient()
            provider_to_query, optimade_filter, error = adaptive_filter.generate_filter(query, temp_client)
            if error:
                console.print(f"[red]Error: {error}[/red]")
                return

    try:
        # If a debug filter is provided, bypass the LLM for filter generation
        if debug_filter:
            optimade_filter = debug_filter
            provider_to_query = providers # Use the --providers flag for debug mode
        elif not interactive:
            # Only generate filter if not in interactive mode (interactive mode already did this)
            try:
                console.print("[bold green]Generating and testing OPTIMADE filter...[/bold green]")
            except UnicodeEncodeError:
                print("Generating and testing OPTIMADE filter...")
            
            # Create the adaptive filter generator
            adaptive_filter = AdaptiveOptimadeFilter(llm_service, FALLBACK_PROVIDERS)
            
            # Create a temporary OPTIMADE client for testing
            temp_client = OptimadeClient()
            
            # Generate the filter - use reasoning mode if --reason flag is set
            if reason:
                # Load schema for reasoning mode
                try:
                    with open("Schema.txt", "r") as f:
                        schema_content = f.read()
                except FileNotFoundError:
                    console.print("[yellow]Warning: Schema.txt not found. Using reasoning mode without schema context.[/yellow]")
                    schema_content = None
                
                provider_to_query, optimade_filter, reasoning_response = adaptive_filter.generate_reasoning_filter(query, schema_content, console)
                error = None if provider_to_query and optimade_filter else reasoning_response
                
                # Display the reasoning process
                if reasoning_response and provider_to_query and optimade_filter:
                    console.print(Panel(reasoning_response, title="Reasoning Process", border_style="cyan", title_align="left"))
            else:
                # Generate the filter with iterative refinement (normal mode)
                provider_to_query, optimade_filter, error = adaptive_filter.generate_filter(query, temp_client)
            
            if error:
                try:
                    console.print(f"[red]Error: {error}[/red]")
                    console.print("[yellow]Try rephrasing your query or being more specific about the database and elements.[/yellow]")
                except UnicodeEncodeError:
                    print(f"Error: {error}")
                    print("Try rephrasing your query or being more specific about the database and elements.")
                return
        
        console.print(Panel(f"[bold]Provider:[/bold] [cyan]{provider_to_query}[/cyan]\n[bold]Filter:[/bold] [cyan]{optimade_filter}[/cyan]", title="Query Analysis", border_style="blue"))

        # Define a comprehensive list of desired fields to retrieve from the OPTIMADE API
        desired_fields = [
            "id", "elements", "nelements", "chemical_formula_descriptive", 
            "chemical_formula_reduced", "nsites", "volume", "density",
            "structure_features", "species", "lattice_vectors",
            # Common properties that may have provider-specific names
            "band_gap", "_mp_band_gap", "_oqmd_band_gap", 
            "formation_energy_per_atom", "_mp_formation_energy_per_atom", "_oqmd_formation_energy_per_atom",
            "e_above_hull", "_mp_e_above_hull",
            # Crystal structure info
            "crystal_system", "_mp_crystal_system",
            "spacegroup_symbol", "_mp_spacegroup_symbol",
            "spacegroup_number", "_mp_spacegroup_number",
        ]

        with console.status(f"[bold green]Fetching provider capabilities from {provider_to_query}...[/bold green]"):
            # Use a new client instance to get the specific provider's info
            info_client = OptimadeClient(
                include_providers=[provider_to_query], 
                verbosity=0,
                max_results_per_provider=limit
            )
            client = info_client # Use this client for the subsequent query
            
            # Request all available fields - let OPTIMADE return what it has
            # The provider will ignore fields it doesn't support
            response_fields = None  # None means "return all available fields"
            console.print(f"[dim]Requesting all available fields from {provider_to_query}[/dim]")

        with console.status(f"[bold green]Querying {provider_to_query} for detailed properties...[/bold green]"):
            # The client object is already created and configured
            search_results = client.get(optimade_filter, response_fields=response_fields)
        
        all_materials = []
        if "structures" in search_results:
            for provider_results in search_results["structures"].get(optimade_filter, {}).values():
                if "data" in provider_results:
                    all_materials.extend(provider_results["data"])

        if not all_materials:
            console.print("[red]ERROR:[/red] No materials found for the generated filter.")
            return

        # Enrich Materials Project entries with native API data
        all_materials = enrich_materials_with_mp_data(all_materials, console, mp_api_key)

        # Display results in a table
        console.print(f"[green]SUCCESS:[/green] Found {len(all_materials)} materials. Showing top 10.")
        table = Table(show_header=True, header_style="bold magenta")
        table.add_column("Source ID")
        table.add_column("Formula")
        table.add_column("Elements")
        table.add_column("Band Gap (eV)")
        table.add_column("Formation Energy (eV/atom)")
        
        for material in all_materials[:10]:
            attrs = material.get("attributes", {})
            
            # Helper to gracefully get potentially missing property values
            def get_prop(keys, default="N/A"):
                for key in keys:
                    if key in attrs and attrs[key] is not None:
                        val = attrs[key]
                        # Format numbers to a reasonable precision
                        return f"{val:.3f}" if isinstance(val, (int, float)) else str(val)
                return default

            band_gap = get_prop(["band_gap", "_mp_band_gap", "_oqmd_band_gap"])
            formation_energy = get_prop(["formation_energy_per_atom", "_mp_formation_energy_per_atom", "_oqmd_formation_energy_per_atom"])

            table.add_row(
                str(material.get("id")),
                attrs.get("chemical_formula_descriptive", "N/A"),
                ", ".join(attrs.get("elements", [])),
                band_gap,
                formation_energy
            )
        console.print(Panel(table, title="Top 10 Search Results", border_style="green"))

        # Use the LLM to summarize the findings
        with console.status("[bold green]Analyzing results and generating answer...[/bold green]"):
            try:
                with open("Schema.txt", "r") as f:
                    schema_content = f.read()
            except FileNotFoundError:
                console.print("[yellow]Warning: Schema.txt not found. Proceeding without schema context.[/yellow]")
                schema_content = None

            # In reasoning mode, we already did the reasoning for filter generation
            # So just provide a summary of the results found
            model_context = ModelContext(query=query, results=all_materials, rag_context=schema_content)
            final_prompt = model_context.to_prompt(reasoning_mode=False)
            
            # Stream the response from the LLM for a better user experience
            stream = llm_service.get_completion(final_prompt, stream=True)

            answer_title = "Results Summary"
            console.print(f"\n[bold green]{answer_title}:[/bold green]")
            full_response = []
            for chunk in stream:
                content = ""
                # Handle different response structures from different LLM providers
                if hasattr(chunk, 'choices') and chunk.choices and hasattr(chunk.choices[0].delta, 'content'):
                    content = chunk.choices[0].delta.content
                elif hasattr(chunk, 'text'): # For providers like VertexAI
                    content = chunk.text
                
                if content:
                    full_response.append(content)
            
            panel_title = "Results Summary"
            panel_style = "magenta";
            console.print(Panel("".join(full_response), title=panel_title, border_style=panel_style, title_align="left"))

    except Exception as e:
        console.print(f"[bold red]An error occurred during 'ask': {e}[/bold red]")

# ==============================================================================
# 'advanced' Command Group
# ==============================================================================
@click.group()
def advanced():
    """Advanced commands for database management and configuration."""
    pass

@advanced.command()
def init():
    """Initializes the database, creating the necessary tables."""
    console.print("Initializing database...")
    Base.metadata.create_all(bind=engine)
    console.print("[green]SUCCESS:[/green] Database initialized.")

@advanced.command()
def configure():
    """Configures the database connection and LLM provider."""
    console.print("Configuring PRISM...")

    # Get database URL from user
    db_url = Prompt.ask("Enter your database URL", default="sqlite:///prism.db")

    # Get LLM provider choice from user
    console.print("\nSelect your LLM provider:")
    console.print("1. OpenAI")
    console.print("2. Google Vertex AI")
    console.print("3. Anthropic")
    console.print("4. OpenRouter")
    provider_choice = IntPrompt.ask("Enter the number of your provider", choices=["1", "2", "3", "4"])
    
    # Get optional model name from user
    llm_model = Prompt.ask("Enter the model name (or press enter for default)")

    env_vars = {"DATABASE_URL": db_url}

    if provider_choice == 1:
        api_key = Prompt.ask("Enter your OpenAI API key")
        env_vars["OPENAI_API_KEY"] = api_key
    elif provider_choice == 2:
        project_id = Prompt.ask("Enter your Google Cloud Project ID")
        env_vars["GOOGLE_CLOUD_PROJECT"] = project_id
        console.print("\n[bold yellow]Important:[/bold yellow] For Google Vertex AI, please ensure the `GOOGLE_APPLICATION_CREDENTIALS` environment variable is set to the path of your service account JSON file.")
    elif provider_choice == 3:
        api_key = Prompt.ask("Enter your Anthropic API key")
        env_vars["ANTHROPIC_API_KEY"] = api_key
    elif provider_choice == 4:
        api_key = Prompt.ask("Enter your OpenRouter API key")
        env_vars["OPENROUTER_API_KEY"] = api_key

    # Add model name to .env if provided
    if llm_model:
        env_vars["LLM_MODEL"] = llm_model

    # Write configuration to .env file
    env_path = Path("app/.env")
    with open(env_path, "w") as f:
        for key, value in env_vars.items():
            f.write(f'{key}="{value}"\n')

    console.print(f"[green]SUCCESS:[/green] Configuration saved to {env_path}")


# ==============================================================================
# 'docs' Command Group
# ==============================================================================
@click.group()
def docs():
    """Commands for generating documentation from templates."""
    pass

README_CONTENT = """
# PRISM: Platform for Research in Intelligent Synthesis of Materials

<p align="center">
    <em>A next-generation command-line interface for materials science research, powered by the OPTIMADE API network and Large Language Models.</em>
</p>

---

PRISM is a powerful, intelligent tool designed to revolutionize materials discovery. It provides a unified interface to query dozens of major materials science databases and leverages cutting-edge AI to make research natural, efficient, and conversational.

## 🌟 Key Features

### **Intelligent Conversational Search**
- **Dynamic Interactive Mode**: PRISM conducts intelligent conversations, asking targeted questions based on your research goals
- **Multi-Step Reasoning**: Enable `--reason` flag for detailed scientific analysis with step-by-step reasoning
- **Adaptive Learning**: The system learns from OPTIMADE API responses to refine filters automatically

### **Unified Database Access**
- **40+ Databases**: Access Materials Project, OQMD, COD, JARVIS, AFLOW, and many more through a single interface
- **OPTIMADE Standard**: Built on the Open Databases Integration for Materials Design specification
- **Smart Provider Selection**: AI automatically selects the best database for your query

### **Multiple LLM Support**
- **Currently Supported**: OpenAI, Google Vertex AI, Anthropic, OpenRouter
- **Coming Soon**: Perplexity, Grok (xAI), Ollama (local models), PRISM Custom Model (trained on materials literature)
- **Quick Switching**: Instantly switch between configured LLM providers

### **Advanced Search Capabilities**
- **Natural Language**: Ask questions like "Materials for space applications with high radiation resistance"
- **Structured Search**: Traditional parameter-based searching with elements, formulas, properties
- **Token-Optimized**: Smart conversation summarization to respect API limits

## 🚀 Core Technologies

- **OPTIMADE**: Industry-standard API for materials database integration
- **MCP (Model Context Protocol)**: Intelligent system translating natural language to database queries
- **Adaptive Filters**: Self-correcting filter generation with error feedback loops
- **BYOK (Bring Your Own Key)**: Full control over LLM usage and costs

## 📋 Command Reference

### **Main Commands**

#### `prism` (Interactive Mode)
Start PRISM without arguments for an interactive session:
```bash
prism
# Ask questions, press 's' to switch LLM, or Enter to exit
```

#### `prism ask` - Intelligent Natural Language Search
```bash
prism ask "Materials for battery electrodes" [OPTIONS]
```

**Advanced Options:**
- `--interactive`: Dynamic conversational refinement with targeted questions
- `--reason`: Multi-step scientific reasoning and analysis  
- `--providers TEXT`: Specific databases to search (cod,mp,oqmd,aflow,jarvis)
- `--debug-filter TEXT`: Developer mode - bypass LLM with direct OPTIMADE filter

**Examples:**
```bash
# Basic natural language query
prism ask "High entropy alloys with titanium"

# Interactive consultation mode
prism ask "Materials for space applications" --interactive

# Multi-step reasoning analysis
prism ask "Why are these materials suitable for batteries?" --reason

# Target specific database
prism ask "Perovskite structures" --providers "mp,cod"
```

#### `prism search` - Structured Parameter Search  
```bash
prism search [OPTIONS]
```

**Options:**
- `--elements TEXT`: Elements that must be present ("Si,O,Ti")
- `--formula TEXT`: Exact chemical formula ("SiO2")
- `--nelements INTEGER`: Number of elements (2 for binary compounds)
- `--providers TEXT`: Specific databases to query

**Examples:**
```bash
# Find titanium dioxide polymorphs
prism search --formula "TiO2"

# All ternary compounds with lithium and cobalt
prism search --elements "Li,Co" --nelements 3

# Iron-containing materials from OQMD only
prism search --elements "Fe" --providers "oqmd"
```

### **Provider and Configuration**

#### `prism switch-llm` - Quick LLM Provider Switching
```bash
prism switch-llm
```
- Lists all configured providers with current selection
- Shows upcoming providers (Perplexity, Grok, Ollama, PRISM Custom)
- One-command switching between active providers

#### `prism optimade list-dbs` - Database Discovery
```bash
prism optimade list-dbs  
```
- Lists all 40+ available OPTIMADE databases
- Shows provider IDs for use with `--providers` flag
- Real-time database availability status

#### `prism advanced` - System Management
```bash
prism advanced configure  # Set up LLM providers and database
prism advanced init       # Initialize local SQLite database
```

#### `prism docs` - Documentation
```bash
prism docs save-readme   # Generate README.md
prism docs save-install  # Generate INSTALL.md  
```

## 🎯 Usage Scenarios

### **Research Discovery**
```bash
# Start broad, get refined through conversation
prism ask "Materials for solar panels" --interactive

Q1: Are you looking for photovoltaic materials, transparent conductors, or protective coatings?
Your answer: Photovoltaic materials with high efficiency

Q2: What type of solar cell technology - silicon, perovskite, or organic?
Your answer: Perovskite and silicon

Q3: Are you interested in single junction or tandem cell materials?
Your answer: Tandem cells
```

### **Property-Based Search**
```bash
# Multi-step reasoning for complex queries
prism ask "Why do these materials have high thermal conductivity?" --reason

Step 1: Understanding the Query
[Analysis of thermal conductivity factors]

Step 2: Data Analysis  
[Examination of crystal structures and bonding]

Step 3: Scientific Conclusions
[Materials science principles explaining properties]
```

### **Database-Specific Research**
```bash
# Target materials databases by expertise
prism ask "Experimental crystal structures" --providers "cod"
prism ask "DFT-calculated properties" --providers "mp,oqmd"  
prism ask "2D materials" --providers "mcloud,twodmatpedia"
```

## 🔧 LLM Provider Configuration

PRISM supports multiple LLM providers with easy switching:

### **Active Providers**
1. **OpenAI** (`OPENAI_API_KEY`): GPT-4, GPT-3.5-turbo
2. **Google Vertex AI** (`GOOGLE_CLOUD_PROJECT`): Gemini models
3. **Anthropic** (`ANTHROPIC_API_KEY`): Claude models  
4. **OpenRouter** (`OPENROUTER_API_KEY`): Access to 200+ models

### **Coming Soon**
5. **Perplexity** (`PERPLEXITY_API_KEY`): Research-focused AI
6. **Grok** (`GROK_API_KEY`): xAI's conversational model
7. **Ollama** (`OLLAMA_HOST`): Local model deployment
8. **PRISM Custom** (`PRISM_CUSTOM_API_KEY`): Materials science-trained model

### **Quick Setup**
```bash
prism advanced configure
# Choose provider → Enter API key → Ready to go!

# Or switch anytime:
prism switch-llm
```

## 🏁 Quick Start

1. **Install** (see `INSTALL.md` for full details):
   ```bash
   git clone <repository-url>
   cd PRISM
   python -m venv .venv
   .venv\\Scripts\\activate  # Windows
   pip install -e .
   ```

2. **Configure LLM Provider**:
   ```bash  
   prism advanced configure
   ```

3. **Start Exploring**:
   ```bash
   prism ask "Materials for quantum computing" --interactive
   ```

## 💡 Pro Tips

- **Use Interactive Mode** for exploratory research with unclear requirements
- **Enable Reasoning** (`--reason`) for detailed scientific analysis
- **Try Quick Switching** - press 's' from main screen to change LLM providers
- **Target Databases** - use `--providers` to search specific repositories

- **Adaptive Filter Generation**: AI learns from API errors to improve query accuracy
- **Token Optimization**: Smart conversation summarization for efficient API usage
- **Error Recovery**: Multiple fallback strategies for robust operation
- **Database Integration**: Save and analyze results in local SQLite database
- **Extensible Architecture**: Ready for future LLM providers and databases

Ready to revolutionize your materials research? Start with `prism` and let AI guide your discovery journey!
"""

INSTALL_CONTENT = """
# PRISM Installation Guide

Complete setup guide for PRISM - Platform for Research in Intelligent Synthesis of Materials

## 🔧 Prerequisites

### **System Requirements**
- **Python**: Version 3.9, 3.10, 3.11, or 3.12 (Python 3.13+ not supported due to dependency constraints)
- **Operating System**: Windows, macOS, or Linux
- **Memory**: 4GB+ RAM recommended for local models (Ollama)
- **Storage**: ~500MB for installation and dependencies

### **Required Tools**
- **Git**: For repository cloning
- **Internet**: For database access and LLM API calls

### **LLM Provider Account** (Choose one or more)
- [OpenAI API](https://platform.openai.com/) - GPT models
- [Google Cloud](https://cloud.google.com/vertex-ai) - Gemini models  
- [Anthropic](https://console.anthropic.com/) - Claude models
- [OpenRouter](https://openrouter.ai/) - 200+ models (Recommended for beginners)

## 🚀 Installation Steps

### **Step 1: Clone the Repository**
```bash
git clone <repository-url>
cd PRISM
```

### **Step 2: Create Virtual Environment**
**Highly recommended** to avoid dependency conflicts:

```bash
# Create virtual environment
python -m venv .venv

# Activate (Windows)
.venv\\Scripts\\activate

# Activate (macOS/Linux) 
source .venv/bin/activate
```

### **Step 3: Install PRISM**
Install in editable mode with all dependencies:
```bash
pip install -e .
```

This installs:
- Core PRISM application
- OPTIMADE client for database access
- Rich library for enhanced CLI display
- SQLAlchemy for local database management
- All LLM provider SDKs (OpenAI, Anthropic, etc.)

### **Step 4: Initial Configuration**
Configure your first LLM provider:
```bash
prism advanced configure
```

You'll see:
```
Select your LLM provider:
1. OpenAI
2. Google Vertex AI
3. Anthropic  
4. OpenRouter
5. Perplexity (coming soon)
6. Grok (coming soon)
7. Ollama Local (coming soon)
8. PRISM Custom Model (coming soon)

Enter the number of your provider: 4
Enter your OpenRouter API key: [your-key-here]
```

**💡 Recommendation**: Choose **OpenRouter** for the easiest setup - it provides access to 200+ models with a single API key.

### **Step 5: Initialize Database** (Optional)
Enable result saving and analysis:
```bash
prism advanced init
```

This creates a local SQLite database for:
- Storing search results
- Query history
- Performance analytics
- Offline access to previous discoveries

## ✅ Verification

Test your installation:

### **Basic Functionality**
```bash
# Check PRISM status
prism

# List available databases
prism optimade list-dbs

# Test structured search
prism search --elements "Ti,O" --nelements 2
```

### **LLM Integration**
```bash
# Test natural language search
prism ask "Materials containing titanium"

# Test interactive mode
prism ask "Battery materials" --interactive

# Test reasoning mode
prism ask "Why are these good conductors?" --reason
```

### **Quick Switching**
```bash
# Switch LLM providers
prism switch-llm

# Or press 's' from main menu
prism
```

## 🔧 Advanced Configuration

### **Multiple LLM Providers**
Configure multiple providers for different use cases:

1. **Research**: OpenRouter (broad model access)
2. **Production**: OpenAI (reliable, fast)
3. **Privacy**: Ollama (local inference)
4. **Analysis**: Anthropic (detailed reasoning)

### **Environment Variables**
Alternative to interactive configuration:

```bash
# Create app/.env file
echo 'OPENAI_API_KEY="your-key-here"' > app/.env
echo 'DATABASE_URL="sqlite:///prism.db"' >> app/.env
echo 'LLM_MODEL="gpt-4"' >> app/.env
```

### **Custom Models**
Prepare for upcoming providers:
```bash
# Ollama setup (when available)
export OLLAMA_HOST="http://localhost:11434"

# PRISM Custom Model (when available)  
export PRISM_CUSTOM_API_KEY="your-research-key"
```

## 🐛 Troubleshooting

### **Common Issues**

#### **1. Import Errors**
```bash
# Solution: Ensure virtual environment is activated
.venv\\Scripts\\activate  # Windows
source .venv/bin/activate  # macOS/Linux

# Reinstall if needed
pip install -e .
```

#### **2. LLM Connection Failed**
```bash
# Check API key configuration
prism advanced configure

# Test connection
prism switch-llm
```

#### **3. Unicode Errors (Windows)**
- PRISM handles this automatically with fallbacks
- Rich library provides compatible display modes

#### **4. Database Initialization**
```bash
# Reset database if corrupted
rm prism.db
prism advanced init
```

### **Performance Optimization**

#### **Token Management**
- Use `--interactive` for focused conversations
- Enable `--reason` only when detailed analysis is needed
- Target specific `--providers` to reduce noise

#### **Local Caching**
- Save frequently used results with `prism advanced init`
- Results are automatically cached to local database
- Use saved data for offline analysis

## 🔄 Updating PRISM

Keep PRISM up-to-date with the latest features:

```bash
# Pull latest changes
git pull origin main

# Update dependencies
pip install -e .

# Regenerate documentation
prism docs save-readme
prism docs save-install
```

## 🆘 Getting Help

### **Built-in Help**
```bash
prism --help                    # Main commands
prism ask --help               # Natural language search
prism search --help            # Structured search  
prism advanced configure --help # Configuration options
```

### **Quick Reference**
```bash
prism                          # Interactive mode
prism ask "query" --interactive # Conversational search
prism search --elements "Fe,Ni" # Direct parameter search
prism switch-llm               # Change LLM provider
prism optimade list-dbs        # Available databases
```

### **Support Resources**
- **Documentation**: Use `prism docs save-readme` for latest features
- **Examples**: Built into help system and main interface
- **Provider Status**: Real-time database availability via `prism optimade list-dbs`

## 🎯 Next Steps

After successful installation:

1. **Explore Interactive Mode**:
   ```bash
   prism ask "Materials for renewable energy" --interactive
   ```

2. **Try Different LLM Providers**:
   ```bash
   prism switch-llm
   ```

3. **Analyze Results with Reasoning**:
   ```bash
   prism ask "Why are perovskites promising for solar cells?" --reason
   ```

4. **Save Important Discoveries**:
   ```bash
   prism advanced init  # Enable database
   # Results automatically saved during searches
   ```

Welcome to the future of materials research! 🚀
"""

@docs.command()
def save_readme():
    """Saves the project README.md file."""
    with open("README.md", "w") as f:
        f.write(README_CONTENT)
    console.print("[green]SUCCESS:[/green] `README.md` saved successfully.")

@docs.command()
def save_install():
    """Saves the project INSTALL.md file."""
    with open("INSTALL.md", "w") as f:
        f.write(INSTALL_CONTENT)
    console.print("[green]SUCCESS:[/green] `INSTALL.md` saved successfully.")

# ==============================================================================
# 'optimade' Command Group
# ==============================================================================
@click.group()
def optimade():
    """Commands for interacting with the OPTIMADE network."""
    pass

@optimade.command("list-dbs")
def list_databases():
    """Lists all available OPTIMADE provider databases."""
    with console.status("[bold green]Fetching all registered OPTIMADE providers...[/bold green]"):
        try:
            # Attempt to fetch live data from the OPTIMADE network
            client = OptimadeClient()
            
            table = Table(show_header=True, header_style="bold magenta", title="Live OPTIMADE Providers")
            table.add_column("ID", style="cyan")
            table.add_column("Name")
            table.add_column("Description")
            table.add_column("Base URL")

            if hasattr(client, 'info') and client.info and hasattr(client.info, 'providers'):
                for provider in client.info.providers:
                    table.add_row(
                        provider.id,
                        provider.name,
                        provider.description,
                        str(provider.base_url) if provider.base_url else "N/A"
                    )
                console.print(table)
            else:
                raise Exception("Could not retrieve live provider information from client.")

        except Exception as e:
            # If the live fetch fails, fall back to a hardcoded list
            console.print(f"[yellow]Warning: Could not fetch the live list of OPTIMADE providers ({e}). Displaying a fallback list of known providers.[/yellow]")
            
            table = Table(show_header=True, header_style="bold magenta", title="Fallback List of Known Providers")
            table.add_column("ID", style="cyan")
            table.add_column("Name")
            table.add_column("Description")
            table.add_column("Base URL")
            
            for provider in FALLBACK_PROVIDERS:
                table.add_row(
                    provider["id"],
                    provider["name"],
                    provider["description"],
                    provider["base_url"]
                )
            console.print(table)

# ==============================================================================
# 'configure' Command
# ==============================================================================
@cli.command()
@click.option('--mp-api-key', help='Set Materials Project API key')
@click.option('--list-config', is_flag=True, help='List current configuration')
@click.option('--reset', is_flag=True, help='Reset configuration to defaults')
def configure(mp_api_key: str, list_config: bool, reset: bool):
    """
    Configure PRISM settings and API keys.
    
    This command allows you to set API keys and other configuration options
    for PRISM. Configuration is stored in the .env file.
    """
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

# ==============================================================================
# 'mcp' Command Group
# ==============================================================================
@click.group("mcp")
def mcp_group():
    """Manage MCP server connections."""
    pass


@mcp_group.command("init")
def mcp_init():
    """Create a template mcp_servers.json config file."""
    config_dir = Path.home() / ".prism"
    config_dir.mkdir(parents=True, exist_ok=True)
    config_path = config_dir / "mcp_servers.json"

    if config_path.exists():
        console.print(f"[yellow]Config already exists: {config_path}[/yellow]")
        console.print("[dim]Edit it directly to add or remove servers.[/dim]")
        return

    template = {
        "mcpServers": {
            "example-filesystem": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"],
            },
        }
    }
    config_path.write_text(json.dumps(template, indent=2))
    console.print(f"[green]Created MCP config: {config_path}[/green]")
    console.print("[dim]Edit the file to configure your MCP servers.[/dim]")


@mcp_group.command("status")
def mcp_status():
    """Show MCP server configuration and connection status."""
    from app.mcp_client import load_mcp_config
    config = load_mcp_config()
    console.print(f"[dim]Config: {config.config_path}[/dim]")

    if not config.config_path.exists():
        console.print("[yellow]No config file found. Run 'prism mcp init' to create one.[/yellow]")
        return

    if not config.servers:
        console.print("[dim]No MCP servers configured.[/dim]")
        return

    console.print(f"[cyan]Configured servers:[/cyan] {len(config.servers)}")
    for name, server_config in config.servers.items():
        if "url" in server_config:
            location = server_config["url"]
        elif "command" in server_config:
            location = f"{server_config['command']} {' '.join(server_config.get('args', []))}"
        else:
            location = "unknown"
        console.print(f"  [green]{name}[/green] — {location}")


# ==============================================================================
# 'sim' Command Group (Pyiron Simulation)
# ==============================================================================
@click.group("sim")
def sim_group():
    """Atomistic simulation commands (pyiron)."""
    pass


@sim_group.command("status")
def sim_status():
    """Show pyiron configuration, available codes, and job counts."""
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


# ==============================================================================
# 'setup' Command — Workflow Preferences Wizard
# ==============================================================================
@cli.command()
def setup():
    """Interactive wizard to configure workflow preferences."""
    from app.config.preferences import UserPreferences

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


# ==============================================================================
# 'plugin' Command Group
# ==============================================================================
@click.group("plugin")
def plugin_group():
    """Manage PRISM plugins."""
    pass


@plugin_group.command("list")
def plugin_list():
    """List installed plugins (entry points + local directory)."""
    from app.plugins.registry import PluginRegistry
    from app.plugins.loader import discover_entry_point_plugins, discover_local_plugins

    reg = PluginRegistry()
    ep_names = discover_entry_point_plugins(reg)
    local_names = discover_local_plugins(reg)

    if not ep_names and not local_names:
        console.print("[dim]No plugins found.[/dim]")
        console.print("[dim]Install a plugin via pip or place a .py file in ~/.prism/plugins/[/dim]")
        return

    if ep_names:
        console.print("[cyan]Entry-point plugins:[/cyan]")
        for name in ep_names:
            console.print(f"  [green]{name}[/green]")
    if local_names:
        console.print("[cyan]Local plugins (~/.prism/plugins/):[/cyan]")
        for name in local_names:
            console.print(f"  [green]{name}[/green]")


@plugin_group.command("init")
@click.argument("name")
def plugin_init(name):
    """Create a plugin template in ~/.prism/plugins/."""
    plugin_dir = Path.home() / ".prism" / "plugins"
    plugin_dir.mkdir(parents=True, exist_ok=True)
    plugin_file = plugin_dir / f"{name}.py"

    if plugin_file.exists():
        console.print(f"[yellow]Plugin already exists: {plugin_file}[/yellow]")
        return

    template = f'''"""PRISM plugin: {name}"""


def register(registry):
    """Called by PRISM plugin loader.

    registry attributes:
      - tool_registry: register custom tools
      - skill_registry: register custom skills
      - collector_registry: register custom data collectors
      - algorithm_registry: register custom ML algorithms
    """
    # Example: register a custom tool
    # from app.tools.base import Tool
    # registry.tool_registry.register(Tool(
    #     name="{name}_tool",
    #     description="My custom tool",
    #     input_schema={{"type": "object", "properties": {{}}}},
    #     func=lambda **kwargs: {{"result": "hello"}},
    # ))
    pass
'''
    plugin_file.write_text(template)
    console.print(f"[green]Created plugin template: {plugin_file}[/green]")
    console.print("[dim]Edit the file and add your custom tools, skills, collectors, or algorithms.[/dim]")


# ==============================================================================
# CLI Entry Point
# ==================================================
cli.add_command(advanced)
cli.add_command(docs)
cli.add_command(optimade)
cli.add_command(mcp_group, "mcp")
cli.add_command(sim_group, "sim")
cli.add_command(plugin_group, "plugin")

from app.commands.data import data as data_group
cli.add_command(data_group, "data")

from app.commands.predict import predict as predict_cmd
cli.add_command(predict_cmd, "predict")

from app.commands.model import model as model_group
cli.add_command(model_group, "model")

if __name__ == "__main__":
    cli()
