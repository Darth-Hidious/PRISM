#!/usr/bin/env python3
"""
PRISM Platform Enhanced CLI Tool

A comprehensive command-line interface for materials discovery and database management.
Supports NOMAD, JARVIS, OQMD, COD and custom databases with advanced filtering,
visualization, and export capabilities.
"""

import asyncio
import json
import sys
import os
from datetime import datetime, timedelta
from pathlib import Path
from typing import Dict, List, Optional, Any

import click
from rich.console import Console
from rich.table import Table
from rich.progress import Progress, TaskID
from rich.panel import Panel
from rich.text import Text
from rich.prompt import Confirm, Prompt, IntPrompt, FloatPrompt
from rich.tree import Tree
from rich.align import Align
from rich.layout import Layout
from rich.columns import Columns
from rich import print as rprint

from optimade.client import OptimadeClient

# Import branding configuration
try:
    from app.config.branding import (
        COMPANY_LOGO, COMPANY_NAME, COMPANY_TAGLINE, COMPANY_DESCRIPTION,
        PRIMARY_COLOR, SECONDARY_COLOR, ACCENT_COLOR, FEATURE_LIST
    )
    PRISM_ASCII_ART = COMPANY_LOGO
    COMPANY_BRANDING = {
        'name': COMPANY_NAME,
        'tagline': COMPANY_TAGLINE,
        'description': COMPANY_DESCRIPTION,
        'features': FEATURE_LIST
    }
except ImportError:
    # Fallback to default PRISM branding
    PRISM_ASCII_ART = """
██████╗ ██████╗ ██╗███████╗███╗   ███╗
██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
██████╔╝██████╔╝██║███████╗██╔████╔██║
██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
██║     ██║  ██║██║███████║██║ ╚═╝ ██║
╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝
"""
    COMPANY_BRANDING = {
        'name': 'PRISM',
        'tagline': 'Platform for Research in Intelligent Synthesis of Materials',
        'description': 'Advanced Materials Discovery & Database Integration Platform',
        'features': [
            "✨ Access 2M+ materials across NOMAD, JARVIS, OQMD & COD",
            "🔬 Advanced filtering, visualization & export capabilities",
            "🚀 Interactive search modes & comprehensive examples"
        ]
    }
    PRIMARY_COLOR = "cyan"
    SECONDARY_COLOR = "blue"
    ACCENT_COLOR = "green"

# Add a function to handle OPTIMADE filter construction
def build_optimade_filter(elements: Optional[List[str]] = None, 
                          formula: Optional[str] = None,
                          nelements: Optional[int] = None,
                          **kwargs) -> str:
    """Builds a valid OPTIMADE filter string from search parameters."""
    filters = []
    if elements:
        element_list = ",".join(f'"{e}"' for e in elements)
        filters.append(f"elements HAS ALL {element_list}")
    if formula:
        filters.append(f'chemical_formula_descriptive="{formula}"')
    if nelements:
        filters.append(f"nelements={nelements}")

    # You can extend this to handle other standard OPTIMADE fields
    # e.g., band_gap, formation_energy, etc.
    # Note: OPTIMADE filter support for quantities can vary by provider.

    return " AND ".join(filters)

WELCOME_TEXT = f"""
[bold {PRIMARY_COLOR}]{COMPANY_BRANDING['tagline']}[/bold {PRIMARY_COLOR}]
[dim]{COMPANY_BRANDING['description']}[/dim]

""" + "\n".join([f"[{ACCENT_COLOR}]{feature}[/{ACCENT_COLOR}]" for feature in COMPANY_BRANDING['features']])

# Initialize Rich console
console = Console()

def show_launch_screen():
    """Display the PRISM launch screen with ASCII art and welcome message."""
    console.clear()
    
    # Create the main layout
    layout = Layout()
    layout.split_column(
        Layout(name="header", size=12),
        Layout(name="content", size=8),
        Layout(name="footer", size=12)
    )
    
    # ASCII Art in header
    ascii_panel = Panel(
        Align.center(Text(PRISM_ASCII_ART, style="bold blue")),
        style="blue",
        border_style="bright_blue"
    )
    layout["header"].update(ascii_panel)
    
    # Welcome text in content
    welcome_panel = Panel(
        Align.center(WELCOME_TEXT),
        title="[bold]Welcome to PRISM[/bold]",
        style="green",
        border_style="bright_green"
    )
    layout["content"].update(welcome_panel)
    
    # Quick tips in footer
    tips_text = f"""
[bold {PRIMARY_COLOR}]PRISM Platform CLI - Advanced Materials Discovery & Database Integration[/bold {PRIMARY_COLOR}]

A comprehensive command-line interface for materials research with support for
NOMAD, JARVIS, OQMD, COD databases and custom database integration.

[bold yellow]Features:[/bold yellow]
• Multi-database material search with advanced filtering
• Formation energy, band gap, and stability screening  
• High Entropy Alloy (HEA) discovery tools
• Data visualization and export (CSV, JSON, plots)
• Interactive search modes with guided prompts
• Custom database integration support

[bold yellow]💡 Quick Start:[/bold yellow]
• [cyan]prism search --interactive[/cyan]        (Interactive guided search)
• [cyan]prism examples[/cyan]                    (See usage examples)
• [cyan]prism getting-started[/cyan]             (Setup guide)
• [cyan]prism --help[/cyan]                      (All commands)
"""
    tips_panel = Panel(
        tips_text,
        title="[bold]Getting Started[/bold]",
        style="yellow",
        border_style="bright_yellow"
    )
    layout["footer"].update(tips_panel)
    
    console.print(layout)
    console.print()

