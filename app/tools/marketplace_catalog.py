# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""Loader and dependency scanner for the marketplace tool catalog.

``marketplace_catalog.json`` next to this file is the single source of truth
for which PRISM tools are published to the MARC27 marketplace and what they
require. The Rust client (`crates/client/src/marketplace.rs`) reads the same
file at compile time and ships each entry's payload verbatim, so there is no
second schema to keep in step.

Nothing here talks to the network — publishing is the CLI's job. This module
exists so ``tests/test_marketplace_catalog.py`` can hold the catalog to
account against the live tool registry and against the imports the tools
actually perform.
"""
from __future__ import annotations

import ast
import json
import sys
from functools import lru_cache
from pathlib import Path
from typing import Any, Iterator

CATALOG_PATH = Path(__file__).with_name("marketplace_catalog.json")
_REPO_ROOT = Path(__file__).resolve().parents[2]


@lru_cache(maxsize=1)
def load() -> dict[str, Any]:
    """Parsed catalog. Cached — the file is static data."""
    return json.loads(CATALOG_PATH.read_text())


def entries() -> list[dict[str, Any]]:
    """Marketplace resource entries, in publish order."""
    return load()["entries"]


def bundled() -> dict[str, str]:
    """tool name -> why it stays bundled instead of being published."""
    return load()["bundled"]


def published_tools() -> dict[str, dict[str, Any]]:
    """tool name -> the entry that publishes it."""
    return {t["name"]: entry for entry in entries() for t in entry["tools"]}


# ---------------------------------------------------------------------------
# Dependency scanning
# ---------------------------------------------------------------------------

def scan_third_party(modules: list[str]) -> dict[str, set[str]]:
    """Third-party import name -> the `app.*` modules that import it.

    Walks the transitive closure of `app.*` imports starting at `modules`,
    so a tool that reaches a dependency two hops away is still caught. Any
    import that is neither stdlib nor `app.*` is reported.
    """
    stdlib = sys.stdlib_module_names
    seen: set[str] = set()
    found: dict[str, set[str]] = {}
    pending = list(modules)

    while pending:
        module = pending.pop()
        if module in seen:
            continue
        seen.add(module)
        path = _module_path(module)
        if path is None:
            continue
        for imported, is_local in _imports_of(path):
            if is_local:
                pending.append(imported)
            elif imported.split(".")[0] not in stdlib:
                found.setdefault(imported.split(".")[0], set()).add(module)
    return found


def undeclared_imports(entry: dict[str, Any]) -> dict[str, set[str]]:
    """Imports an entry's tools perform that the entry does not account for.

    Empty means the entry tells the truth: every third-party import is
    either in a default install, in one of the entry's declared extras, or
    on the explicit unmanaged-optional list.
    """
    from app.tools._extras import CORE_MODULES, EXTRA_MODULES, UNMANAGED_OPTIONAL

    allowed = set(CORE_MODULES) | set(UNMANAGED_OPTIONAL)
    for extra in entry["requires_extras"] + entry.get("optional_extras", []):
        allowed |= set(EXTRA_MODULES[extra])

    return {
        name: sources
        for name, sources in scan_third_party(entry["modules"]).items()
        if name not in allowed
    }


def _module_path(module: str) -> Path | None:
    base = _REPO_ROOT / module.replace(".", "/")
    for candidate in (base.with_suffix(".py"), base / "__init__.py"):
        if candidate.is_file():
            return candidate
    return None


def _imports_of(path: Path) -> Iterator[tuple[str, bool]]:
    """Yield (module name, is_local) for every import in `path`."""
    tree = ast.parse(path.read_text(), filename=str(path))
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                yield alias.name, alias.name.startswith("app.")
        elif isinstance(node, ast.ImportFrom) and node.module and not node.level:
            if node.module.startswith("app"):
                yield node.module, True
                # `from app.tools.x import y` may name a submodule.
                for alias in node.names:
                    yield f"{node.module}.{alias.name}", True
            else:
                yield node.module, False
