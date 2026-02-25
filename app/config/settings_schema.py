"""Unified settings system for PRISM.

Two-tier hierarchy (like Claude Code):
  1. Global:  ~/.prism/settings.json   (user-level defaults)
  2. Project: .prism/settings.json     (per-project overrides, can be checked into git)

Merge order: defaults < global < project < environment variables.
"""

import dataclasses
import json
import os
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional


PRISM_DIR = Path.home() / ".prism"
GLOBAL_SETTINGS_PATH = PRISM_DIR / "settings.json"


def _find_project_settings() -> Optional[Path]:
    """Find .prism/settings.json in the current or ancestor directories."""
    cwd = Path.cwd()
    for parent in [cwd, *cwd.parents]:
        candidate = parent / ".prism" / "settings.json"
        if candidate.exists():
            return candidate
        # Stop at git root or home
        if (parent / ".git").exists() or parent == Path.home():
            break
    return None


# ---------- Schema ----------


@dataclass
class AgentSettings:
    """Agent behavior configuration."""
    model: str = ""                         # e.g. "claude-sonnet-4-20250514"
    provider: str = ""                      # anthropic, openai, openrouter, marc27
    max_iterations: int = 30                # TAOR loop limit
    auto_approve: bool = False              # auto-approve tool calls
    temperature: float = 0.0                # LLM temperature
    max_tokens: int = 0                     # 0 = use model default
    system_prompt_file: str = ""            # path to custom system prompt


@dataclass
class SearchSettings:
    """Search engine configuration."""
    default_providers: List[str] = field(default_factory=lambda: ["optimade"])
    max_results_per_source: int = 100
    cache_ttl_hours: int = 24
    timeout_seconds: int = 30
    retry_attempts: int = 3


@dataclass
class OutputSettings:
    """Output and display configuration."""
    format: str = "csv"                     # csv, parquet, both
    directory: str = "output"
    report_format: str = "markdown"         # markdown, pdf
    verbose: bool = False
    quiet: bool = False


@dataclass
class ComputeSettings:
    """Compute budget configuration."""
    budget: str = "local"                   # local, hpc
    hpc_queue: str = "default"
    hpc_cores: int = 4


@dataclass
class MLSettings:
    """Machine learning defaults."""
    algorithm: str = "random_forest"        # random_forest, gradient_boosting, xgboost, etc.
    feature_backend: str = ""               # matminer, builtin, or empty for auto


@dataclass
class UpdateSettings:
    """Update check configuration."""
    check_on_startup: bool = True
    cache_ttl_hours: int = 24
    channel: str = "stable"                 # stable, beta


@dataclass
class PermissionSettings:
    """Tool permission configuration."""
    require_approval: List[str] = field(default_factory=lambda: [
        "execute_python", "write_file", "submit_lab_job",
    ])
    deny: List[str] = field(default_factory=list)


@dataclass
class PrismSettings:
    """Root settings object — the complete settings.json schema."""
    agent: AgentSettings = field(default_factory=AgentSettings)
    search: SearchSettings = field(default_factory=SearchSettings)
    output: OutputSettings = field(default_factory=OutputSettings)
    compute: ComputeSettings = field(default_factory=ComputeSettings)
    ml: MLSettings = field(default_factory=MLSettings)
    updates: UpdateSettings = field(default_factory=UpdateSettings)
    permissions: PermissionSettings = field(default_factory=PermissionSettings)


# ---------- Load / Merge ----------


# Map of section name -> dataclass type for safe reconstruction
_SECTION_TYPES: Dict[str, type] = {
    "agent": AgentSettings,
    "search": SearchSettings,
    "output": OutputSettings,
    "compute": ComputeSettings,
    "ml": MLSettings,
    "updates": UpdateSettings,
    "permissions": PermissionSettings,
}


def _dataclass_from_dict(cls, data: dict):
    """Recursively construct a dataclass from a dict, ignoring unknown keys."""
    if not dataclasses.is_dataclass(cls):
        return data
    valid_fields = {f.name for f in dataclasses.fields(cls)}
    filtered = {}
    for key, value in data.items():
        if key not in valid_fields:
            continue
        # Check if this key maps to a nested dataclass section
        if key in _SECTION_TYPES and isinstance(value, dict):
            filtered[key] = _dataclass_from_dict(_SECTION_TYPES[key], value)
        else:
            filtered[key] = value
    return cls(**filtered)