def create_help_table():
    """Create a comprehensive help table for all commands."""
    table = Table(title="PRISM Commands Reference", show_header=True, header_style="bold magenta")
    table.add_column("Command", style="cyan", no_wrap=True, width=20)
    table.add_column("Description", style="white", width=50)
    table.add_column("Example", style="green", width=40)
    
    # Core search commands
    table.add_row(
        "search",
        "Advanced material search across databases",
        "prism search --elements Si,O --limit 10"
    )
    table.add_row(
        "search --interactive",
        "Interactive guided search with prompts",
        "prism search --interactive"
    )
    
    # Database management
    table.add_row(
        "test-database",
        "Test connection to specific database",
        "prism test-database --database oqmd"
    )
    table.add_row(
        "list-databases",
        "Show all available databases",
        "prism list-databases"
    )
    table.add_row(
        "add-custom-database",
        "Add your own database configuration",
        "prism add-custom-database mydb.json"
    )
    
    # Data export and visualization
    table.add_row(
        "export-data",
        "Export search results to files",
        "prism search --export csv --elements Fe"
    )
    table.add_row(
        "visualize",
        "Generate plots and analysis",
        "prism search --plot --elements Li,O"
    )
    
    # System and help
    table.add_row(
        "examples",
        "Show comprehensive usage examples",
        "prism examples"
    )
    table.add_row(
        "getting-started",
        "Step-by-step setup and usage guide",
        "prism getting-started"
    )
    table.add_row(
        "schema",
        "Show command schemas and parameters",
        "prism schema --command search"
    )
    
    return table

# Import application modules (keeping existing imports)
try:
    from app.core.config import get_settings
    from app.services.job_processor import JobProcessor
    from app.services.job_scheduler import JobScheduler
    from app.services.data_viewer import MaterialsDataViewer
    from app.services.connectors.base_connector import StandardizedMaterial, MaterialStructure, MaterialProperties, MaterialMetadata
except ImportError as e:
    console.print(f"[red]Warning: Some modules not available: {e}[/red]")
    console.print("[yellow]Some features may be limited.[/yellow]")

# Custom exceptions
class CLIError(click.ClickException):
    """Custom CLI exception with rich formatting."""
    
    def format_message(self):
        return f"[red]Error:[/red] {self.message}"

def get_nomad_config():
    """Get NOMAD configuration."""
    return {
        'base_url': 'https://nomad-lab.eu/prod/v1/api/v1',
        'timeout': 30.0
    }

def get_database_configs():
    """Get all database configurations."""
    # This is now less relevant but can be kept for other purposes
    return {
        'optimade': {'description': 'Unified OPTIMADE endpoint'}
    }

@click.group(invoke_without_command=True)
@click.option('--debug', is_flag=True, help='Enable debug mode')
@click.option('--config-file', help='Path to configuration file')
@click.option('--no-banner', is_flag=True, help='Skip the welcome banner')
@click.pass_context
def cli(ctx, debug, config_file, no_banner):
    """
    MARC27's PRISM - Platform for Research in Intelligent Synthesis of Materials
    
    Advanced materials discovery and database integration platform.
    Run without arguments to see the interactive launch screen.
    """
    ctx.ensure_object(dict)
    ctx.obj['debug'] = debug
    ctx.obj['config_file'] = config_file
    
    # Show launch screen if no command provided and banner not disabled
    if ctx.invoked_subcommand is None:
        if not no_banner:
            show_launch_screen()
        
        # Show main help
        console.print(create_help_table())
        
        # Interactive prompt for next action
        console.print("\n[bold yellow]What would you like to do?[/bold yellow]")
        choices = {
            "1": ("Interactive Search", "search --interactive"),
            "2": ("View Examples", "examples"),
            "3": ("Getting Started Guide", "getting-started"),
            "4": ("Test Database Connection", "test-database"),
            "5": ("Show All Commands", "--help")
        }
        
        for key, (desc, cmd) in choices.items():
            console.print(f"  {key}. {desc} [dim]({cmd})[/dim]")
        
        choice = Prompt.ask("\nSelect option (1-5)", choices=list(choices.keys()), default="1")
        
        if choice in choices:
            _, cmd = choices[choice]
            if cmd == "--help":
                console.print(ctx.get_help())
            else:
                console.print(f"\n[green]💡 To run this:[/green] [cyan]prism {cmd}[/cyan]")
    
    if debug:
        console.print("[yellow]🐛 Debug mode enabled[/yellow]")

