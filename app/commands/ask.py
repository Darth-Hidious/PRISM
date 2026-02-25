"""Ask CLI command: natural-language materials science queries via LLM + OPTIMADE."""
import click
from rich.console import Console
from rich.panel import Panel
from rich.prompt import Prompt
from rich.table import Table

from app.config.providers import FALLBACK_PROVIDERS
from app.commands.search import _make_optimade_client, enrich_materials_with_mp_data


@click.command()
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
    console = Console(force_terminal=True, width=120)

    from app.llm import get_llm_service
    from app.mcp import ModelContext, AdaptiveOptimadeFilter

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
                temp_client = _make_optimade_client(max_results=1)
                provider_to_query, optimade_filter, error = adaptive_filter.generate_filter(query, temp_client)
                if error:
                    console.print(f"[red]Error: {error}[/red]")
                    return
        else:
            console.print("[yellow]No conversation data collected. Using original query.[/yellow]")
            # Fall back to normal generation
            temp_client = _make_optimade_client(max_results=1)
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
            temp_client = _make_optimade_client(max_results=1)

            # Generate the filter - use reasoning mode if --reason flag is set
            if reason:
                provider_to_query, optimade_filter, reasoning_response = adaptive_filter.generate_reasoning_filter(query, None, console)
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
            info_client = _make_optimade_client(
                providers=[provider_to_query],
                max_results=limit,
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
            model_context = ModelContext(query=query, results=all_materials, rag_context=None)
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
