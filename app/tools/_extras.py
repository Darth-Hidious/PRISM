# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""One table of PRISM's optional dependency extras, and one missing-dep shape.

`app/tools/simulation/mace_bridge.py` set the pattern when the MACE tools
landed: a tool whose dependency is absent returns

    {"error": ..., "install_hint": "pip install 'prism-platform[mace]'"}

instead of raising. This module makes that the *only* pattern — the CALPHAD,
pyiron, MACE, ASE and ML gates all build their error dict here — and gives
the marketplace catalog (``app/tools/marketplace_catalog.json``) the same
table to declare requirements from. A published entry therefore cannot
promise an extra the runtime never asks for, and a tool cannot quietly grow
a dependency its entry does not declare: ``tests/test_marketplace_catalog.py``
checks both directions against pyproject.toml and against the real imports.

Distribution names live in pyproject.toml; this module maps them to the
*import* names tool code actually writes.
"""
from __future__ import annotations

import importlib
from typing import Any

PACKAGE = "prism-platform"

#: extra name -> every top-level import name that extra makes available.
#: Mirrors ``[project.optional-dependencies]`` in pyproject.toml.
EXTRA_MODULES: dict[str, tuple[str, ...]] = {
    "ml": (
        "sklearn", "xgboost", "lightgbm", "optuna", "pymatgen", "matminer",
        "matgl", "matplotlib", "pyarrow", "joblib", "robocrystallographer",
    ),
    "simulation": ("pyiron_base", "pyiron_atomistics"),
    "qe": ("ase", "pymatgen"),
    "lpbf": ("numpy", "scipy"),
    "calphad": ("pycalphad", "scheil"),
    "cluster-expansion": ("icet", "mchammer", "trainstation"),
    "precipitation": ("kawin",),
    "polymer": ("rdkit",),
    "mace": ("mace", "torch", "ase", "numpy", "huggingface_hub", "phonopy"),
    "data": ("datasets",),
    "reports": ("markdown", "weasyprint"),
}

#: extra name -> the pip distribution names that extra installs, exactly as
#: pyproject.toml lists them (version pins deliberately live only there).
#: `prism-platform` is not published to any index as of 2026-07-27 —
#: `pip install 'prism-platform[calphad]'` 404s — so the hint a user is given
#: names these distributions directly, which always works.
EXTRA_DISTRIBUTIONS: dict[str, tuple[str, ...]] = {
    "ml": (
        "scikit-learn", "xgboost", "lightgbm", "optuna", "pymatgen",
        "matminer", "matgl", "matplotlib", "pyarrow", "joblib",
        "robocrystallographer",
    ),
    "simulation": ("pyiron-base", "pyiron-atomistics"),
    "qe": ("ase", "pymatgen"),
    "lpbf": ("numpy", "scipy"),
    "calphad": ("pycalphad", "scheil"),
    "cluster-expansion": ("icet", "trainstation"),
    "precipitation": ("kawin",),
    "polymer": ("rdkit",),
    "mace": ("mace-torch", "torch", "ase", "numpy", "huggingface-hub", "phonopy"),
    "data": ("datasets",),
    "reports": ("markdown", "weasyprint"),
}

#: The subset of each extra that has to import for the extra to count as
#: present. Kept narrow on purpose: `[ml]` is usable without matgl (that
#: only gates pre-trained GNN structure prediction, which reports its own
#: hint), so requiring it here would refuse a working install.
GATE_IMPORTS: dict[str, tuple[str, ...]] = {
    "ml": ("sklearn", "pymatgen", "matminer"),
    "simulation": ("pyiron_atomistics",),
    "qe": ("ase", "pymatgen"),
    "lpbf": ("numpy", "scipy"),
    # Scheil is needed only by scheil_solidification; the core CALPHAD tools
    # remain usable when pycalphad is installed without that optional helper.
    "calphad": ("pycalphad",),
    "cluster-expansion": ("icet",),
    "precipitation": ("kawin",),
    "polymer": ("rdkit",),
    "mace": ("mace", "ase"),
    "data": ("datasets",),
    "reports": ("markdown",),
}

#: Top-level import names a *default* `pip install prism-platform` provides.
#: The declared half comes from ``[project] dependencies``; `numpy` and
#: `pydantic` are hard transitives of pandas/openai/fastmcp/optimade that
#: PRISM code imports directly and that no extra owns.
CORE_MODULES: frozenset[str] = frozenset({
    "rich", "yaml", "optimade", "sqlalchemy", "dotenv", "openai", "google",
    "anthropic", "pandas", "tenacity", "requests", "mp_api", "matplotlib",
    "joblib", "fastmcp", "httpx", "firecrawl", "ddgs", "duckduckgo_search",
    "bs4", "ulid", "sympy",
    "numpy", "pydantic",
})

#: Third-party imports reachable from tool code that belong to no extra and
#: are not declared anywhere. Each one is guarded at its import site and the
#: feature degrades rather than failing. Listed explicitly so the drift test
#: stays a real check instead of a wildcard.
UNMANAGED_OPTIONAL: frozenset[str] = frozenset({
    # app/tools/memory/embedder.py — semantic artifact recall. Optional:
    # the memory subsystem logs a warning and disables itself when absent.
    "sentence_transformers",
    # app/tools/spark.py — registered only if _check_spark_available().
    "pyspark",
    # app/tools/data_collectors/eastern_literature_collector.py — hardened
    # XML parsing. Guarded with an explicit fall back to stdlib ElementTree;
    # the module's own comment records that it is not a declared dependency.
    "defusedxml",
})

#: pyproject distribution name -> top-level import name, for the names where
#: they differ. Used by the test that keeps the tables above honest.
DIST_TO_IMPORT: dict[str, str] = {
    "PyYAML": "yaml",
    "python-dotenv": "dotenv",
    "google-cloud-aiplatform": "google",
    "mp-api": "mp_api",
    "firecrawl-py": "firecrawl",
    "duckduckgo-search": "duckduckgo_search",
    "beautifulsoup4": "bs4",
    "python-ulid": "ulid",
    "scikit-learn": "sklearn",
    "pyiron-base": "pyiron_base",
    "pyiron-atomistics": "pyiron_atomistics",
    "mace-torch": "mace",
    "prism-platform": "app",
}


def install_command(extra: str) -> str:
    """The command that works today: install the distributions directly.

    Not the `prism-platform[extra]` form — `prism-platform` is not on PyPI
    (verified 2026-07-27, HTTP 404), so that form only resolves for someone
    who already has the wheel or a source checkout. Version floors come from
    pyproject.toml via [`extra_command`]; naming the distributions plainly is
    the hint that never sends a user to a 404.
    """
    return "pip install " + " ".join(EXTRA_DISTRIBUTIONS[extra])


def extra_command(extra: str) -> str:
    """The pyproject extra form, for anyone installing prism-platform itself.

    Resolves from a source checkout (`pip install -e '.[calphad]'`) or from
    the wheel attached to the GitHub release; it does NOT resolve from PyPI.
    """
    return f"pip install '{PACKAGE}[{extra}]'"


def missing_imports(extra: str) -> list[str]:
    """Gate imports of `extra` that are not importable right now."""
    return [name for name in GATE_IMPORTS[extra] if not _importable(name)]


def extra_available(extra: str) -> bool:
    return not missing_imports(extra)


def missing_extra_error(extra: str, message: str, **fields: Any) -> dict[str, Any]:
    """The one missing-dependency shape. Never raises; never lies.

    `message` says what is missing in domain terms; `install_hint` says what
    to type. Extra keys (e.g. MACE's `rationale`) pass through.
    """
    return {
        "error": message,
        "install_hint": install_command(extra),
        "install_extra_hint": extra_command(extra),
        "provision_command": f"prism provision extra {extra}",
        "requires_extra": extra,
        **fields,
    }


def _importable(name: str) -> bool:
    try:
        importlib.import_module(name)
        return True
    except ImportError:
        return False