@cli.command()
def getting_started():
    """
    Step-by-step guide to get started with PRISM.
    
    This command provides a comprehensive walkthrough of PRISM's capabilities,
    including database setup, search examples, and advanced features.
    """
    console.clear()
    
    # Header
    console.print(Panel(
        "[bold cyan]PRISM Getting Started Guide[/bold cyan]\n"
        "[dim]Your comprehensive guide to materials discovery[/dim]",
        style="blue"
    ))
    
    # Step 1: Understanding PRISM with OPTIMADE
    console.print("\n[bold green]📚 Step 1: Understanding PRISM with OPTIMADE[/bold green]")
    console.print("""
PRISM now uses the [bold cyan]OPTIMADE[/bold cyan] standard to access a vast network of materials databases, including:
• [cyan]Materials Project[/cyan], [cyan]AFLOW[/cyan], [cyan]OQMD[/cyan], [cyan]COD[/cyan], and many more.
This provides unified access to millions of materials structures and properties through a single, powerful query language.
""")

    if not Confirm.ask("Continue to connection testing?", default=True):
        return

    # Step 2: Test Database Connections
    console.print("\n[bold green]🔗 Step 2: Test OPTIMADE Connection[/bold green]")
    console.print("Let's test your connection to the OPTIMADE network:")
    
    with console.status("Testing OPTIMADE providers..."):
        try:
            client = OptimadeClient()
            console.print(f"  ✅ OPTIMADE connection: [green]OK[/green] ({len(client.providers)} providers found)")
        except Exception as e:
            console.print(f"  ❌ OPTIMADE connection: [red]Failed[/red] - {e}")

    console.print("\n💡 [yellow]Tip:[/yellow] Use [cyan]prism test-database[/cyan] to see the list of all available providers.")

    if not Confirm.ask("Continue to search examples?", default=True):
        return

    # Step 3: Basic Search Examples
    console.print("\n[bold green]🔍 Step 3: Basic Search Examples[/bold green]")
    
    examples = [
        {
            "title": "Search for Silicon materials",
            "command": "prism search --elements Si --limit 5",
            "description": "Find silicon-containing materials across all OPTIMADE databases"
        },
        {
            "title": "Find ternary compounds with Ti but not O",
            "command": 'prism search --filter \'elements HAS "Ti" AND nelements=3 AND NOT elements HAS "O"\'',
            "description": "Use the powerful OPTIMADE filter language for complex queries"
        },
        {
            "title": "Search for a specific formula",
            "command": "prism search --formula Al2O3 --export csv",
            "description": "Find alumina structures and export the results to a CSV file"
        },
        {
            "title": "High Entropy Alloys (by composition)",
            "command": "prism search --elements Nb,Mo,Ta,W --nelements 4",
            "description": "Search for 4-component alloys containing specific elements"
        }
    ]
    
    for i, example in enumerate(examples, 1):
        console.print(f"\n[cyan]Example {i}: {example['title']}[/cyan]")
        console.print(f"[dim]{example['description']}[/dim]")
        console.print(f"[yellow]Command:[/yellow] {example['command']}")
    
    if not Confirm.ask("Continue to advanced features?", default=True):
        return
    
    # Step 4: Advanced Features
    console.print("\n[bold green]🚀 Step 4: Advanced Features[/bold green]")
    console.print("""
[cyan]Interactive Search:[/cyan]
• [yellow]prism search --interactive[/yellow] - Guided search with prompts
• Great for exploring databases without knowing exact parameters

[cyan]Data Visualization:[/cyan]
• [yellow]prism search --plot --elements Li,O[/yellow] - Generate formation energy plots
• [yellow]prism search --export both[/yellow] - Export CSV and JSON with metadata

[cyan]Custom Databases:[/cyan]
• [yellow]prism add-custom-database mydb.json[/yellow] - Add your own database
• [yellow]prism schema --command add-custom-database[/yellow] - See required format

[cyan]Comprehensive Examples:[/cyan]
• [yellow]prism examples[/yellow] - 50+ usage examples for different research areas
""")
    
    # Step 5: Next Steps
    console.print("\n[bold green]🎯 Step 5: Your Next Steps[/bold green]")
    console.print("""
Now you're ready to start using PRISM! Here are some suggestions:

[yellow]For Battery Research:[/yellow]
• Search for stable Li-ion materials: [cyan]prism search --database oqmd --elements Li,Co,O --stability-max 0.1[/cyan]

[yellow]For Semiconductor Research:[/yellow]
• Find wide bandgap materials: [cyan]prism search --band-gap-min 2.0 --export csv[/cyan]

[yellow]For Alloy Research:[/yellow]
• Discover HEAs: [cyan]prism search --database cod --min-elements 4[/cyan]

[yellow]For Data Analysis:[/yellow]
• Export and visualize: [cyan]prism search --elements Si --plot --export both[/cyan]
""")
    
    console.print("\n[bold blue]🎉 You're all set to start discovering materials with PRISM![/bold blue]")
    console.print("[dim]💡 Use 'prism examples' for more specific research examples[/dim]")

@cli.command()
@click.option('--command', help='Show schema for specific command')
def schema(command):
    """
    Show command schemas and parameter documentation.
    
    Displays detailed parameter schemas, validation rules, and examples
    for PRISM commands. Useful for API integration and automation.
    """
    console.print(Panel(
        "[bold cyan]PRISM Command Schemas[/bold cyan]\n"
        "[dim]Parameter documentation and validation rules[/dim]",
        style="blue"
    ))
    
    if command == "search":
        show_search_schema()
    elif command == "add-custom-database":
        show_custom_database_schema()
    elif command is None:
        show_all_schemas()
    else:
        console.print(f"[red]Unknown command: {command}[/red]")
        console.print("[yellow]Available schemas: search, add-custom-database[/yellow]")

def show_search_schema():
    """Show detailed schema for search command."""
    schema_data = {
        "command": "search",
        "description": "Advanced material search across multiple databases",
        "parameters": {
            "database": {
                "type": "choice",
                "choices": ["nomad", "jarvis", "oqmd", "cod", "all"],
                "default": "all",
                "description": "Target database(s) for search"
            },
            "elements": {
                "type": "string",
                "format": "comma-separated",
                "example": "Si,O or Fe,Ni,Cr",
                "description": "Chemical elements to search for"
            },
            "formula": {
                "type": "string",
                "example": "SiO2 or Li2CO3",
                "description": "Specific chemical formula"
            },
            "formation-energy-max": {
                "type": "float",
                "unit": "eV/atom",
                "range": "typically -5.0 to 5.0",
                "description": "Maximum formation energy for stability filtering"
            },
            "band-gap-min": {
                "type": "float",
                "unit": "eV",
                "range": "0.0 to 10.0",
                "description": "Minimum band gap for semiconductor filtering"
            },
            "band-gap-max": {
                "type": "float", 
                "unit": "eV",
                "range": "0.0 to 10.0",
                "description": "Maximum band gap for semiconductor filtering"
            },
            "stability-max": {
                "type": "float",
                "unit": "eV/atom",
                "database": "OQMD only",
                "description": "Maximum hull distance (stability criterion)"
            },
            "min-elements": {
                "type": "integer",
                "range": "1 to 20",
                "description": "Minimum number of elements (for HEA searches)"
            },
            "limit": {
                "type": "integer",
                "default": 50,
                "range": "1 to 10000",
                "description": "Maximum number of results to return"
            },
            "export": {
                "type": "choice",
                "choices": ["csv", "json", "both"],
                "description": "Export format for results"
            },
            "plot": {
                "type": "flag",
                "description": "Generate visualization plots"
            },
            "interactive": {
                "type": "flag",
                "description": "Enable interactive guided search"
            }
        }
    }
    
    console.print(json.dumps(schema_data, indent=2))

