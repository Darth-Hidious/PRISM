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
from app.db.database import Base, engine, get_db
from app.db.models import Material
from app.llm import get_llm_service
from app.mcp import ModelContext, AdaptiveOptimadeFilter
from app.prompts import ROUTER_PROMPT, SUMMARIZATION_PROMPT

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
def cli(ctx):
    """
    PRISM: A command-line tool for materials science research.

    This tool provides access to a network of materials science databases
    through the OPTIMADE API. You can perform structured searches or use
    natural language queries powered by Large Language Models (LLMs).

    Running PRISM without any subcommands will start the interactive 'ask' mode.
    """
    if ctx.invoked_subcommand is None:
        try:
            console.print(Panel(PRISM_BRAND, style="bold blue", title="PRISM"))
        except UnicodeEncodeError:
            # Fallback for Windows console encoding issues
            print("=" * 80)
            print("PRISM - Platform for Research in Intelligent Synthesis of Materials")
            print("=" * 80)
        
        # Check LLM configuration status
        try:
            llm_service = get_llm_service()
            llm_configured = True
            # Determine which service is configured
            if os.getenv("OPENAI_API_KEY"):
                llm_provider = f"OpenAI ({llm_service.model})"
            elif os.getenv("GOOGLE_CLOUD_PROJECT"):
                llm_provider = f"Google Vertex AI ({llm_service.model})"
            elif os.getenv("ANTHROPIC_API_KEY"):
                llm_provider = f"Anthropic ({llm_service.model})"
            elif os.getenv("OPENROUTER_API_KEY"):
                llm_provider = f"OpenRouter ({llm_service.model})"
            elif os.getenv("PERPLEXITY_API_KEY"):
                llm_provider = "Perplexity (coming soon)"
            elif os.getenv("GROK_API_KEY"):
                llm_provider = "Grok (coming soon)"
            elif os.getenv("OLLAMA_HOST"):
                llm_provider = "Ollama Local (coming soon)"
            elif os.getenv("PRISM_CUSTOM_API_KEY"):
                llm_provider = "PRISM Custom Model (coming soon)"
            else:
                llm_provider = "Unknown"
        except (ValueError, NotImplementedError):
            llm_configured = False
            llm_provider = "Not configured"
        
        # Show system status with quick switcher
        try:
            if llm_configured:
                console.print(f"\n[green]STATUS:[/green] LLM Provider: [cyan]{llm_provider}[/cyan] [green]✓[/green]")
                console.print("[dim]Quick switch: Use 'prism switch-llm' or press 's' below[/dim]")
            else:
                console.print(f"\n[yellow]STATUS:[/yellow] LLM Provider: [red]{llm_provider}[/red] [red]✗[/red]")
                console.print("[yellow]Note: Run 'prism advanced configure' to set up LLM provider for 'ask' command[/yellow]")
        except UnicodeEncodeError:
            if llm_configured:
                print(f"\nSTATUS: LLM Provider: {llm_provider} (configured)")
                print("Quick switch: Use 'prism switch-llm' or press 's' below")
            else:
                print(f"\nSTATUS: LLM Provider: {llm_provider}")
                print("Note: Run 'prism advanced configure' to set up LLM provider for 'ask' command")
        
        # Show available commands
        try:
            console.print("\n[bold green]Available Commands:[/bold green]")
            console.print("• [cyan]search[/cyan]    - Search materials databases with specific criteria")
            if llm_configured:
                console.print("• [cyan]ask[/cyan]       - Ask questions using natural language")
                console.print("  [dim]--interactive[/dim] - Get targeted questions to refine your search")
                console.print("  [dim]--reason[/dim]      - Enable multi-step reasoning analysis")
            else:
                console.print("• [dim cyan]ask[/dim cyan]       - Ask questions using natural language [dim](requires LLM setup)[/dim]")
            console.print("• [cyan]switch-llm[/cyan] - Quick switch between LLM providers")
            console.print("• [cyan]optimade[/cyan]  - OPTIMADE network tools (list-dbs)")
            console.print("• [cyan]advanced[/cyan]  - Database and configuration management")
            console.print("• [cyan]docs[/cyan]      - Generate documentation files")
            console.print("\nUse [cyan]prism COMMAND --help[/cyan] for detailed information about each command.")
        except UnicodeEncodeError:
            print("\nAvailable Commands:")
            print("• search    - Search materials databases with specific criteria")
            if llm_configured:
                print("• ask       - Ask questions using natural language")
                print("  --interactive - Get targeted questions to refine your search")
                print("  --reason      - Enable multi-step reasoning analysis")
            else:
                print("• ask       - Ask questions using natural language (requires LLM setup)")
            print("• switch-llm - Quick switch between LLM providers")
            print("• optimade  - OPTIMADE network tools (list-dbs)")
            print("• advanced  - Database and configuration management")
            print("• docs      - Generate documentation files")
            print("\nUse 'prism COMMAND --help' for detailed information about each command.")
        
        # Prompt for question or quick actions
        if llm_configured:
            try:
                query = Prompt.ask("\n[bold cyan]Ask a question about materials science, press 's' to switch LLM, or Enter to exit[/bold cyan]", default="")
            except UnicodeEncodeError:
                # Fallback prompt
                query = input("\nAsk a question about materials science, press 's' to switch LLM, or Enter to exit: ").strip()
                    
            if query == "s" or query.lower() == "switch":
                ctx.invoke(switch_llm)
            elif query:
                ctx.invoke(ask, query=query)
        else:
            try:
                console.print("\n[yellow]To use the 'ask' command, please run:[/yellow] [cyan]prism advanced configure[/cyan]")
            except UnicodeEncodeError:
                print("\nTo use the 'ask' command, please run: prism advanced configure")

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
    
    # Add coming soon providers (for display only)
    coming_soon = []
    if os.getenv("PERPLEXITY_API_KEY"):
        coming_soon.append("Perplexity")
    if os.getenv("GROK_API_KEY"):
        coming_soon.append("Grok")
    if os.getenv("OLLAMA_HOST"):
        coming_soon.append("Ollama Local")
    if os.getenv("PRISM_CUSTOM_API_KEY"):
        coming_soon.append("PRISM Custom Model")
    
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
    
    if coming_soon:
        console.print(f"\n[dim]Coming Soon:[/dim]")
        for provider in coming_soon:
            console.print(f"• [dim]{provider} (configured but not yet supported)[/dim]")
    
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
def search(elements, formula, nelements, providers):
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
            client = OptimadeClient(include_providers=providers.split(',') if providers else None)
            results = client.get(optimade_filter)

        # The optimade-client returns a nested dictionary. We need to extract the actual list of materials.
        all_materials = []
        if "structures" in results:
            for provider_results in results["structures"].get(optimade_filter, {}).values():
                if "data" in provider_results:
                    all_materials.extend(provider_results["data"])

        if all_materials:
            console.print(f"[green]SUCCESS:[/green] Found {len(all_materials)} materials.")

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
def ask(query: str, providers: str, interactive: bool, reason: bool, debug_filter: str):
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
            
            # Generate the filter with iterative refinement
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

        with console.status(f"[bold green]Querying {provider_to_query}...[/bold green]"):
            client = OptimadeClient(include_providers=[provider_to_query])
            search_results = client.get(optimade_filter)
        
        all_materials = []
        if "structures" in search_results:
            for provider_results in search_results["structures"].get(optimade_filter, {}).values():
                if "data" in provider_results:
                    all_materials.extend(provider_results["data"])

        if not all_materials:
            console.print("[red]ERROR:[/red] No materials found for the generated filter.")
            return

        # Display results in a table
        console.print(f"[green]SUCCESS:[/green] Found {len(all_materials)} materials. Showing top 10.")
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
            try:
                with open("Schema.txt", "r") as f:
                    schema_content = f.read()
            except FileNotFoundError:
                console.print("[yellow]Warning: Schema.txt not found. Proceeding without schema context.[/yellow]")
                schema_content = None

            model_context = ModelContext(query=query, results=all_materials, rag_context=schema_content)
            final_prompt = model_context.to_prompt(reasoning_mode=reason)
            
            # Stream the response from the LLM for a better user experience
            stream = llm_service.get_completion(final_prompt, stream=True)

            answer_title = "Reasoning Analysis" if reason else "Answer"
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
            
            panel_title = "Reasoning Analysis" if reason else "Answer"
            panel_style = "cyan" if reason else "magenta"
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
    console.print("[dim]5. Perplexity (coming soon)[/dim]")
    console.print("[dim]6. Grok (coming soon)[/dim]")
    console.print("[dim]7. Ollama Local (coming soon)[/dim]")
    console.print("[dim]8. PRISM Custom Model (coming soon)[/dim]")
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
- **Save Results** - run `prism advanced init` to enable local data persistence

## 🔬 Advanced Features

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
# CLI Entry Point
# =================================================_
cli.add_command(advanced)
cli.add_command(docs)
cli.add_command(optimade)

if __name__ == "__main__":
    cli()
