"""Compiled Ink TUI binary discovery."""
import os
from pathlib import Path


def _bin_dir() -> Path:
    """Package-bundled binary location."""
    return Path(__file__).parent.parent / "_bin"


def _user_bin_dir() -> Path:
    """User-installed binary override location."""
    return Path.home() / ".prism" / "bin"


def _repo_target_dir() -> Path:
    """Cargo development build output location."""
    return Path(__file__).parent.parent.parent / "target" / "debug"


def _frontend_dist_dir() -> Path:
    """Frontend development build output location."""
    return Path(__file__).parent.parent.parent / "frontend" / "dist"


def _platform_binary_name(base: str) -> str:
    return f"{base}.exe" if os.name == "nt" else base


def tui_binary_path() -> Path | None:
    """Find the compiled TUI binary. Returns None if not found."""
    names = [_platform_binary_name("prism-tui")]
    if os.name != "nt":
        names.extend(
            [
                "prism-tui-darwin-arm64",
                "prism-tui-darwin-x64",
                "prism-tui-linux-arm64",
                "prism-tui-linux-x64",
            ]
        )

    for directory in (_frontend_dist_dir(), _user_bin_dir(), _bin_dir()):
        for name in names:
            candidate = directory / name
            if candidate.exists() and os.access(candidate, os.X_OK):
                return candidate

    return None


def has_tui_binary() -> bool:
    """Check if a compiled TUI binary is available."""
    return tui_binary_path() is not None


def rust_cli_binary_path() -> Path | None:
    """Find the Rust CLI binary if available."""
    for base in ("prism", "prism-cli"):
        name = _platform_binary_name(base)
        for candidate in (
            _user_bin_dir() / name,
            _bin_dir() / name,
            _repo_target_dir() / name,
        ):
            if candidate.exists() and os.access(candidate, os.X_OK):
                return candidate
    return None


def has_rust_cli_binary() -> bool:
    """Check if the Rust CLI binary is available."""
    return rust_cli_binary_path() is not None