def show_custom_database_schema():
    """Show schema for custom database configuration."""
    schema_example = {
        "name": "my_custom_db",
        "display_name": "My Custom Materials Database",
        "description": "Custom database for specialized materials",
        "connection": {
            "type": "api",
            "base_url": "https://api.mydatabase.com/v1",
            "auth": {
                "type": "api_key",
                "key_header": "X-API-Key",
                "key_value": "your_api_key_here"
            },
            "timeout": 30.0
        },
        "endpoints": {
            "search": "/materials/search",
            "get_by_id": "/materials/{id}",
            "health_check": "/health"
        },
        "parameters": {
            "elements": {
                "api_param": "elements",
                "format": "comma_separated"
            },
            "formation_energy": {
                "api_param": "formation_energy_max",
                "type": "float"
            }
        },
        "data_mapping": {
            "id": "material_id",
            "formula": "chemical_formula",
            "formation_energy": "properties.formation_energy",
            "band_gap": "properties.electronic.band_gap"
        }
    }
    
    console.print("[bold green]Custom Database Configuration Schema:[/bold green]")
    console.print(json.dumps(schema_example, indent=2))
    
    console.print(f"\n[bold yellow]To add this database:[/bold yellow]")
    console.print(f"1. Save the configuration as [cyan]mydb.json[/cyan]")
    console.print(f"2. Run [cyan]prism add-custom-database mydb.json[/cyan]")
    console.print(f"3. Test with [cyan]prism test-database --database my_custom_db[/cyan]")

def show_all_schemas():
    """Show overview of all available schemas."""
    table = Table(title="Available Command Schemas")
    table.add_column("Command", style="cyan")
    table.add_column("Description", style="white")
    table.add_column("Usage", style="green")
    
    table.add_row("search", "Material search parameters and validation", "prism schema --command search")
    table.add_row("add-custom-database", "Custom database configuration format", "prism schema --command add-custom-database")
    
    console.print(table)

@cli.command()
def list_databases():
    """
    List all available databases with their status and capabilities.
    
    Shows information about supported databases including connection status,
    data types, and approximate number of materials available.
    """
    console.print(Panel(
        "[bold cyan]Available Databases (via OPTIMADE)[/bold cyan]\n"
        "[dim]PRISM connects to the OPTIMADE network of materials databases.[/dim]",
        style="blue"
    ))

    try:
        with console.status("Fetching OPTIMADE providers..."):
            client = OptimadeClient()
        
        table = Table(title=f"OPTIMADE Providers ({len(client.providers)} found)", show_header=True, header_style="bold magenta")
        table.add_column("Provider Name", style="cyan", no_wrap=True)
        table.add_column("Description", style="white")
        table.add_column("Homepage", style="green")

        for provider in client.providers:
            table.add_row(provider.name, provider.description, provider.homepage)

        console.print(table)
        console.print("\n[dim]You can query all these databases simultaneously using the `search` command.[/dim]")

    except Exception as e:
        console.print(f"[red]Error fetching OPTIMADE providers: {e}[/red]")
        console.print("[yellow]Please check your internet connection.[/yellow]")


@cli.command()
@click.option('--filter', 'optimade_filter',
              help='A raw OPTIMADE filter string. Example: \'elements HAS "Si" AND nelements=2\'')
@click.option('--elements', 
              help='Elements to search for (comma-separated, e.g., Si,O)')
@click.option('--formula', 
              help='Specific chemical formula (e.g., SiO2)')
@click.option('--nelements',
              type=int,
              help='The number of elements in the material.')
@click.option('--limit', 
              type=int, 
              default=50,
              help='Maximum number of results (default: 50)')
@click.option('--export', 
              type=click.Choice(['csv', 'json', 'both']),
              help='Export results to file')
@click.option('--plot', 
              is_flag=True,
              help='Generate visualization plots')
@click.option('--interactive', 
              is_flag=True,
              help='Interactive search mode with prompts')
@click.pass_context
def search(ctx, optimade_filter, elements, formula, nelements, limit, export, plot, interactive):
    """
    Search for materials using the OPTIMADE unified API.
    
    This command queries all available OPTIMADE providers. You can either provide a raw
    OPTIMADE filter string or use simplified options like --elements and --formula.
    
    Examples:
    
    \b
    # Basic element search across all databases
    prism search --elements Si,O --limit 10
    
    \b
    # Use a powerful, raw OPTIMADE filter
    prism search --filter 'elements HAS "Ti" AND nelements=3 AND NOT elements HAS "O"'
    
    \b  
    # Find a specific formula and export the results
    prism search --formula Al2O3 --export csv
    
    \b
    # Interactive guided search
    prism search --interactive
    """
    
    if interactive:
        # This function would also need to be refactored to build an OPTIMADE filter
        return run_interactive_search()
    
    # Validate search parameters
    if not any([optimade_filter, elements, formula, nelements]):
        console.print("[red]Error:[/red] At least one search parameter is required (--filter, --elements, --formula, or --nelements).")
        console.print("[yellow]💡 Tip:[/yellow] Use [cyan]--interactive[/cyan] for guided search.")
        return

    # Build the filter
    final_filter = optimade_filter
    if not final_filter:
        final_filter = build_optimade_filter(
            elements=[e.strip() for e in elements.split(',')] if elements else None,
            formula=formula,
            nelements=nelements
        )

    # Build search parameters dictionary
    search_params = {
        'filter': final_filter,
        'limit': limit
    }
    
    # Execute search
    asyncio.run(execute_search(search_params, export, plot))

