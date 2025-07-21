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

import click
from rich.console import Console
from rich.panel import Panel
from rich.prompt import Prompt, Confirm, IntPrompt
from rich.table import Table
from sqlalchemy.exc import OperationalError
from optimade.client import OptimadeClient

from app.config.branding import PRISM_BRAND
from app.config.providers import FALLBACK_PROVIDERS
from app.db.database import Base, engine, get_db
from app.db.models import Material
from app.llm import get_llm_service
from app.mcp import ModelContext
from app.prompts import OPTIMADE_PROMPT

# ==============================================================================
# Setup
# ==============================================================================
console = Console()

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
def cli(ctx):
    """
    PRISM: A command-line tool for materials science research.

    This tool provides access to a network of materials science databases
    through the OPTIMADE API. You can perform structured searches or use
    natural language queries powered by Large Language Models (LLMs).

    Running PRISM without any subcommands will start the interactive 'ask' mode.
    """
    if ctx.invoked_subcommand is None:
        console.print(Panel(PRISM_BRAND, style="bold blue", title="PRISM"))
        query = Prompt.ask("[bold cyan]Ask a question about materials science[/bold cyan]", default=None)
        if query:
            ctx.invoke(ask, query=query)

# ==============================================================================
# 'search' Command
# ==============================================================================
@cli.command()
@click.option('--elements', help='Comma-separated list of elements (e.g., "Si,O").')
@click.option('--formula', help='Chemical formula (e.g., "SiO2").')
@click.option('--nelements', type=int, help='Number of elements in the material.')
@click.option('--providers', help='Comma-separated list of provider IDs (e.g., "mp,oqmd,cod").')
def search(elements, formula, nelements, providers):
    """
    Performs a structured search of the OPTIMADE network based on specific criteria.
    """
    # Ensure at least one search criterion is provided
    if not any([elements, formula, nelements]):
        console.print("[red]Error: Please provide at least one search criterion (e.g., --elements, --formula).[/red]")
        return
    
    optimade_filter = build_optimade_filter(
        elements=elements.split(',') if elements else None,
        formula=formula,
        nelements=nelements
    )
    
    console.print(Panel(f"🔍 [bold]Filter:[/bold] [cyan]{optimade_filter}[/cyan]", title="Search Query", border_style="blue"))

    try:
        with console.status("[bold green]Querying OPTIMADE providers...[/bold green]"):
            # If specific providers are requested, use them. Otherwise, search all.
            client = OptimadeClient(include_providers=providers.split(',') if providers else None)
            results = client.get(optimade_filter)

        # The optimade-client returns a nested dictionary. We need to extract the actual list of materials.
        all_materials = []
        if "structures" in results:
            for provider_results in results["structures"].get(optimade_filter, {}).values():
                if "data" in provider_results:
                    all_materials.extend(provider_results["data"])

        if all_materials:
            console.print(f"✅ Found {len(all_materials)} materials.")

            # Display results in a table
            table = Table(show_header=True, header_style="bold magenta")
            table.add_column("Source ID")
            table.add_column("Formula")
            table.add_column("Elements")
            
            # Show only the first 10 results for brevity
            for material in all_materials[:10]:
                attrs = material.get("attributes", {})
                table.add_row(
                    str(material.get("id")),
                    attrs.get("chemical_formula_descriptive", "N/A"),
                    ", ".join(attrs.get("elements", []))
                )
            console.print(Panel(table, title="Search Results", border_style="green"))

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
                    console.print("✅ Results saved to the database.")
                    # Inform the user where the database is located
                    db_path = os.path.abspath(engine.url.database)
                    console.print(f"Database located at: [green]{db_path}[/green]")

                except Exception as e:
                    console.print(f"[bold red]An unexpected error occurred during save: {e}[/bold red]")
        else:
            console.print("❌ No materials found for the given filter.")

    except Exception as e:
        console.print(f"[bold red]An error occurred during search: {e}[/bold red]")

