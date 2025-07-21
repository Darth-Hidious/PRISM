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
import openai

from . import crud, schemas
from .db import models
from .db.database import Base, engine, get_db
from .db.models import Material
from .mcp import ModelContext
from .prompts import OPTIMADE_PROMPT
from .llm import get_llm_service, OpenAIService, VertexAIService, AnthropicService

console = Console()

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

@click.group()
def cli():
    """PRISM: A command-line tool for materials science."""
    pass

@cli.command()
@click.option('--elements', help='Comma-separated list of elements.')
@click.option('--formula', help='Chemical formula.')
@click.option('--nelements', type=int, help='Number of elements.')
def search(elements, formula, nelements):
    """Search for materials using an OPTIMADE filter."""
    optimade_filter = build_optimade_filter(
        elements=elements.split(',') if elements else None,
        formula=formula,
        nelements=nelements
    )
    
    console.print(f"🔍 Searching for materials with filter: {optimade_filter}")
    
    try:
        client = OptimadeClient()
        results = client.get(optimade_filter)
        
        if "data" in results and results["data"]:
            materials = results["data"]
            console.print(f"✅ Found {len(materials)} materials.")
            
            table = Table(show_header=True, header_style="bold magenta")
            table.add_column("ID")
            table.add_column("Formula")
            
            for material in materials:
                table.add_row(material["id"], material["attributes"]["chemical_formula_descriptive"])
            
            console.print(table)
            
            db = next(get_db())
            for material in materials:
                db_material = Material(
                    id=material["id"],
                    formula=material["attributes"]["chemical_formula_descriptive"],
                    elements=",".join(material["attributes"]["elements"])
                )
                db.add(db_material)
            db.commit()
            console.print("✅ Results saved to the database.")
        else:
            console.print("No materials found for the given filter.")
            
    except Exception as e:
        console.print(f"[red]An error occurred: {e}[/red]")

@cli.command()
@click.argument("query")
def ask(query: str):
    """Ask a question about materials science."""
    llm_service = get_llm_service()
    if not llm_service:
        console.print("[red]LLM service not configured. Please run 'prism db configure' to set it.[/red]")
        return
    
    try:
        # Step 1: Generate OPTIMADE filter
        with console.status("[bold green]Generating OPTIMADE filter from your query...[/bold green]"):
            filter_prompt = f"Based on the following query, generate an OPTIMADE filter string to find relevant materials. Query: {query}"
            filter_response = llm_service.get_completion(filter_prompt)

            if isinstance(llm_service, AnthropicService):
                optimade_filter = filter_response.content[0].text.strip()
            elif isinstance(llm_service, OpenAIService):
                optimade_filter = filter_response.choices[0].message.content.strip()
            else: # VertexAI
                optimade_filter = filter_response.text.strip()
            
            console.print(f"🔍 Generated OPTIMADE filter: {optimade_filter}")

        # Step 2: Search with OptimadeClient
        with console.status("[bold green]Searching across the OPTIMADE network...[/bold green]"):
            optimade_client = OptimadeClient()
            search_results = optimade_client.get(optimade_filter)
            
            if not isinstance(search_results, dict):
                raise TypeError("search_results must be a dictionary.")
        
        # Step 3: Create ModelContext
        with console.status("[bold green]Analyzing search results...[/bold green]"):
            model_context = ModelContext(query=query, results=search_results.get("data", []))

        # Step 4: Generate final answer
        with console.status("[bold green]Generating final answer...[/bold green]"):
            stream = llm_service.get_completion(model_context.to_prompt(), stream=True)
            if isinstance(llm_service, AnthropicService):
                with stream as s:
                    for event in s:
                        if event.type == "content_block_delta":
                            console.print(event.delta.text, end="")
            else:
                for chunk in stream:
                    if hasattr(chunk.choices[0].delta, "content"):
                        console.print(chunk.choices[0].delta.content or "", end="")
                    else:
                        console.print(chunk.text or "", end="")

    except Exception as e:
        console.print(f"[red]An error occurred: {e}[/red]")

@click.group()
def db():
    """Commands for managing the database."""
    pass

@db.command()
def init():
    """Initializes the database."""
    Base.metadata.create_all(bind=engine)
    console.print("Database initialized.")

@db.command()
def clear():
    """Clears all data from the database."""
    if Confirm.ask("Are you sure you want to clear the database?"):
        Base.metadata.drop_all(bind=engine)
        Base.metadata.create_all(bind=engine)
        console.print("Database cleared.")

@db.command()
def status():
    """Checks the status of the database connection."""
    try:
        get_db()
        console.print("Database connection successful.")
    except Exception as e:
        console.print(f"[red]Database connection failed: {e}[/red]")

@db.command()
def configure():
    """Configures the database and LLM provider."""
    # ... existing code ...
cli.add_command(db)

if __name__ == "__main__":
    cli()
