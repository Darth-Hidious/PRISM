"""Agent-facing MACE tool schemas must tell the model the truth.

Regression tests for the defect where ``app/tools/mace.py`` declared
``n_atoms`` twice with contradictory bounds (2..1000 in one fragment,
8..432 in another) and phase enums listing values the runtime rejects
(``b2`` / ``l12`` / ``sigma`` / ``amorphous``) while omitting one it
accepts (``c14_laves``). Depending on which tool the model called, it was
given a different contract for the same field.

The single source of truth is the runtime enforcement itself:

* the pydantic models in ``app.tools.simulation.mace.schemas`` — the
  synchronous gate every tool call passes through;
* the supercell builders in ``app.tools.simulation.mace.core.builders``
  — the job-execution gate (bcc/fcc/hcp build a fixed 100-atom cell, so
  the only ``n_atoms`` that survives end-to-end is 100).

These tests derive the enforced bounds from those modules and assert that
the declared JSON Schema fragments never permit anything the runtime
would reject, and that every surface declaring a shared field declares
the SAME contract for it. If a primitive ever genuinely needs a different
bound, it must declare its own fragment and these tests must be updated
deliberately, with the reason.
"""

from __future__ import annotations

import typing

import pytest

from app.tools import mace as mace_tools
from app.tools.base import ToolRegistry
from app.tools.simulation.mace import schemas as mace_schemas


# ---------------------------------------------------------------------------
# Helpers — derive the enforced truth from the runtime modules
# ---------------------------------------------------------------------------

def _registry() -> ToolRegistry:
    reg = ToolRegistry()
    mace_tools.create_mace_tools(reg)
    return reg


def _tool_fragments(field: str) -> dict[str, dict]:
    """tool name -> declared JSON-schema fragment for top-level ``field``."""
    out: dict[str, dict] = {}
    for tool in _registry().list_tools():
        props = tool.input_schema.get("properties", {})
        if field in props:
            out[tool.name] = props[field]
    return out


def _module_fragments() -> dict[str, dict]:
    """Every module-level dict in app.tools.mace that is a JSON-schema object.

    Catches contradictions hiding in fragments that are defined but never
    wired into a tool (the original defect: _STRUCTURE_REF_SCHEMA declared
    n_atoms 2..1000 while the live fragment declared 8..432).
    """
    out: dict[str, dict] = {}
    for attr in dir(mace_tools):
        obj = getattr(mace_tools, attr)
        if isinstance(obj, dict) and isinstance(obj.get("properties"), dict):
            out[attr] = obj
    return out


def _enforced_bounds(model: type, name: str) -> tuple[object, float | None, float | None]:
    """(default, lower, upper) as enforced by the pydantic field constraints."""
    fi = model.model_fields[name]
    lo: float | None = None
    hi: float | None = None
    for m in fi.metadata:
        for attr in ("ge", "gt"):
            if hasattr(m, attr):
                lo = getattr(m, attr)
        for attr in ("le", "lt"):
            if hasattr(m, attr):
                hi = getattr(m, attr)
    return fi.default, lo, hi


# ---------------------------------------------------------------------------
# The named regression test: one consistent contract per shared field
# ---------------------------------------------------------------------------

def test_shared_fragments_give_every_tool_one_consistent_contract() -> None:
    """No tool and no module-level fragment may contradict the shared contract.

    For every field shared across primitives (n_atoms, phase), all declared
    fragments — registered tools AND module-level dicts, used or unused —
    must be identical. This is the class of contradiction that let the model
    be told min 2 / max 1000 by one fragment and min 8 / max 432 by another.
    """
    for field in ("n_atoms", "phase"):
        declared: dict[str, dict] = {}
        for tool_name, frag in _tool_fragments(field).items():
            declared[f"tool:{tool_name}"] = frag
        for attr, frag in _module_fragments().items():
            if field in frag["properties"]:
                declared[f"module:{attr}"] = frag["properties"][field]

        assert declared, f"no surface declares {field!r}"
        first_name, first = next(iter(declared.items()))
        for name, frag in declared.items():
            assert frag == first, (
                f"{field} contract contradicts {first_name}: "
                f"{name} declares {frag!r}, {first_name} declares {first!r}"
            )


# ---------------------------------------------------------------------------
# Declared bounds must be runtime truth, not aspiration
# ---------------------------------------------------------------------------