def _deep_merge(base: dict, override: dict) -> dict:
    """Deep-merge two dicts. override wins for non-dict values."""
    result = dict(base)
    for key, value in override.items():
        if key in result and isinstance(result[key], dict) and isinstance(value, dict):
            result[key] = _deep_merge(result[key], value)
        else:
            result[key] = value
    return result


def _read_json(path: Path) -> dict:
    """Read a JSON file, returning {} on any error."""
    try:
        if path.exists():
            return json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        pass
    return {}


# Env vars that exist for other purposes (not settings overrides)
_RESERVED_ENV_VARS = frozenset({
    "PRISM_LABS_API_KEY", "PRISM_DEFAULT_MODEL",
    "PRISM_MAX_FILTER_ATTEMPTS", "PRISM_MAX_RESULTS_DISPLAY",
    "PRISM_MAX_INTERACTIVE_QUESTIONS", "PRISM_MAX_RESULTS_PER_PROVIDER",
})


def _apply_env_overrides(data: dict) -> dict:
    """Apply environment variable overrides to settings dict.

    Env vars follow the pattern: PRISM_<SECTION>_<KEY>=value
    e.g. PRISM_AGENT_MODEL=gpt-4o -> data["agent"]["model"] = "gpt-4o"
    """
    prefix = "PRISM_"
    for key, value in os.environ.items():
        if not key.startswith(prefix) or key in _RESERVED_ENV_VARS:
            continue
        parts = key[len(prefix):].lower().split("_", 1)
        if len(parts) == 2:
            section, field_name = parts
            if section in data and isinstance(data[section], dict):
                # Type coercion based on existing default type
                existing = data[section].get(field_name)
                if isinstance(existing, bool):
                    data[section][field_name] = value.lower() in ("true", "1", "yes")
                elif isinstance(existing, int):
                    try:
                        data[section][field_name] = int(value)
                    except ValueError:
                        pass
                elif isinstance(existing, float):
                    try:
                        data[section][field_name] = float(value)
                    except ValueError:
                        pass
                else:
                    data[section][field_name] = value

    # Legacy env var support
    model = os.getenv("PRISM_DEFAULT_MODEL") or os.getenv("PRISM_MODEL") or os.getenv("LLM_MODEL")
    if model:
        data.setdefault("agent", {})["model"] = model

    return data


def load_settings() -> PrismSettings:
    """Load and merge settings from all sources.

    Merge order: defaults < global (~/.prism/settings.json) < project (.prism/settings.json) < env vars.
    """
    # Start with defaults as dict
    defaults = asdict(PrismSettings())

    # Load global
    global_data = _read_json(GLOBAL_SETTINGS_PATH)

    # Load project
    project_path = _find_project_settings()
    project_data = _read_json(project_path) if project_path else {}

    # Merge: defaults < global < project
    merged = _deep_merge(defaults, global_data)
    merged = _deep_merge(merged, project_data)

    # Apply env var overrides
    merged = _apply_env_overrides(merged)

    return _dataclass_from_dict(PrismSettings, merged)


def save_global_settings(settings: PrismSettings) -> Path:
    """Save settings to ~/.prism/settings.json."""
    PRISM_DIR.mkdir(parents=True, exist_ok=True)
    GLOBAL_SETTINGS_PATH.write_text(json.dumps(asdict(settings), indent=2) + "\n")
    return GLOBAL_SETTINGS_PATH


def save_project_settings(settings: PrismSettings, project_dir: Optional[Path] = None) -> Path:
    """Save settings to .prism/settings.json in the project directory."""
    base = project_dir or Path.cwd()
    settings_dir = base / ".prism"
    settings_dir.mkdir(parents=True, exist_ok=True)
    path = settings_dir / "settings.json"
    path.write_text(json.dumps(asdict(settings), indent=2) + "\n")
    return path


def get_settings_paths() -> Dict[str, Optional[Path]]:
    """Return paths to all settings files (for display)."""
    project = _find_project_settings()
    return {
        "global": GLOBAL_SETTINGS_PATH if GLOBAL_SETTINGS_PATH.exists() else None,
        "project": project,
    }


# ---------- Singleton cache ----------

_cached: Optional[PrismSettings] = None


def get_settings() -> PrismSettings:
    """Get cached settings (loads once per process)."""
    global _cached
    if _cached is None:
        _cached = load_settings()
    return _cached


def reload_settings() -> PrismSettings:
    """Force reload settings from disk."""
    global _cached
    _cached = load_settings()
    return _cached
