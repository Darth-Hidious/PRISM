"""Informed-autonomy elicitation gate — proof the mechanism actually gates.

These tests are the success definition for build (b): a simulation tool
cannot return numbers until an informed human confirmed a matching spec,
and heavy compute can never silently target the dev CPU.
"""

from __future__ import annotations

import pytest

from app.tools.base import Tool, ToolRegistry
from app.tools.elicitation import (
    ResearchSpec,
    create_elicitation_tools,
    extract_element_scope,
    get_ledger,
)


@pytest.fixture(autouse=True)
def _fresh_ledger():
    get_ledger().reset()
    yield
    get_ledger().reset()


def _sim_tool(executed: list) -> Tool:
    return Tool(
        name="fake_sim",
        description="x",
        input_schema={"type": "object"},
        func=lambda **kw: executed.append(kw) or {"K_VRH_GPa": 137.0},
        requires_elicitation=True,
        record_artifacts=False,
    )


def _propose_confirm(system, target="hf-jobs"):
    reg = ToolRegistry()
    create_elicitation_tools(reg)
    reg.get("propose_research_spec").func(
        system=system,
        structure="FCC SQS",
        dataset="none",
        params={"fmax": 0.05},
        compute_envelope={"target": target},
        elicited_from="the materials lead who specced the chamber",
    )
    return reg.get("confirm_research_spec").func()


def test_gated_tool_refuses_without_spec():
    executed: list = []
    tool = _sim_tool(executed)
    out = tool.execute(base_elements=["Cu", "Ni", "Si"])
    assert out["elicitation_required"] is True
    assert out["element_scope"] == ["Cu", "Ni", "Si"]
    assert "structure" in out["missing"]
    assert executed == []  # body NEVER ran → no fabricated 137.0


def test_propose_then_confirm_unlocks_matching_scope():
    executed: list = []
    tool = _sim_tool(executed)
    res = _propose_confirm(["Cu", "Ni", "Si"])
    assert res["confirmed"] is True

    out = tool.execute(composition={"atoms": {"Cu": 97, "Ni": 2, "Si": 1}})
    assert out == {"K_VRH_GPa": 137.0}
    assert len(executed) == 1


def test_subset_call_is_covered_superset_call_is_not():
    executed: list = []
    tool = _sim_tool(executed)
    _propose_confirm(["Cu", "Ni", "Si"])

    # subset → allowed
    assert tool.execute(base_elements=["Cu", "Ni"]) == {"K_VRH_GPa": 137.0}
    # element outside the confirmed system → blocked again
    blocked = tool.execute(base_elements=["Cu", "Ni", "Fe"])
    assert blocked["elicitation_required"] is True


def test_dev_cpu_target_rejected_at_confirm_time():
    res = _propose_confirm(["Cu"], target="local")
    assert res["confirmed"] is False
    assert "dev CPU" in res["error"]


def test_dev_cpu_spec_blocked_at_gate_even_if_present():
    executed: list = []
    tool = _sim_tool(executed)
    # Inject a confirmed dev-CPU spec directly (bypass confirm guard) to
    # prove the gate ALSO rejects, defence-in-depth.
    ledger = get_ledger()
    ledger._confirmed.append(  # noqa: SLF001 - deliberate defence-in-depth test
        ResearchSpec(
            system=["Cu"],
            structure="s",
            dataset="none",
            params={},
            compute_envelope={"target": "cpu"},
            confirmed_by="x",
        )
    )
    out = tool.execute(base_elements=["Cu"])
    assert out["elicitation_required"] is True
    assert out["blocked_reason"] == "compute_envelope_targets_dev_cpu"
    assert executed == []


def test_non_compositional_tool_covered_by_any_confirmed_spec():
    executed: list = []
    tool = _sim_tool(executed)
    _propose_confirm(["Cu", "Ni", "Si"])
    # no element kwargs at all → empty scope → covered
    assert tool.execute(temperature=1500) == {"K_VRH_GPa": 137.0}


def test_ungated_tool_runs_normally():
    executed: list = []
    tool = Tool(
        name="cheap",
        description="x",
        input_schema={"type": "object"},
        func=lambda **kw: executed.append(kw) or {"ok": True},
        record_artifacts=False,
    )
    assert tool.execute(a=1) == {"ok": True}
    assert len(executed) == 1


def test_elicitation_tools_are_not_themselves_gated():
    reg = ToolRegistry()
    create_elicitation_tools(reg)
    assert reg.get("propose_research_spec").requires_elicitation is False
    assert reg.get("confirm_research_spec").requires_elicitation is False
    # confirm IS approval-gated (the human-in-loop substrate)
    assert reg.get("confirm_research_spec").requires_approval is True
    assert reg.get("propose_research_spec").requires_approval is False


def test_confirm_without_draft_is_a_clean_error():
    reg = ToolRegistry()
    create_elicitation_tools(reg)
    out = reg.get("confirm_research_spec").func()
    assert out["confirmed"] is False
    assert "no draft" in out["error"]


@pytest.mark.parametrize(
    "kwargs,expected",
    [
        ({"base_elements": ["Cu", "Ni"]}, {"Cu", "Ni"}),
        ({"system": "Ti"}, {"Ti"}),
        ({"composition": {"atoms": {"Cu": 97, "Si": 3}}}, {"Cu", "Si"}),
        ({"structure": {"composition": {"atoms": {"W": 50}}}}, {"W"}),
        ({"candidate_grid": [{"Cu": 0.9, "Ni": 0.1}]}, {"Cu", "Ni"}),
        ({"temperature": 1500}, set()),
    ],
)
def test_scope_extraction_is_structural(kwargs, expected):
    assert extract_element_scope(kwargs) == expected
