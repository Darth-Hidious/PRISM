"""TUI configuration with sensible defaults."""
from dataclasses import dataclass


@dataclass
class TUIConfig:
    """User-overridable TUI settings."""
    truncation_lines: int = 6
    max_status_tasks: int = 5
    auto_scroll: bool = True
    image_preview: str = "system"  # "inline" | "system" | "none"
