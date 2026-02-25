"""PRISM CLI and TUI — all presentation code lives here.

Re-exports the Click ``cli`` group so that existing ``from app.cli import cli``
imports continue to work after cli.py was moved to cli/main.py.
"""

from app.cli.main import cli  # noqa: F401