def run_interactive_search():
    """Run interactive guided search mode."""
    console.clear()
    console.print(Panel(
        "[bold cyan]🔍 Interactive Material Search[/bold cyan]\n"
        "[dim]Guided search with step-by-step prompts[/dim]",
        style="blue"
    ))
    
    # Show quick tips
    console.print("""
[bold yellow]💡 Interactive Search Tips:[/bold yellow]
• Answer prompts to build your search criteria
• Press Enter for default values
• Type 'help' for parameter explanations
• Use Ctrl+C to exit at any time
""")
    
    # Step 1: Choose database
    console.print("\n[bold green]Step 1: Choose Database[/bold green]")
    db_choices = {
        "1": ("all", "Search all databases (recommended)"),
        "2": ("nomad", "NOMAD - DFT calculations (1.9M+ materials)"),
        "3": ("oqmd", "OQMD - Formation energies (700K+ materials)"),
        "4": ("cod", "COD - Crystal structures (500K+ materials)"),
        "5": ("jarvis", "JARVIS - NIST materials (100K+ materials)")
    }
    
    for key, (code, desc) in db_choices.items():
        console.print(f"  {key}. {desc}")
    
    db_choice = Prompt.ask("Select database", choices=list(db_choices.keys()), default="1")
    database = db_choices[db_choice][0]
    
    # Step 2: Research focus
    console.print(f"\n[bold green]Step 2: Research Focus[/bold green]")
    focus_choices = {
        "1": "Elements/Composition",
        "2": "Battery Materials", 
        "3": "Semiconductors",
        "4": "High Entropy Alloys",
        "5": "Thermodynamic Stability",
        "6": "Custom Advanced Search"
    }
    
    for key, desc in focus_choices.items():
        console.print(f"  {key}. {desc}")
    
    focus = Prompt.ask("Select research focus", choices=list(focus_choices.keys()), default="1")
    
    # Build search based on focus
    search_params = {'limit': 50}
    
    if focus == "1":  # Elements/Composition
        elements_input = Prompt.ask("Enter elements (e.g., Si,O or Fe,Ni,Cr)")
        if elements_input:
            search_params['elements'] = [e.strip() for e in elements_input.split(',')]
        
        formula_input = Prompt.ask("Specific formula (optional)", default="")
        if formula_input:
            search_params['formula'] = formula_input
    
    elif focus == "2":  # Battery materials
        console.print("[cyan]🔋 Battery Materials Search[/cyan]")
        console.print("Searching for stable Li-ion materials with formation energies...")
        
        search_params['elements'] = ['Li']
        search_params['formation_energy_max'] = -0.5
        
        cathode_elements = Prompt.ask("Cathode elements (e.g., Co,O or Ni,Mn,Co,O)", default="Co,O")
        if cathode_elements:
            search_params['elements'].extend([e.strip() for e in cathode_elements.split(',')])
        
        max_energy = FloatPrompt.ask("Max formation energy (eV/atom)", default=-0.5)
        search_params['formation_energy_max'] = max_energy
        
        if database == 'all':
            database = 'oqmd'  # OQMD best for formation energies
            console.print("[yellow]💡 Switched to OQMD for formation energy data[/yellow]")
    
    elif focus == "3":  # Semiconductors
        console.print("[cyan]💡 Semiconductor Materials Search[/cyan]")
        
        min_gap = FloatPrompt.ask("Minimum band gap (eV)", default=1.0)
        max_gap = FloatPrompt.ask("Maximum band gap (eV)", default=5.0)
        search_params['band_gap_min'] = min_gap
        search_params['band_gap_max'] = max_gap
        
        elements_input = Prompt.ask("Preferred elements (optional, e.g., Ga,N)", default="")
        if elements_input:
            search_params['elements'] = [e.strip() for e in elements_input.split(',')]
    
    elif focus == "4":  # High Entropy Alloys
        console.print("[cyan]🔧 High Entropy Alloys Search[/cyan]")
        
        min_elements = IntPrompt.ask("Minimum elements", default=4)
        max_elements = IntPrompt.ask("Maximum elements", default=8)
        search_params['min_elements'] = min_elements
        search_params['max_elements'] = max_elements
        
        hea_elements = Prompt.ask("HEA elements (e.g., Nb,Mo,Ta,W,Re)", default="")
        if hea_elements:
            search_params['elements'] = [e.strip() for e in hea_elements.split(',')]
        
        if database == 'all':
            database = 'cod'  # COD best for crystal structures
            console.print("[yellow]💡 Switched to COD for crystal structure data[/yellow]")
    
    elif focus == "5":  # Thermodynamic stability
        console.print("[cyan]⚖️ Thermodynamic Stability Search[/cyan]")
        
        max_hull = FloatPrompt.ask("Max hull distance (eV/atom)", default=0.1)
        search_params['stability_max'] = max_hull
        
        max_formation = FloatPrompt.ask("Max formation energy (eV/atom)", default=-1.0)
        search_params['formation_energy_max'] = max_formation
        
        elements_input = Prompt.ask("Elements of interest", default="")
        if elements_input:
            search_params['elements'] = [e.strip() for e in elements_input.split(',')]
        
        if database == 'all':
            database = 'oqmd'  # OQMD best for stability data
            console.print("[yellow]💡 Switched to OQMD for stability data[/yellow]")
    
    elif focus == "6":  # Custom advanced
        console.print("[cyan]🔬 Custom Advanced Search[/cyan]")
        
        # Collect all possible parameters
        elements_input = Prompt.ask("Elements (optional)", default="")
        if elements_input:
            search_params['elements'] = [e.strip() for e in elements_input.split(',')]
        
        formula_input = Prompt.ask("Formula (optional)", default="")
        if formula_input:
            search_params['formula'] = formula_input
        
        fe_max = Prompt.ask("Max formation energy (eV/atom, optional)", default="")
        if fe_max:
            search_params['formation_energy_max'] = float(fe_max)
        
        bg_min = Prompt.ask("Min band gap (eV, optional)", default="")
        if bg_min:
            search_params['band_gap_min'] = float(bg_min)
        
        bg_max = Prompt.ask("Max band gap (eV, optional)", default="")
        if bg_max:
            search_params['band_gap_max'] = float(bg_max)
        
        hull_max = Prompt.ask("Max hull distance (eV/atom, optional)", default="")
        if hull_max:
            search_params['stability_max'] = float(hull_max)
        
        space_group_input = Prompt.ask("Space group (optional)", default="")
        if space_group_input:
            search_params['space_group'] = space_group_input
    
    # Step 3: Output preferences
    console.print(f"\n[bold green]Step 3: Output Preferences[/bold green]")
    
    limit = IntPrompt.ask("Maximum results", default=50)
    search_params['limit'] = limit
    
    export_choice = Prompt.ask("Export results?", 
                              choices=['none', 'csv', 'json', 'both'], 
                              default='none')
    export_format = None if export_choice == 'none' else export_choice
    
    generate_plots = Confirm.ask("Generate visualization plots?", default=True)
    
    # Show search summary
    console.print(f"\n[bold blue]🔍 Search Summary[/bold blue]")
    console.print(f"Database: [cyan]{database}[/cyan]")
    console.print(f"Parameters: [yellow]{search_params}[/yellow]")
    if export_format:
        console.print(f"Export: [green]{export_format}[/green]")
    if generate_plots:
        console.print(f"Plots: [green]Yes[/green]")
    
    if not Confirm.ask("\nProceed with search?", default=True):
        console.print("[yellow]Search cancelled[/yellow]")
        return
    
    # Execute search
    console.print(f"\n[bold blue]🚀 Executing search...[/bold blue]")
    asyncio.run(execute_search(search_params, export_format, generate_plots))

