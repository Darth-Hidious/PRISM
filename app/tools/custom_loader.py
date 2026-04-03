# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""Auto-discover custom tools from ~/.prism/tools/*.py.

Drop a Python file in ~/.prism/tools/ and it becomes a tool.
The file must define:
  - TOOL_NAME: str           — unique name
  - TOOL_DESCRIPTION: str    — what the tool does (shown to the LLM)
  - TOOL_SCHEMA: dict        — JSON Schema for inputs
  - def run(**kwargs) -> str  — the tool function

Optional:
  - REQUIRES_APPROVAL: bool  — default False

Example (~/.prism/tools/calculate_density.py):

    TOOL_NAME = "calculate_density"
    TOOL_DESCRIPTION = "Calculate material density from composition and crystal structure."
    TOOL_SCHEMA = {
        "type": "object",
        "properties": {
            "formula": {"type": "string", "description": "Chemical formula"},
            "volume": {"type": "number", "description": "Unit cell volume in A^3"},
        },
        "required": ["formula", "volume"],
    }

    def run(formula: str, volume: float) -> str:
        from pymatgen.core import Composition
        comp = Composition(formula)
        mass = comp.weight  # g/mol
        density = mass / (volume * 6.022e-1)  # g/cm^3
        return f"Density of {formula}: {density:.3f} g/cm^3"
"""

import importlib.util
import logging
import sys
from pathlib import Path
from typing import Optional

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)

CUSTOM_TOOLS_DIR = Path.home() / ".prism" / "tools"


def discover_custom_tools(
    registry: ToolRegistry,
    tools_dir: Optional[Path] = None,
) -> list[str]:
    """Scan a directory for custom tool .py files and register them.

    Returns list of loaded tool names.
    """
    search_dir = tools_dir or CUSTOM_TOOLS_DIR
    if not search_dir.is_dir():
        return []

    loaded = []
    for py_file in sorted(search_dir.glob("*.py")):
        if py_file.name.startswith("_"):
            continue

        module_name = f"prism_custom_tool_{py_file.stem}"
        try:
            spec = importlib.util.spec_from_file_location(module_name, py_file)
            if spec is None or spec.loader is None:
                continue
            module = importlib.util.module_from_spec(spec)
            sys.modules[module_name] = module
            spec.loader.exec_module(module)

            # Validate required attributes
            name = getattr(module, "TOOL_NAME", None)
            description = getattr(module, "TOOL_DESCRIPTION", None)
            schema = getattr(module, "TOOL_SCHEMA", None)
            run_fn = getattr(module, "run", None)

            if not all([name, description, schema, run_fn]):
                logger.warning(
                    "Skipping %s: missing TOOL_NAME, TOOL_DESCRIPTION, TOOL_SCHEMA, or run()",
                    py_file.name,
                )
                continue

            if not callable(run_fn):
                logger.warning("Skipping %s: run is not callable", py_file.name)
                continue

            requires_approval = getattr(module, "REQUIRES_APPROVAL", False)

            # Wrap run() to return string
            def make_wrapper(fn):
                def wrapper(**kwargs):
                    result = fn(**kwargs)
                    return str(result) if result is not None else ""
                return wrapper

            tool = Tool(
                name=name,
                description=description,
                input_schema=schema,
                func=make_wrapper(run_fn),
                requires_approval=requires_approval,
            )
            registry.register(tool)
            loaded.append(name)
            logger.info("Loaded custom tool: %s from %s", name, py_file.name)

        except Exception as e:
            logger.warning("Failed to load custom tool %s: %s", py_file.name, e)

    return loaded