# ==============================================================================
# 'ask' Command
# ==============================================================================
@cli.command()
@click.argument("query", required=False)
@click.option('--providers', help='Comma-separated list of provider IDs (e.g., "cod,mp").')
@click.option('--interactive', is_flag=True, help='Enable interactive mode to refine the query.')
@click.option('--debug-filter', help='(Dev) Bypass LLM and use this exact OPTIMADE filter.')
def ask(query: str, providers: str, interactive: bool, debug_filter: str):
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

    # Interactive mode allows the user to refine the query with more filters
    if interactive:
        console.print("[bold cyan]Entering interactive mode...[/bold cyan]")
        clarification = Prompt.ask(f"Your query is: '{query}'. Do you want to add any other filters (e.g., band_gap > 1, nelements=3)? If not, just press enter.")
        if clarification:
            query += " and " + clarification
        
        provider_list = Prompt.ask("Which providers should I search? (e.g., 'cod,mp,oqmd', press enter for all)")
        providers = provider_list if provider_list else None

    try:
        # If a debug filter is provided, bypass the LLM for filter generation
        if debug_filter:
            optimade_filter = debug_filter
        else:
            with console.status("[bold green]Generating OPTIMADE filter from your query...[/bold green]"):
                # Use the LLM to extract key elements from the user's query
                filter_prompt = OPTIMADE_PROMPT.format(query=query)
                filter_response = llm_service.get_completion(filter_prompt)
                
                # The response from the LLM can come in different formats depending on the provider
                elements_str = ""
                if hasattr(filter_response, 'choices'):
                    elements_str = filter_response.choices[0].message.content.strip()
                elif hasattr(filter_response, 'content'):
                    elements_str = filter_response.content[0].text.strip()
                else: # Fallback for providers that return a simple text response
                    elements_str = filter_response.text.strip()
                
                elements = [e.strip() for e in elements_str.split(',')]
                optimade_filter = build_optimade_filter(elements=elements)
        
        console.print(Panel(f"🔍 [bold]Generated Filter:[/bold] [cyan]{optimade_filter}[/cyan]", title="Query Analysis", border_style="blue"))

        with console.status("[bold green]Searching across the OPTIMADE network...[/bold green]"):
            client = OptimadeClient(include_providers=providers.split(',') if providers else None)
            search_results = client.get(optimade_filter)
        
        all_materials = []
        if "structures" in search_results:
            for provider_results in search_results["structures"].get(optimade_filter, {}).values():
                if "data" in provider_results:
                    all_materials.extend(provider_results["data"])

        if not all_materials:
            console.print("❌ No materials found for the generated filter.")
            return

        # Display results in a table
        console.print(f"✅ Found {len(all_materials)} materials. Showing top 10.")
        table = Table(show_header=True, header_style="bold magenta")
        table.add_column("Source ID")
        table.add_column("Formula")
        table.add_column("Elements")
        
        for material in all_materials[:10]:
            attrs = material.get("attributes", {})
            table.add_row(
                str(material.get("id")),
                attrs.get("chemical_formula_descriptive", "N/A"),
                ", ".join(attrs.get("elements", []))
            )
        console.print(Panel(table, title="Top 10 Search Results", border_style="green"))

        # Use the LLM to summarize the findings
        with console.status("[bold green]Analyzing results and generating answer...[/bold green]"):
            model_context = ModelContext(query=query, results=all_materials)
            final_prompt = model_context.to_prompt()
            
            # Stream the response from the LLM for a better user experience
            stream = llm_service.get_completion(final_prompt, stream=True)

            console.print("\n[bold green]Answer:[/bold green]")
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
            console.print(Panel("".join(full_response), title="Answer", border_style="magenta", title_align="left"))

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
    console.print("✅ Database initialized.")

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

    console.print(f"✅ Configuration saved to {env_path}")


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
  <img src="https://i.imgur.com/your-logo-url.png" alt="PRISM Logo" width="200"/>
</p>

<p align="center">
    <em>A next-generation command-line interface for materials science research, powered by the OPTIMADE API network and Large Language Models.</em>
</p>

---

PRISM is a powerful, intuitive tool designed to streamline the process of materials discovery. It provides a single, unified interface to query dozens of major materials science databases and leverages the latest advances in AI to make your search process more natural and efficient.

## Core Concepts

