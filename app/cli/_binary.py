"""Compiled Ink TUI binary discovery."""
import os
from pathlib import Path


def _bin_dir() -> Path:
    """Package-bundled binary location."""
    return Path(__file__).parent.parent / "_bin"


def _user_bin_dir() -> Path:
    """User-installed binary override location."""
    return Path.home() / ".prism" / "bin"


def tui_binary_path() -> Path | None:
    """Find the compiled TUI binary. Returns None if not found."""
    name = "prism-tui"

    # Check user override first (takes priority)
    candidate = _user_bin_dir() / name
    if candidate.exists() and os.access(candidate, os.X_OK):
        return candidate

    # Check package-bundled
    candidate = _bin_dir() / name
    if candidate.exists() and os.access(candidate, os.X_OK):
        return candidate

    return None


def has_tui_binary() -> bool:
    """Check if a compiled TUI binary is available."""
    return tui_binary_path() is not None
