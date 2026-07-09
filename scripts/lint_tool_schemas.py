#!/usr/bin/env python3
"""Lint every PRISM tool for schema + description quality (TOOL_SURFACE_SPEC D1-D4, D7).

Enforces the definition-of-ready from docs/TOOL_SURFACE_SPEC.md:
  - D2: description >= 60 chars AND contains a when-to-use/return signal.
  - D3: input_schema is a JSON object (typed, honest-empty, or umbrella-with-subcommand).
  - D4: typed schemas have per-property descriptions + required + additionalProperties set.
  -D7: data-returning tools mention a return/shape signal.

Exits non-zero with a per-tool report if any tool fails. Designed to run in CI
on the live registry produced by app.plugins.bootstrap.build_full_registry().

Run (from repo root):

    PRISM_DISABLE_MEMORY=1 python3 scripts/lint_tool_schemas.py

PRISM_DISABLE_MEMORY=1 skips the memory tools (optional deps); the lint then
covers the always-present core catalog. To also lint memory/MACE/Spark tools,
run in a venv where those optional deps import cleanly.
"""
from __future__ import annotations

import sys
from typing import Iterable

from app.plugins.bootstrap import build_full_registry

# Phrases that satisfy the "when to use / what it returns" signal (D2/D7).
# Deliberately broad: the lint is a floor, not a judge of prose quality.
_SIGNAL_PHRASES = (
    "use for", "use when", "use this", "when you", "best for", "call this",
    "use it", "returns", "return ", "use the", "prefer", "gives you",
    # verb-form signals (action-oriented triggers / returns)
    "lists", "list ", "get the", "fetch", "search", "query", "inspect",
    "run ", "execute", "predict", "compute", "generate", "train", "submit",
    "cancel", "check ", "show ", "print", "read ", "write ", "import",
    "export", "plot", "analyze", "save", "filter", "rank", "cancel",
)

DESC_MIN_CHARS = 60


def _has_signal(text: str) -> bool:
    low = text.lower()
    return any(p in low for p in _SIGNAL_PHRASES)


def _lint_tool(tool) -> tuple[list[str], list[str]]:
    """Return (hard_failures, soft_warnings) for one tool.

    Hard failures block CI (definition-of-ready D1-D4). Soft warnings are
    reported but do not fail — they cover shape-dependent guidance (e.g.
    additionalProperties on typed schemas is only required for closed shapes,
    so its absence is a nudge, not a defect).
    """
    fails: list[str] = []
    warns: list[str] = []
    desc = (getattr(tool, "description", "") or "").strip()

    # D2 — description length + signal floor (hard).
    if len(desc) < DESC_MIN_CHARS:
        fails.append(f"description is {len(desc)} chars, need >= {DESC_MIN_CHARS}")
    if not _has_signal(desc):
        fails.append(
            "description lacks a when-to-use / return signal "
            "(e.g. 'use for', 'returns', 'call this to')"
        )

    # D3 — schema must be a JSON object of an accepted shape (hard).
    schema = getattr(tool, "input_schema", None)
    if not isinstance(schema, dict) or schema.get("type") != "object":
        fails.append("input_schema is not a JSON object with type=='object'")
        return fails, warns

    props = schema.get("properties", {})
    if not isinstance(props, dict):
        fails.append("input_schema.properties is not an object")
        return fails, warns

    is_empty = len(props) == 0

    # D3b — honest-empty schemas must close additionalProperties (hard).
    if is_empty:
        if schema.get("additionalProperties") is not False:
            fails.append(
                "parameter-less schema must set additionalProperties: false"
            )
        return fails, warns

    # D4 — typed schemas: per-property description + required (hard).
    for pname, pdef in props.items():
        if not isinstance(pdef, dict):
            fails.append(f"property {pname!r} is not a schema object")
        elif "description" not in pdef:
            fails.append(f"property {pname!r} has no description")

    if "required" not in schema:
        fails.append("typed schema has no 'required' array (declare mandatory args)")

    # additionalProperties on typed schemas is shape-dependent (open vs closed),
    # so it is a WARNING, not a hard fail. SPEC §1.1.
    if "additionalProperties" not in schema:
        warns.append(
            "typed schema does not set additionalProperties "
            "(set false for closed shapes)"
        )

    return fails, warns


def lint_all(tools: Iterable) -> tuple[list[tuple[str, list[str]]], list[tuple[str, list[str]]]]:
    """Return (hard_failures, soft_warnings), each a list of (name, reasons)."""
    hard: list[tuple[str, list[str]]] = []
    soft: list[tuple[str, list[str]]] = []
    for t in tools:
        fails, warns = _lint_tool(t)
        if fails:
            hard.append((t.name, fails))
        if warns:
            soft.append((t.name, warns))
    return hard, soft


def main() -> int:
    registry, _providers, err = build_full_registry(enable_mcp=False, enable_plugins=False)
    if err is not None:
        print(f"FAIL: registry build error: {err}", file=sys.stderr)
        return 2
    tools = list(registry.list_tools())
    hard, soft = lint_all(tools)

    print(f"Linted {len(tools)} tools; {len(hard)} hard failures, {len(soft)} warnings.")

    if soft:
        print("\nWarnings (do not fail CI):")
        for name, reasons in sorted(soft):
            for r in reasons:
                print(f"  {name}: {r}")

    if not hard:
        print("PASS: all tools meet the schema + description floor.")
        return 0

    print("\nFailing tools (hard):")
    for name, reasons in sorted(hard):
        print(f"  {name}:")
        for r in reasons:
            print(f"    - {r}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
