"""Centralized configuration for PRISM."""

import os
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent.parent

def get_env_path() -> Path:
    """Return the path to the .env file."""
    project_env = PROJECT_ROOT / ".env"
    if project_env.exists():
        return project_env
    cwd_env = Path.cwd() / ".env"
    if cwd_env.exists():
        return cwd_env
    return project_env

MAX_FILTER_ATTEMPTS = int(os.getenv("PRISM_MAX_FILTER_ATTEMPTS", "3"))
MAX_RESULTS_DISPLAY = int(os.getenv("PRISM_MAX_RESULTS_DISPLAY", "10"))
MAX_INTERACTIVE_QUESTIONS = int(os.getenv("PRISM_MAX_INTERACTIVE_QUESTIONS", "3"))
MAX_RESULTS_PER_PROVIDER = int(os.getenv("PRISM_MAX_RESULTS_PER_PROVIDER", "1000"))