def test_declared_n_atoms_values_survive_the_supercell_builder() -> None:
    """Every n_atoms the schema permits must actually build.

    The local backend and the hf_jobs payload both call ``build_supercell``,
    which raises ``ValueError: composition must sum to 100`` for anything
    else (builders.py). A declared range wider than that sells the model
    calls that pass validation, spend an approval, and then fail the job.
    """
    pytest.importorskip("ase")
    from app.tools.simulation.mace.core.builders import build_supercell

    fragments = _tool_fragments("n_atoms")
    assert fragments, "no tool declares n_atoms"
    for tool_name, frag in fragments.items():
        lo, hi = frag.get("minimum"), frag.get("maximum")
        assert lo is not None and hi is not None, (
            f"{tool_name}: n_atoms bounds must be declared"
        )
        for n in {int(lo), int(hi)}:
            build_supercell({"Fe": n}, "bcc")  # raises if n != 100


def test_declared_n_atoms_within_the_pydantic_gate() -> None:
    """Declared range must not exceed the pydantic validation surface."""
    _, lo, hi = _enforced_bounds(mace_schemas.StructureRef, "n_atoms")
    for tool_name, frag in _tool_fragments("n_atoms").items():
        assert frag.get("minimum") is not None and frag["minimum"] >= lo, (
            f"{tool_name}: declared n_atoms minimum below enforced {lo}"
        )
        assert frag.get("maximum") is not None and frag["maximum"] <= hi, (
            f"{tool_name}: declared n_atoms maximum above enforced {hi}"
        )


def test_phase_enum_is_exactly_the_enforced_literal() -> None:
    """Declared phase values must be exactly what pydantic accepts.

    Extras are rejected calls; omissions hide a valid option.
    """
    enforced = set(typing.get_args(mace_schemas.Phase))
    declared = _tool_fragments("phase")
    assert declared, "no tool declares phase"
    for tool_name, frag in declared.items():
        assert set(frag.get("enum", [])) == enforced, (
            f"{tool_name}: phase enum {sorted(frag.get('enum', []))} != "
            f"enforced {sorted(enforced)}"
        )


def test_relax_fmax_declared_never_permits_a_rejected_value() -> None:
    frag = _registry().get("mace_relax_structure").input_schema["properties"]["fmax_eV_per_A"]
    default, lo, hi = _enforced_bounds(mace_schemas.RelaxStructureInput, "fmax_eV_per_A")
    # Enforced lower bound is EXCLUSIVE (gt=0): the declared inclusive
    # minimum must stay strictly above it.
    assert frag.get("minimum") is not None and frag["minimum"] > lo
    assert frag.get("maximum") is not None and frag["maximum"] <= hi
    assert frag["default"] == default


def test_relax_max_steps_declared_matches_enforced() -> None:
    frag = _registry().get("mace_relax_structure").input_schema["properties"]["max_steps"]
    default, lo, hi = _enforced_bounds(mace_schemas.RelaxStructureInput, "max_steps")
    assert frag.get("minimum") is not None and frag["minimum"] >= lo, (
        f"max_steps minimum below enforced {lo}"
    )
    assert frag.get("maximum") is not None and frag["maximum"] <= hi, (
        f"max_steps maximum above enforced {hi}"
    )
    assert frag["default"] == default, "declared default is not what runs when omitted"


def test_primitive_options_head_and_timeout_match_enforced() -> None:
    enforced_heads = set(typing.get_args(mace_schemas.Head))
    _, t_lo, t_hi = _enforced_bounds(mace_schemas.PrimitiveOptions, "timeout_seconds")
    checked = 0
    for tool in _registry().list_tools():
        opts = tool.input_schema.get("properties", {}).get("options")
        if not opts:
            continue
        checked += 1
        head = opts["properties"]["head"]
        assert set(head.get("enum", [])) == enforced_heads, (
            f"{tool.name}: head enum must list exactly the MACE heads that exist"
        )
        t = opts["properties"]["timeout_seconds"]
        assert t.get("minimum") is not None and t["minimum"] >= t_lo
        assert t.get("maximum") is not None and t["maximum"] <= t_hi
    assert checked >= 5, "all five primitives must declare options"


def test_phonon_temperatures_declared_item_bounds() -> None:
    frag = _registry().get("mace_phonon_harmonic").input_schema["properties"]["temperatures_K"]
    md = mace_schemas.PhononHarmonicInput.model_fields["temperatures_K"].metadata
    min_len = next(m.min_length for m in md if hasattr(m, "min_length"))
    max_len = next(m.max_length for m in md if hasattr(m, "max_length"))
    assert frag.get("minItems") is not None and frag["minItems"] >= min_len, (
        "temperatures_K minItems missing or below the enforced minimum"
    )
    assert frag.get("maxItems") is not None and frag["maxItems"] <= max_len, (
        "temperatures_K maxItems missing or above the enforced maximum "
        "(an over-long list passes the declared schema and fails validation)"
    )
