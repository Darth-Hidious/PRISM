"""Stdlib-logging shim — preserves the `get_logger` import surface that the
merged mace subpackage uses internally.

The original mace-mcp had its own logging_cfg.py that set up stderr-only,
JSON-line-friendly handlers because it was an MCP server speaking on stdout.
PRISM doesn't need that — it has central logging — so this shim just hands
back a stdlib logger and lets PRISM's own logging configuration drive
formatting / handlers / levels.
"""

from __future__ import annotations

import logging
from typing import Optional


def get_logger(name: Optional[str] = None) -> logging.Logger:
    """Return a stdlib logger.

    Kept signature-compatible with mace-mcp's original `get_logger(name)`
    so internal call sites in `runner.py`, `provenance.py`, `hf_jobs.py`,
    etc. don't have to change.
    """
    return logging.getLogger(name or "app.tools.simulation.mace")
