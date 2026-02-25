"""Advanced CLI command group: database management and LLM configuration."""
from pathlib import Path

import click
from rich.console import Console
from rich.prompt import Prompt, IntPrompt


@click.group()
def advanced():
    """Advanced commands for database management and configuration."""
    pass


@advanced.command()
def init():
    """Initializes the database, creating the necessary tables."""
    console = Console(force_terminal=True, width=120)

    from app.db.database import Base, engine
    console.print("Initializing database...")
    Base.metadata.create_all(bind=engine)
    console.print("[green]SUCCESS:[/green] Database initialized.")


@advanced.command()
def configure():
    """Configures the database connection and LLM provider."""
    console = Console(force_terminal=True, width=120)
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
