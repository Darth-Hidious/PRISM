"""mace_core — physics primitives wrapping MACE-MH-1 + ASE + Phonopy.

Pure functions. No I/O. No CLI. No MCP. No Hugging Face Hub upload.
The MCP layer (``mace_mcp``) wraps these for tool dispatch.

The functions live here so that:
  - Local in-process backends can import them directly.
  - HF Jobs payloads can ``from app.tools.simulation.mace.core import ...`` after a clone.
  - Tests can monkey-patch ``make_calc`` and exercise everything else
    deterministically.
"""

from importlib.metadata import PackageNotFoundError, version

try:
    __version__ = version("mace-mcp")
except PackageNotFoundError:
    __version__ = "0.0.0+dev"

__all__ = ["__version__"]
