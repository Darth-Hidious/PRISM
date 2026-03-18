"""Bootstrap entrypoint that prefers the Rust CLI when available."""

from __future__ import annotations

import os
import sys
from pathlib import Path

from app.cli._binary import rust_cli_binary_path

_DISABLE_RUST_BOOTSTRAP_ENV = "PRISM_DISABLE_RUST_BOOTSTRAP"


def _project_root() -> Path:
    return Path(__file__).parent.parent.parent


def _exec_python_cli() -> None:
    from app.cli.main import cli

    cli()


def main() -> None:
    """Launch Rust CLI if present, else fall back to Python CLI."""
    if os.getenv(_DISABLE_RUST_BOOTSTRAP_ENV) == "1":
        _exec_python_cli()
        return

    rust_binary = rust_cli_binary_path()
    if rust_binary:
        env = os.environ.copy()
        env[_DISABLE_RUST_BOOTSTRAP_ENV] = "1"
        python_bin = env.get("PYTHON_SYS_EXECUTABLE") or sys.executable
        os.execvpe(
            str(rust_binary),
            [
                str(rust_binary),
                "--python",
                python_bin,
                "--project-root",
                str(_project_root()),
                *sys.argv[1:],
            ],
            env,
        )
        return

    _exec_python_cli()