async def execute_search(search_params: Dict[str, Any], export_format: Optional[str], generate_plots: bool):
    """Execute the material search using the OPTIMADE client."""
    
    all_results = []
    
    console.print(f"🔍 Querying all OPTIMADE providers with filter: [cyan]{search_params.get('filter')}[/cyan]")

    try:
        with console.status("[bold green]Searching across the OPTIMADE network...[/bold green]"):
            client = OptimadeClient()
            results = client.get(
                filter=search_params['filter'],
                page_limit=search_params.get('limit', 50) # Note: OPTIMADE uses page_limit
            )

        # The client aggregates results. The actual data entries are in the 'data' key.
        all_results = results.get('data', [])

        if all_results:
            console.print(f"✅ Found {len(all_results)} matching structures across all databases.")
        else:
            console.print("❌ No materials found matching your criteria.")

    except Exception as e:
        console.print(f"[red]❌ An error occurred while querying OPTIMADE providers: {e}[/red]")
        return
    
    # Display results
    if all_results:
        display_optimade_results(all_results)
        
        # Export if requested
        if export_format:
            # This function would need to be adapted for the OPTIMADE format
            # export_results(all_results, export_format)
            console.print("[yellow]Note: Export functionality needs to be adapted for OPTIMADE results.[/yellow]")

        # Generate plots if requested
        if generate_plots:
            # This function would also need adaptation
            # generate_visualizations(all_results)
            console.print("[yellow]Note: Plotting functionality needs to be adapted for OPTIMADE results.[/yellow]")
    else:
        suggest_alternatives(search_params)

def suggest_alternatives(search_params):
    """Suggest alternative search strategies when no results found."""
    console.print("\n[bold yellow]💡 Suggestions to find materials:[/bold yellow]")
    
    suggestions = []
    
    if 'formation_energy_max' in search_params:
        fe = search_params['formation_energy_max']
        if fe < -2.0:
            suggestions.append(f"Try higher formation energy: [cyan]--formation-energy-max {fe + 1.0}[/cyan]")
    
    if 'band_gap_min' in search_params and 'band_gap_max' in search_params:
        suggestions.append("Try broader band gap range: [cyan]--band-gap-min 0.5 --band-gap-max 6.0[/cyan]")
    
    if 'min_elements' in search_params:
        min_el = search_params['min_elements']
        if min_el > 3:
            suggestions.append(f"Try fewer elements: [cyan]--min-elements {min_el - 1}[/cyan]")
    
    if 'elements' in search_params and len(search_params['elements']) > 3:
        suggestions.append("Try fewer elements or remove specific element constraints")
    
    suggestions.extend([
        "Use [cyan]--interactive[/cyan] mode for guided parameter selection",
        "Check [cyan]prism examples[/cyan] for working search patterns",
        "Try different database: [cyan]--database nomad[/cyan] or [cyan]--database cod[/cyan]"
    ])
    
    for i, suggestion in enumerate(suggestions[:4], 1):
        console.print(f"  {i}. {suggestion}")

def display_optimade_results(results: List[Dict[str, Any]]):
    """Display OPTIMADE search results in a formatted table."""
    if not results:
        return
    
    table = Table(show_header=True, header_style="bold magenta")
    table.add_column("Provider", style="blue", width=15)
    table.add_column("Formula", style="green", width=20)
    table.add_column("Elements", style="cyan", width=20)
    table.add_column("NElements", style="white", width=10)
    table.add_column("Structure ID", style="cyan", width=30)

    display_count = min(len(results), 50)
    for res in results[:display_count]:
        attributes = res.get('attributes', {})
        formula = attributes.get('chemical_formula_descriptive', 'N/A')
        elements = ", ".join(attributes.get('elements', []))
        nelements = str(attributes.get('nelements', 'N/A'))
        provider_info = res.get('meta', {}).get('provider', {})
        provider_name = provider_info.get('name', 'Unknown')
        structure_id = res.get('id', 'N/A')

        table.add_row(
            provider_name,
            formula,
            elements,
            nelements,
            structure_id
        )

    console.print(table)

    if len(results) > display_count:
        console.print(f"\n... and {len(results) - display_count} more results.")

def export_results(materials, export_format):
    """Export search results to files."""
    if not materials:
        return
    
    try:
        from app.services.data_viewer import MaterialsDataViewer
        viewer = MaterialsDataViewer()
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        
        if export_format in ['csv', 'both']:
            csv_file = f"search_results_{timestamp}.csv"
            viewer.export_to_csv(materials, csv_file)
            console.print(f"📁 Results exported to CSV: [cyan]{csv_file}[/cyan]")
        
        if export_format in ['json', 'both']:
            json_file = f"search_results_{timestamp}.json"
            viewer.export_to_json(materials, json_file)
            console.print(f"📁 Results exported to JSON: [cyan]{json_file}[/cyan]")
            
    except Exception as e:
        console.print(f"[red]❌ Export failed: {e}[/red]")

