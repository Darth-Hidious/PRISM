"""The scientific-tool authoring contract, enforced instead of hoped for.

`Tool` has carried a well-designed contract for a while — `output_schema`,
EMMO/QUDT `units`, `examples`, and a `validate` gate whose docstring states the
real problem exactly: "Exit 0 is not enough (an unconverged SCF, a NaN tensor, a
max-steps hit all exit 0)."

Every field is optional, and measured across the loaded registry: 3/80 declare
an output schema, 3/80 declare units, 8/80 carry examples, 3/80 have a validity
gate. Two tools satisfy all four. An optional contract that 89% of tools ignore
is a document, not a standard.

These tests make it a standard the only way that survives contact with a large
existing tree: a RATCHET. Tools already meeting the contract must keep meeting
it, the compliant set may only grow, and — most importantly — a declaration must
be REAL rather than decorative. A tool that declares an example its own validity
gate rejects has documented a bug, not a contract.

Nothing here breaks the 71 tools that declare nothing today. It makes it
impossible to lose ground, and makes progress a number you can see.
"""

import math
import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))


@pytest.fixture(scope="module")
def tools():
    os.environ.setdefault("PRISM_ENABLE_MCP", "0")
    from app.plugins.bootstrap import build_full_registry

    registry, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
    return list(registry.list_tools())


def _declares(tool, field):
    return getattr(tool, field, None) is not None


def _compliant(tool):
    return all(
        _declares(tool, f)
        for f in ("output_schema", "units", "examples", "validate")
    )


# The ratchet. These tools satisfy the whole contract today and are the
# reference implementation for the rest. Removing one, or letting one lose a
# field, is a regression — not a refactor.
FULLY_COMPLIANT = {
    "lpbf_printability_map",
    "lpbf_kou_cracking_index",
}

# Tools that declare a validity gate. Same rule: this set may grow, never shrink.
HAS_VALIDITY_GATE = {
    "lpbf_printability_map",
    "lpbf_kou_cracking_index",
    "qe_parse_output",
}

VALID_VERDICTS = {"ok", "warn", "invalid"}


def test_the_reference_tools_still_satisfy_the_whole_contract(tools):
    by_name = {t.name: t for t in tools}
    for name in sorted(FULLY_COMPLIANT):
        tool = by_name.get(name)
        assert tool is not None, f"{name} vanished from the registry"
        missing = [
            f
            for f in ("output_schema", "units", "examples", "validate")
            if not _declares(tool, f)
        ]
        assert not missing, f"{name} lost {missing} — the reference must stay reference"


def test_the_compliant_set_may_only_grow(tools):
    actual = {t.name for t in tools if _compliant(t)}
    lost = FULLY_COMPLIANT - actual
    assert not lost, f"tools stopped satisfying the contract: {sorted(lost)}"
    gained = actual - FULLY_COMPLIANT
    if gained:
        pytest.fail(
            "Good news, and this test is how you record it: "
            f"{sorted(gained)} now satisfy the contract. "
            "Add them to FULLY_COMPLIANT so they can never silently regress."
        )


def test_every_validity_gate_returns_the_declared_vocabulary(tools):
    """A gate that returns anything else is not a gate — callers branch on it."""
    by_name = {t.name: t for t in tools}
    for name in sorted(HAS_VALIDITY_GATE):
        tool = by_name.get(name)
        assert tool is not None, f"{name} vanished from the registry"
        assert callable(tool.validate), f"{name}.validate is not callable"
        # Garbage in: the verdict must still be one of the three words, never a
        # crash and never an invented status.
        for garbage in ({}, {"status": "error"}, {"status": "ok"}):
            verdict = tool.validate(garbage)
            assert verdict in VALID_VERDICTS, (
                f"{name}.validate returned {verdict!r} for {garbage!r}; "
                f"callers branch on {sorted(VALID_VERDICTS)}"
            )