- **OPTIMADE**: PRISM is built on the [Open Databases Integration for Materials Design (OPTIMADE)](https://www.optimade.org/) API specification. This allows PRISM to communicate with a wide range of materials databases using a single, standardized query language.
- **MCP (Model Context Protocol)**: This is the internal system that allows PRISM to translate between human language and the structured query language of OPTIMADE. When you use the `ask` command, the MCP takes your question, uses an LLM to extract the key scientific concepts, and then constructs a precise OPTIMADE filter to find the data you need.
- **BYOK (Bring Your Own Key)**: PRISM is designed to be used with your own API keys for various LLM providers. This ensures that you have full control over your usage and costs.

## Features

- **Unified Search**: Query dozens of materials databases (including Materials Project, OQMD, COD, and more) with a single `search` command.
- **Intelligent Search (`ask`)**: Use natural language to ask questions about materials (e.g., `"Find me all materials containing cobalt and lithium"`). PRISM uses an LLM to translate your query into a precise OPTIMADE filter, searches the databases, and provides a summarized, easy-to-understand answer.
- **Interactive Mode (`ask --interactive`)**: Refine your queries through a conversation with the built-in LLM research assistant. If your query is ambiguous, PRISM will ask you clarifying questions to help you narrow down your search.
- **Local Database**: Save your search results to a local SQLite database for persistence, analysis, and future reference.
- **Pluggable LLM Providers**: Bring your own API key for a variety of LLM providers, including OpenAI, Google Vertex AI, Anthropic, and OpenRouter.
- **Provider Discovery**: List all available OPTIMADE databases with the `optimade list-dbs` command.

## Command Reference

A detailed look at the available commands and their options.

---
### `prism search`
Performs a structured search of the OPTIMADE network. This command is best for when you know the specific properties of the materials you are looking for.

**Usage:**
```bash
prism search [OPTIONS]
```

**Options:**
- `--elements TEXT`: Comma-separated list of elements the material must contain (e.g., `"Si,O"`).
- `--formula TEXT`: An exact chemical formula (e.g., `"SiO2"`).
- `--nelements INTEGER`: The exact number of elements in the material.
- `--providers TEXT`: A comma-separated list of OPTIMADE provider IDs to search. By default, it searches all providers.

**Examples:**
```bash
# Find all materials containing Iron, Nickel, and Chromium
prism search --elements "Fe,Ni,Cr"

# Find materials with the exact formula for silicon carbide
prism search --formula "SiC"

# Find all binary compounds containing Cobalt from the OQMD and Materials Project databases
prism search --elements "Co" --nelements 2 --providers "oqmd,mp"
```
---
### `prism ask`
Asks a question about materials science using natural language. This command is best for exploratory searches or when you are not sure of the exact chemical properties.

**Usage:**
```bash
prism ask "[QUERY]" [OPTIONS]
```

**Options:**
- `--providers TEXT`: A comma-separated list of provider IDs to search.
- `--interactive`: Enables a conversational mode where PRISM will ask clarifying questions to refine your search.

**Examples:**
```bash
# General query
prism ask "What are the known binary compounds of silicon and carbon?"

# A more complex query targeting specific databases
prism ask "high entropy alloys containing molybdenum" --providers "oqmd"

# Start an interactive session to find a semiconductor
prism ask "I need to find a good semiconductor for a high-power application" --interactive
```
---
### `prism optimade list-dbs`
Lists all available OPTIMADE provider databases that PRISM can search. This is useful for finding the provider IDs to use with the `--providers` option in the `search` and `ask` commands.
---
### `prism advanced`
Advanced commands for database management and application configuration.

- `prism advanced init`: Initializes the local SQLite database. This is required if you want to save search results.
- `prism advanced configure`: Guides you through setting up your database connection and LLM provider. This is required to use the `ask` command.
---
### `prism docs`
Commands for generating the project documentation.

- `prism docs save-readme`: Saves this README file to the project root.
- `prism docs save-install`: Saves the `INSTALL.md` file to the project root.

## Quick Start

1.  **Installation**: See the `INSTALL.md` file for detailed instructions.
2.  **Configuration**: To use the `ask` command, you must first configure your preferred LLM provider. PRISM will guide you through this process.
    ```bash
    prism advanced configure
    ```
    You will be prompted to choose an LLM provider (like OpenAI, OpenRouter, etc.) and enter your API key. For the easiest setup, we recommend the **OpenRouter** option.

3.  **Initialize the Database (Optional)**: If you want to save your search results, you first need to initialize the local database.
    ```bash
    prism advanced init
    ```
4.  **Run a Search**:
    ```bash
    prism search --elements "Ti,O" --nelements 2
    ```
5.  **Ask a Question**:
    ```bash
    prism ask "Find me materials containing titanium and oxygen"
    ```
"""

INSTALL_CONTENT = """
# Installation Guide

Follow these steps to get PRISM up and running on your system.

## Prerequisites

- **Python**: PRISM requires Python version 3.9, 3.10, 3.11, or 3.12. It is **not** compatible with Python 3.13 or newer due to a dependency conflict.
- **Git**: For cloning the repository.

## Installation Steps

1.  **Clone the Repository**
    ```bash
    git clone <repository-url>
    cd PRISM
    ```

2.  **Create and Activate a Virtual Environment**
    It is highly recommended to install PRISM in a dedicated virtual environment.
    ```bash
    # Create the virtual environment
    python -m venv .venv

    # Activate it (on macOS/Linux)
    source .venv/bin/activate

    # Or on Windows
    .\\venv\\Scripts\\activate
    ```

3.  **Install Dependencies**
    The project uses `pyproject.toml` to manage dependencies. Install the project in editable mode, which will also install all required packages.
    ```bash
    pip install -e .
    ```

4.  **Configure PRISM**
    Before you can use the `ask` command, you need to configure your preferred LLM provider.
    ```bash
    prism advanced configure
    ```
    This will prompt you to select a provider and enter your API key. 
    
    **💡 Tip:** For the quickest start, we recommend choosing the **OpenRouter** option. It's free and only requires a single API key to get started.

5.  **Initialize the Database (Optional but Recommended)**
    To save search results, you need to initialize the local SQLite database.
    ```bash
    prism advanced init
    ```
    The `search` command will also prompt you to do this automatically if you try to save results to an uninitialized database.

You are now ready to use PRISM!
"""

@docs.command()
def save_readme():
    """Saves the project README.md file."""
    with open("README.md", "w") as f:
        f.write(README_CONTENT)
    console.print("✅ `README.md` saved successfully.")

@docs.command()
def save_install():
    """Saves the project INSTALL.md file."""
    with open("INSTALL.md", "w") as f:
        f.write(INSTALL_CONTENT)
    console.print("✅ `INSTALL.md` saved successfully.")


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
# CLI Entry Point
# =================================================_
cli.add_command(advanced)
cli.add_command(docs)
cli.add_command(optimade)

if __name__ == "__main__":
    cli()