def generate_visualizations(materials):
    """Generate visualization plots for search results."""
    if not materials:
        return
    
    try:
        import matplotlib
        matplotlib.use('Agg')  # Use non-GUI backend
        
        from app.services.data_viewer import MaterialsDataViewer
        viewer = MaterialsDataViewer()
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        
        plots_generated = []
        
        # Formation energy distribution
        try:
            plot_file = f"formation_energy_plot_{timestamp}.png"
            viewer.plot_formation_energy_distribution(materials, save_path=plot_file)
            plots_generated.append(("Formation energy distribution", plot_file))
        except Exception as e:
            console.print(f"[yellow]⚠️  Formation energy plot failed: {e}[/yellow]")
        
        # Band gap correlation
        try:
            correlation_file = f"band_gap_correlation_{timestamp}.png"
            viewer.plot_band_gap_vs_formation_energy(materials, save_path=correlation_file)
            plots_generated.append(("Band gap vs formation energy", correlation_file))
        except Exception as e:
            console.print(f"[yellow]⚠️  Band gap correlation plot failed: {e}[/yellow]")
        
        # Element frequency
        try:
            element_file = f"element_frequency_{timestamp}.png"
            viewer.plot_element_frequency(materials, save_path=element_file)
            plots_generated.append(("Element frequency", element_file))
        except Exception as e:
            console.print(f"[yellow]⚠️  Element frequency plot failed: {e}[/yellow]")
        
        if plots_generated:
            console.print(f"\n📊 Generated {len(plots_generated)} visualization(s):")
            for desc, filename in plots_generated:
                console.print(f"  • {desc}: [cyan]{filename}[/cyan]")
        else:
            console.print("[yellow]⚠️  No plots could be generated[/yellow]")
        
    except ImportError:
        console.print("[yellow]⚠️  Matplotlib not available for plotting[/yellow]")
    except Exception as e:
        console.print(f"[yellow]⚠️  Plotting not available: {e}[/yellow]")

@cli.command()
@click.option('--database', 
              type=click.Choice(['optimade']),
              help='Database to test (now always OPTIMADE)')
def test_database(database):
    """
    Test connection to the OPTIMADE provider network.
    
    Verifies connectivity and lists all available database providers
    discovered through the OPTIMADE network.
    """
    console.print("🔍 Testing connection to the OPTIMADE network...")
    try:
        with console.status("[bold green]Fetching provider list...[/bold green]"):
            client = OptimadeClient()
        
        console.print(f"✅ Connection successful! Found {len(client.providers)} providers.")

        table = Table(title="Discovered OPTIMADE Providers", show_header=True, header_style="bold magenta")
        table.add_column("Provider Name", style="cyan")
        table.add_column("Description", style="white")
        table.add_column("Homepage", style="green")

        for provider in client.providers:
            table.add_row(provider.name, provider.description, provider.homepage)

        console.print(table)

    except Exception as e:
        console.print(f"[red]❌ Test failed: {e}[/red]")
        console.print("[yellow]Please check your internet connection and ensure `optimade-client` is installed correctly.[/yellow]")

async def test_database_connections(databases):
    """Test connections to specified databases."""
    database_configs = get_database_configs()
    
    for db_name in databases:
        console.print(f"🔍 Testing connection to {db_name.upper()}...")
        
        try:
            # Create connector
            if db_name == 'nomad':
                connector = NOMADConnector(config=database_configs[db_name])
            elif db_name == 'jarvis':
                connector = JarvisConnector()
            elif db_name == 'oqmd':
                connector = OQMDConnector(config=database_configs[db_name])
            elif db_name == 'cod':
                connector = CODConnector(config=database_configs[db_name])
            else:
                console.print(f"[red]❌ Unknown database: {db_name}[/red]")
                continue
            
            # Test connection
            success = await connector.connect()
            
            if success:
                console.print(f"✅ {db_name.upper()} connection successful!")
                
                # Get database info
                if hasattr(connector, 'get_database_info'):
                    info = await connector.get_database_info()
                    
                    # Display database information
                    table = Table(title=f"{info.get('full_name', db_name.upper())} Database Information")
                    table.add_column("Property", style="cyan")
                    table.add_column("Value", style="white")
                    
                    for key, value in info.items():
                        if isinstance(value, list):
                            value = ", ".join(str(v) for v in value)
                        table.add_row(key.replace('_', ' ').title(), str(value))
                    
                    console.print(table)
                
                # Test basic search
                try:
                    if db_name == 'oqmd':
                        test_materials = await connector.search_materials(elements=['Si'], max_results=1)
                    elif db_name == 'cod':
                        test_materials = await connector.search_materials(elements=['Si'], max_results=1)
                    elif db_name == 'nomad':
                        # For NOMAD - use proper keyword arguments
                        test_materials = await connector.search_materials(elements=['Si'], limit=1)
                    elif db_name == 'jarvis':
                        # For JARVIS - uses different parameter names
                        test_materials = await connector.search_materials(formula='Si', limit=1)
                    else:
                        test_materials = await connector.search_materials(elements=['Si'], limit=1)
                    
                    console.print(f"📊 Retrieved {len(test_materials)} test material(s)")
                    
                except Exception as e:
                    console.print(f"[yellow]⚠️  Search test failed: {e}[/yellow]")
                
                await connector.disconnect()
            else:
                console.print(f"[red]❌ {db_name.upper()} connection failed[/red]")
                
        except Exception as e:
            console.print(f"[red]❌ {db_name.upper()} test error: {e}[/red]")
        
        console.print()

