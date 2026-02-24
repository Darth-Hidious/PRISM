"""User preferences for PRISM workflows."""

import json
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import List, Optional


PRISM_DIR = Path.home() / ".prism"
PREFERENCES_PATH = PRISM_DIR / "preferences.json"


@dataclass
class UserPreferences:
    """Persistent user preferences for skill execution."""

    output_format: str = "parquet"  # csv, parquet, both
    output_dir: str = "output"
    default_providers: List[str] = field(default_factory=lambda: ["optimade"])
    max_results_per_source: int = 100
    default_algorithm: str = "random_forest"
    report_format: str = "markdown"  # markdown, pdf
    compute_budget: str = "local"  # local, hpc
    hpc_queue: str = "default"
    hpc_cores: int = 4
    check_updates: bool = True

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
