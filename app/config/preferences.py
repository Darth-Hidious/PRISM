"""User preferences for PRISM workflows."""

import json
import os
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import List, Optional


PRISM_DIR = Path.home() / ".prism"
PREFERENCES_PATH = PRISM_DIR / "preferences.json"
ENV_PATH = Path.cwd() / ".env"


@dataclass
class UserPreferences:
    """Persistent user preferences for skill execution."""

    output_format: str = "csv"  # csv, parquet, both
    output_dir: str = "output"
    default_providers: List[str] = field(default_factory=lambda: ["optimade"])
    max_results_per_source: int = 100
    default_algorithm: str = "random_forest"
    report_format: str = "markdown"  # markdown, pdf
    compute_budget: str = "local"  # local, hpc
    hpc_queue: str = "default"
    hpc_cores: int = 4
    check_updates: bool = True
    onboarding_complete: bool = False

    @classmethod
    def load(cls) -> "UserPreferences":
        """Load preferences from ~/.prism/preferences.json, or return defaults."""
        if PREFERENCES_PATH.exists():
            try:
                data = json.loads(PREFERENCES_PATH.read_text())
                # Only use keys that are valid fields
                valid = {f.name for f in cls.__dataclass_fields__.values()}
                filtered = {k: v for k, v in data.items() if k in valid}
                return cls(**filtered)
            except (json.JSONDecodeError, TypeError):
                return cls()
        return cls()

    def save(self) -> Path:
        """Persist preferences to ~/.prism/preferences.json."""
        PRISM_DIR.mkdir(parents=True, exist_ok=True)
        PREFERENCES_PATH.write_text(json.dumps(asdict(self), indent=2))
        return PREFERENCES_PATH

    @staticmethod
    def is_first_run() -> bool:
        """True if the user has never completed onboarding."""
        if not PREFERENCES_PATH.exists():
            return True
        try:
            data = json.loads(PREFERENCES_PATH.read_text())
            return not data.get("onboarding_complete", False)
        except (json.JSONDecodeError, TypeError):
            return True

    @staticmethod
    def has_llm_key() -> bool:
        """Check if any LLM API key is configured."""
        return bool(
            os.getenv("ANTHROPIC_API_KEY")
            or os.getenv("OPENAI_API_KEY")
            or os.getenv("OPENROUTER_API_KEY")
        )


def run_onboarding(console) -> bool:
    """First-run onboarding flow. Returns True if completed, False if skipped."""
    from rich.panel import Panel
    from rich.prompt import Prompt

    console.print()
    console.print(Panel.fit(
        "[bold cyan]Welcome to PRISM[/bold cyan]\n"
        "[dim]AI-Native Autonomous Materials Discovery[/dim]\n\n"
        "Let's get you set up. This takes about 30 seconds.",
        border_style="cyan",
        padding=(1, 3),
    ))
    console.print()

    # --- LLM Provider ---
    console.print("  [bold]Step 1:[/bold] Configure an LLM provider\n")
    console.print("  [dim]1[/dim]  Anthropic  [dim](Claude — recommended)[/dim]")
    console.print("  [dim]2[/dim]  OpenAI     [dim](GPT-4)[/dim]")
    console.print("  [dim]3[/dim]  OpenRouter  [dim](200+ models, single key)[/dim]")
    console.print("  [dim]s[/dim]  Skip       [dim](configure later with prism configure)[/dim]")
    console.print()

    choice = Prompt.ask("  Choice", choices=["1", "2", "3", "s"], default="1")

    env_lines = []
    provider_name = None

    if choice == "1":
        key = Prompt.ask("  Anthropic API key", password=True)
        if key.strip():
            env_lines.append(f"ANTHROPIC_API_KEY={key.strip()}")
            os.environ["ANTHROPIC_API_KEY"] = key.strip()
            provider_name = "Anthropic"
    elif choice == "2":
        key = Prompt.ask("  OpenAI API key", password=True)
        if key.strip():
            env_lines.append(f"OPENAI_API_KEY={key.strip()}")
            os.environ["OPENAI_API_KEY"] = key.strip()
            provider_name = "OpenAI"
    elif choice == "3":
        key = Prompt.ask("  OpenRouter API key", password=True)
        if key.strip():
            env_lines.append(f"OPENROUTER_API_KEY={key.strip()}")
            os.environ["OPENROUTER_API_KEY"] = key.strip()
            provider_name = "OpenRouter"

    if provider_name:
        console.print(f"  [green]{provider_name} configured.[/green]\n")
    elif choice != "s":
        console.print("  [dim]No key entered, skipping.[/dim]\n")

    # --- Materials Project API key (optional) ---
    console.print("  [bold]Step 2:[/bold] Materials Project API key [dim](optional — enriches search data)[/dim]\n")
    mp_key = Prompt.ask("  MP API key [dim](Enter to skip)[/dim]", default="", password=True)
    if mp_key.strip():
        env_lines.append(f"MATERIALS_PROJECT_API_KEY={mp_key.strip()}")
        os.environ["MATERIALS_PROJECT_API_KEY"] = mp_key.strip()
        console.print("  [green]Materials Project key configured.[/green]\n")
    else:
        console.print("  [dim]Skipped. You can add it later with: prism configure --mp-api-key YOUR_KEY[/dim]\n")

    # --- Write .env file ---
    if env_lines:
        env_path = _find_env_path()
        _write_env_keys(env_path, env_lines)
        console.print(f"  [dim]Keys saved to {env_path}[/dim]")

    # --- Save preferences ---
    prefs = UserPreferences.load()
    prefs.onboarding_complete = True
    prefs.save()

    console.print()
    console.print("  [green]Setup complete.[/green] Type your first query below.\n")
    return True


def _find_env_path() -> Path:
    """Find or create the .env file path."""
    # Prefer project-level .env
    from app.config.settings import get_env_path
    env_path = get_env_path()
    return env_path


def _write_env_keys(env_path: Path, lines: list[str]):
    """Append or update keys in the .env file."""
    existing = {}
    if env_path.exists():
        for line in env_path.read_text().splitlines():
            stripped = line.strip()
            if stripped and not stripped.startswith("#") and "=" in stripped:
                k, v = stripped.split("=", 1)
                existing[k.strip()] = v.strip()

    for line in lines:
        k, v = line.split("=", 1)
        existing[k.strip()] = v.strip()

    content_lines = ["# PRISM Environment Configuration"]
    for k, v in existing.items():
        content_lines.append(f"{k}={v}")

    env_path.parent.mkdir(parents=True, exist_ok=True)
    env_path.write_text("\n".join(content_lines) + "\n")