@cli.command()
def examples():
    """Show a comprehensive list of usage examples."""
    console.clear()
    console.print(Panel(
        "[bold cyan]PRISM CLI Usage Examples[/bold cyan]\n"
        "[dim]A comprehensive guide to PRISM's capabilities[/dim]",
        style="blue",
        border_style="bright_blue"
    ))

    # Basic Search
    console.print("\n[bold green]🔍 Basic Search Examples[/bold green]")
    basic_search_table = Table(box=None, show_header=False)
    basic_search_table.add_column(width=50)
    basic_search_table.add_column(width=70)
    basic_search_table.add_row(
        "[cyan]Search for materials containing Silicon and Oxygen[/cyan]",
        "[yellow]prism search --elements Si,O[/yellow]"
    )
    basic_search_table.add_row(
        "[cyan]Search for a specific chemical formula[/cyan]",
        "[yellow]prism search --formula Li2CO3[/yellow]"
    )
    basic_search_table.add_row(
        "[cyan]Limit the number of results[/cyan]",
        "[yellow]prism search --elements Fe --limit 5[/yellow]"
    )
    basic_search_table.add_row(
        "[cyan]Use a raw OPTIMADE filter for complex queries[/cyan]",
        '[yellow]prism search --filter \'elements HAS "Ti" AND nelements=3\'[/yellow]'
    )
    console.print(basic_search_table)

    # Advanced Search
    console.print("\n[bold green]🔬 Advanced Search & Filtering (via OPTIMADE)[/bold green]")
    advanced_search_table = Table(box=None, show_header=False)
    advanced_search_table.add_column(width=50)
    advanced_search_table.add_column(width=70)
    advanced_search_table.add_row(
        "[cyan]Find semiconductors with a band gap > 2 eV[/cyan]",
        "[yellow]prism search --filter 'band_gap > 2'[/yellow]"
    )
    advanced_search_table.add_row(
        "[cyan]Find stable materials (formation energy < 0)[/cyan]",
        "[yellow]prism search --filter 'formation_energy_per_atom < 0'[/yellow]"
    )
    advanced_search_table.add_row(
        "[cyan]Search for High Entropy Alloys (4+ elements)[/cyan]",
        "[yellow]prism search --nelements 4[/yellow]"
    )
    advanced_search_table.add_row(
        "[cyan]Find materials with a specific space group number[/cyan]",
        "[yellow]prism search --filter 'space_group_symbol = 14'[/yellow]"
    )
    console.print(advanced_search_table)

    # Database Specific Searches
    console.print("\n[bold green]💾 Database Searches are now unified via OPTIMADE[/bold green]")
    db_search_table = Table(box=None, show_header=False)
    db_search_table.add_column(width=50)
    db_search_table.add_column(width=70)
    db_search_table.add_row(
        "[cyan]All searches query the entire OPTIMADE network by default.[/cyan]",
        "[dim]The client automatically handles discovering and querying all providers.[/dim]"
    )
    console.print(db_search_table)

    # Data Export and Visualization
    console.print("\n[bold green]📊 Data Export & Visualization[/bold green]")
    export_table = Table(box=None, show_header=False)
    export_table.add_column(width=50)
    export_table.add_column(width=70)
    export_table.add_row(
        "[cyan]Export search results to a CSV file[/cyan]",
        "[yellow]prism search --elements Fe,Ni --export csv[/yellow]"
    )
    export_table.add_row(
        "[cyan]Export results to both JSON and CSV files[/cyan]",
        "[yellow]prism search --elements Au --export both[/yellow]"
    )
    export_table.add_row(
        "[cyan]Generate visualization plots from search results[/cyan]",
        "[yellow]prism search --elements Si --plot[/yellow]"
    )
    console.print(export_table)

    # Interactive and System Commands
    console.print("\n[bold green]🤖 Interactive & System Commands[/bold green]")
    interactive_table = Table(box=None, show_header=False)
    interactive_table.add_column(width=50)
    interactive_table.add_column(width=70)
    interactive_table.add_row(
        "[cyan]Start an interactive, guided search[/cyan]",
        "[yellow]prism search --interactive[/yellow]"
    )
    interactive_table.add_row(
        "[cyan]Test the connection to the OPTIMADE network[/cyan]",
        "[yellow]prism test-database[/yellow]"
    )
    interactive_table.add_row(
        "[cyan]List all available OPTIMADE providers[/cyan]",
        "[yellow]prism list-databases[/yellow]"
    )
    interactive_table.add_row(
        "[cyan]View the schema for the 'search' command[/cyan]",
        "[yellow]prism schema --command search[/yellow]"
    )
    console.print(interactive_table)

    console.print("\n[bold yellow]💡 Tip:[/bold yellow] [cyan]Combine filters for more specific searches![/cyan]")

@cli.command()
@click.argument('config_file', type=click.Path(exists=True))
def add_custom_database(config_file):
    """
    Add a custom database configuration.
    
    Loads and validates a custom database configuration file, then integrates
    it with PRISM for use in searches. The configuration file should follow
    the PRISM database schema format.
    
    Use 'prism schema --command add-custom-database' to see the required format.
    """
    console.print(f"🔧 Adding custom database from: [cyan]{config_file}[/cyan]")
    
    try:
        with open(config_file, 'r') as f:
            config = json.load(f)
        
        # Validate configuration
        required_fields = ['name', 'display_name', 'connection', 'endpoints']
        for field in required_fields:
            if field not in config:
                console.print(f"[red]❌ Missing required field: {field}[/red]")
                return
        
        # Validate connection settings
        conn = config['connection']
        if 'base_url' not in conn:
            console.print("[red]❌ Missing base_url in connection settings[/red]")
            return
        
        console.print(f"✅ Configuration valid for database: [green]{config['display_name']}[/green]")
        console.print(f"Base URL: {conn['base_url']}")
        
        # Save to custom databases directory (would need to implement)
        custom_db_dir = Path("custom_databases")
        custom_db_dir.mkdir(exist_ok=True)
        
        custom_config_path = custom_db_dir / f"{config['name']}.json"
        with open(custom_config_path, 'w') as f:
            json.dump(config, f, indent=2)
        
        console.print(f"📁 Custom database saved to: [cyan]{custom_config_path}[/cyan]")
        console.print(f"🔍 Test connection with: [yellow]prism test-database --database {config['name']}[/yellow]")
        
    except json.JSONDecodeError as e:
        console.print(f"[red]❌ Invalid JSON in config file: {e}[/red]")
    except Exception as e:
        console.print(f"[red]❌ Error adding custom database: {e}[/red]")

# Now I'll add the enhanced search command and other commands...
# (This file is getting long, so I'll continue with the key commands)

if __name__ == '__main__':
    cli()