def test_a_declared_example_survives_its_own_validity_gate(tools):
    """The check that makes examples worth having.

    A tool whose own documented output is rejected by its own gate has
    published a bug as documentation. This is cheap to run and catches the
    example drifting away from the code that produces it.
    """
    for tool in tools:
        if not (_declares(tool, "examples") and _declares(tool, "validate")):
            continue
        for n, example in enumerate(tool.examples):
            output = example.get("output")
            if not isinstance(output, dict):
                continue
            verdict = tool.validate(output)
            assert verdict in VALID_VERDICTS, (
                f"{tool.name} example #{n}: gate returned {verdict!r}"
            )
            assert verdict != "invalid", (
                f"{tool.name} example #{n} is rejected by {tool.name}'s OWN "
                f"validity gate — the example and the gate disagree about what "
                f"a good result looks like"
            )


def test_declared_units_name_a_real_vocabulary(tools):
    """`units` exists to make a number checkable, which a bare string is not."""
    for tool in tools:
        if not _declares(tool, "units"):
            continue
        assert isinstance(tool.units, dict), f"{tool.name}.units must be a mapping"
        for field, unit in tool.units.items():
            assert isinstance(unit, str) and unit, (
                f"{tool.name}.units[{field!r}] is not a unit string"
            )
            assert unit.split(":", 1)[0] in {"QUDT", "EMMO"}, (
                f"{tool.name}.units[{field!r}] = {unit!r} — a unit tag must name "
                f"its vocabulary (QUDT: or EMMO:), or nothing downstream can "
                f"resolve it"
            )


def test_declared_examples_have_both_halves(tools):
    for tool in tools:
        if not _declares(tool, "examples"):
            continue
        assert isinstance(tool.examples, list) and tool.examples, (
            f"{tool.name}.examples is declared but empty"
        )
        for n, example in enumerate(tool.examples):
            assert isinstance(example, dict), f"{tool.name} example #{n} is not a dict"
            assert "input" in example, (
                f"{tool.name} example #{n} has no input — an example that does "
                f"not show the call cannot help arg-filling, which is why "
                f"examples exist"
            )


def test_an_output_schema_actually_promises_something(tools):
    """A schema with no required fields constrains nothing."""
    for tool in tools:
        if not _declares(tool, "output_schema"):
            continue
        schema = tool.output_schema
        assert isinstance(schema, dict), f"{tool.name}.output_schema must be a dict"
        assert schema.get("type"), f"{tool.name}.output_schema has no type"
        assert schema.get("properties"), (
            f"{tool.name}.output_schema declares no properties"
        )
        assert schema.get("required"), (
            f"{tool.name}.output_schema requires nothing, so it promises nothing"
        )


def test_a_declared_example_matches_its_own_input_schema(tools):
    """An example that could not be called is not an example."""
    jsonschema = pytest.importorskip("jsonschema")
    for tool in tools:
        if not (_declares(tool, "examples") and tool.input_schema):
            continue
        for n, example in enumerate(tool.examples):
            payload = example.get("input")
            if not isinstance(payload, dict):
                continue
            try:
                jsonschema.validate(payload, tool.input_schema)
            except jsonschema.ValidationError as error:
                pytest.fail(
                    f"{tool.name} example #{n} does not satisfy {tool.name}'s own "
                    f"input schema: {error.message}"
                )


def test_report_current_adoption(tools, capsys):
    """Not a gate — a number, printed, so progress is visible rather than felt."""
    total = len(tools)
    counts = {
        field: sum(1 for t in tools if _declares(t, field))
        for field in ("output_schema", "units", "examples", "validate")
    }
    full = sum(1 for t in tools if _compliant(t))
    with capsys.disabled():
        print(f"\n  scientific-tool contract adoption ({total} tools loaded)")
        for field, n in counts.items():
            print(f"    {field:<14} {n:3d}  {100 * n / total:5.1f}%")
        print(f"    {'ALL FOUR':<14} {full:3d}  {100 * full / total:5.1f}%")
    assert total > 0
