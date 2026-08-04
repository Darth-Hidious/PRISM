# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Tests for the tiered evaluation ladder (app/tools/evaluation.py).

Covers the three things the ladder promises:
  1. one interface, every property carrying the tier + method that
     produced it (provenance is the product);
  2. explicit cheap-first escalation — nothing reaches tier N+1 unless it
     survived tier N;
  3. honest degradation — a tier with a missing dependency reports
     "unavailable" + install hint and fabricates NO numbers.

No network, no heavy physics: the escalation tests monkeypatch the tier
runners; the real tier-0 numbers come from the pure-math empirical screen.
"""
from __future__ import annotations

import pytest

from app.tools import evaluation as ev
from app.tools.base import ToolRegistry

WTM = {"composition": "W0.5Ta0.3Mo0.2"}


# ---------------------------------------------------------------------------
# 1. The interface + provenance
# ---------------------------------------------------------------------------

def test_tier0_evaluation_produces_provenanced_properties():
    result = ev.evaluate_candidate(WTM, tier=0)
    assert "error" not in result
    assert result["requested_tier"] == 0
    assert result["highest_completed_tier"] == 0

    block = result["tiers"]["0"]
    assert block["status"] == "ok"
    # The empirical numbers this candidate is actually scored by.
    props = block["properties"]
    assert isinstance(props["omega"], float)
    assert isinstance(props["VEC"], float)
    assert props["delta_radius_pct"] is not None
    assert props["criterion"].startswith("Yang")
    # Every property block names the tier and method that produced it.
    assert block["tier"] == 0
    assert block["name"] == "empirical_screen"
    assert "Yang" in block["method"]
    assert block["provenance"]["tier"] == 0
    assert block["provenance"]["wasGeneratedBy"]["engine"] == (
        "empirical-descriptor-screen"
    )
    assert block["provenance"]["units"]["delta_H_mix_kJ_per_mol"] == "kJ/mol"
    assert "reproduce" in block["provenance"]
    # Gate verdict for a BCC-forming refractory composition.
    assert block["gate"]["passed"] is True
    assert props["phase_prediction"] in (
        "solid_solution",
        "solid_solution_segregation_risk",
    )


def test_higher_tiers_are_opt_in():
    """Default fidelity is tier 0 — nothing expensive runs unasked."""
    result = ev.evaluate_candidate(WTM)
    assert result["requested_tier"] == 0
    assert list(result["tiers"].keys()) == ["0"]


def test_invalid_tier_and_bad_composition_rejected():
    assert "error" in ev.evaluate_candidate(WTM, tier=4)
    assert "error" in ev.evaluate_candidate(WTM, tier=-1)
    assert "error" in ev.evaluate_candidate({"composition": "W"}, tier=0)
    assert "error" in ev.evaluate_candidate({}, tier=0)


def test_non_unit_composition_never_enters_tier_zero():
    result = ev.evaluate_candidate(
        {"composition": "W0.6 Mo0.2 Ta0.4 Nb0.4 V0.4"}, tier=0
    )
    assert "error" in result
    assert "sum to 1.0" in result["error"]
    assert "tiers" not in result


# ---------------------------------------------------------------------------
# 2. Escalation order: survivors only, cheap first
# ---------------------------------------------------------------------------

def _ok_block(tier, gate=True, **props):
    return {
        "tier": tier,
        "name": ev.TIER_NAMES[tier],
        "status": "ok",
        "properties": props or {"marker": tier},
        "gate": {"passed": gate, "rule": "test rule", "reason": "test"},
    }


def test_escalation_visits_tiers_in_order_and_stops_at_gate(monkeypatch):
    calls: list[int] = []

    def runner(tier):
        def _run(candidate, elems, fracs):
            calls.append(tier)
            # Candidate fails the tier-1 gate: nothing above it may run.
            return _ok_block(tier, gate=(tier != 1))
        return _run

    for t in range(4):
        monkeypatch.setitem(ev._TIER_RUNNERS, t, runner(t))

    result = ev.evaluate_candidate(WTM, tier=3)
    assert calls == [0, 1]  # cheap-first, stopped by the tier-1 gate
    assert result["tiers"]["0"]["status"] == "ok"
    assert result["tiers"]["1"]["gate"]["passed"] is False
    assert result["tiers"]["2"]["status"] == "not_attempted"
    assert result["tiers"]["3"]["status"] == "not_attempted"
    assert result["highest_completed_tier"] == 1
    assert result["stopped_at"]["tier"] == 2


def test_escalation_survivor_reaches_requested_tier(monkeypatch):
    calls: list[int] = []

    def runner(tier):
        def _run(candidate, elems, fracs):
            calls.append(tier)
            return _ok_block(tier)
        return _run

    for t in range(4):
        monkeypatch.setitem(ev._TIER_RUNNERS, t, runner(t))

    result = ev.evaluate_candidate(WTM, tier=3)
    assert calls == [0, 1, 2, 3]
    assert result["highest_completed_tier"] == 3
    assert result["stopped_at"] is None


def test_escalate_candidates_summary_counts(monkeypatch):
    """Batch ladder: two candidates, one dies at the tier-0 gate."""
    good = {"composition": "W0.5Ta0.3Mo0.2"}
    bad = {"composition": "Cu0.9W0.1"}

    def tier0(candidate, elems, fracs):
        survive = candidate is good or candidate.get("composition") == good["composition"]
        return _ok_block(0, gate=survive)

    monkeypatch.setitem(ev._TIER_RUNNERS, 0, tier0)
    for t in (1, 2, 3):
        monkeypatch.setitem(ev._TIER_RUNNERS, t, runner_ok(t))

    out = ev.escalate_candidates([good, bad], max_tier=3)
    assert out["summary"]["0"]["entered"] == 2
    assert out["summary"]["0"]["survived"] == 1
    assert out["summary"]["1"]["entered"] == 1   # only the survivor
    assert out["summary"]["3"]["completed"] == 1
    bad_tiers = out["results"][1]["tiers"]
    assert bad_tiers["1"]["status"] == "not_attempted"


def runner_ok(tier):
    def _run(candidate, elems, fracs):
        return _ok_block(tier)
    return _run


# ---------------------------------------------------------------------------
# 3. Missing-dependency degradation: unavailable + hint, no numbers
# ---------------------------------------------------------------------------

def test_mace_missing_reports_unavailable_with_hint(monkeypatch):
    monkeypatch.setattr(
        "app.tools.simulation.mace_bridge.check_mace_available",
        lambda: False,
    )
    result = ev.evaluate_candidate(WTM, tier=2)
    block = result["tiers"]["1"]
    assert block["status"] == "unavailable"
    assert "install_hint" in block
    assert "properties" not in block          # never fabricate
    # Tier 2 is above the broken rung: never attempted.
    assert result["tiers"]["2"]["status"] == "not_attempted"
    assert result["highest_completed_tier"] == 0


def test_tier_status_shape_and_honesty(monkeypatch):
    status = ev.tier_status()
    for key in ("0", "1", "2", "3"):
        assert key in status
        assert isinstance(status[key]["available"], bool)
        assert "method" in status[key]
    # Tier 3 always reports the io/execution split.
    t3 = status["3"]
    assert set(t3) >= {"io_available", "execution_available", "available"}
    assert t3["available"] == bool(t3["io_available"] and t3["execution_available"])
    # When deps are missing the tier says so with a hint, per the
    # gated-registration pattern.
    monkeypatch.setattr(
        "app.tools.simulation.calphad_bridge.check_calphad_available",
        lambda: False,
    )
    status = ev.tier_status()
    assert status["2"]["available"] is False
    assert "install_hint" in status["2"]


def test_crashing_tier_fails_loudly_and_fabricates_nothing(monkeypatch):
    def boom(candidate, elems, fracs):
        raise RuntimeError("engine exploded")

    monkeypatch.setitem(ev._TIER_RUNNERS, 1, boom)
    result = ev.evaluate_candidate(WTM, tier=1)
    block = result["tiers"]["1"]
    assert block["status"] == "failed"
    assert "exploded" in block["error"]
    assert "properties" not in block


# ---------------------------------------------------------------------------
# Tier 3 wiring: input generation works, execution absent (no pw.x here)
# ---------------------------------------------------------------------------

def test_tier3_input_generated_but_not_executed(tmp_path, monkeypatch):
    try:
        from app.tools.simulation.qe import check_qe_available
    except Exception:
        pytest.skip("qe deps not importable")
    if not check_qe_available():
        pytest.skip("qe deps not importable")

    pseudo_dir = tmp_path / "pseudos"
    pseudo_dir.mkdir()
    for el in ("W", "Ta", "Mo"):
        (pseudo_dir / f"{el}.pbe-n-rrkjus_psl.UPF").write_text("<UPF test stub>")

    # Bypass tiers 0-2 (unit test: only tier-3 wiring is under test).
    for t in (0, 1, 2):
        monkeypatch.setitem(ev._TIER_RUNNERS, t, runner_ok(t))

    result = ev.evaluate_candidate(
        {
            **WTM,
            "qe_pseudo_dir": str(pseudo_dir),
            "qe_workdir": str(tmp_path / "work"),
        },
        tier=3,
    )
    block = result["tiers"]["3"]
    import shutil as _sh

    if _sh.which("pw.x"):
        pytest.skip("pw.x present — execution path exercised elsewhere")
    assert block["status"] == "input_generated"
    assert block["properties"] == {}          # no DFT number without a run
    assert block["execution"]["status"] == "unavailable"
    assert block["execution"]["verified"] is False
    # The input file is real: written for W/Ta/Mo with resolved pseudos.
    in_file = block["input"]["input_file"]
    text = open(in_file).read()
    assert "ATOMIC_SPECIES" in text and "W" in text and "Ta" in text and "Mo" in text
    # Regression: the BCC reference lattice must come from the CUBIC cell —
    # the primitive BCC basis has negative diagonals and once produced a
    # negative lattice constant here.
    lines = text.splitlines()
    i = next(j for j, l in enumerate(lines) if l.startswith("CELL_PARAMETERS"))
    a_x = float(lines[i + 1].split()[0])
    assert a_x > 0, f"lattice vector must be positive, got {a_x}"
    assert "a_eff=-" not in block["input"]["structure_note"]
    assert block["gate"]["passed"] is False   # cannot gate on a run that never ran


def test_tier3_missing_pseudopotentials_is_honest(tmp_path, monkeypatch):
    try:
        from app.tools.simulation.qe import check_qe_available
    except Exception:
        pytest.skip("qe deps not importable")
    if not check_qe_available():
        pytest.skip("qe deps not importable")

    empty = tmp_path / "no_pseudos"
    empty.mkdir()
    for t in (0, 1, 2):
        monkeypatch.setitem(ev._TIER_RUNNERS, t, runner_ok(t))
    result = ev.evaluate_candidate(
        {**WTM, "qe_pseudo_dir": str(empty), "qe_workdir": str(tmp_path / "w")},
        tier=3,
    )
    block = result["tiers"]["3"]
    assert block["status"] in ("failed", "unavailable")
    assert "properties" not in block or block.get("properties") in (None, {})


# ---------------------------------------------------------------------------
# Tool registration
# ---------------------------------------------------------------------------

def test_tools_register_and_dispatch_tier0():
    reg = ToolRegistry()
    ev.create_evaluation_tools(reg)
    names = {t.name for t in reg.list_tools()}
    assert {"evaluate_candidate", "evaluation_tier_status"} <= names

    tool = reg.get("evaluate_candidate")
    out = tool.func(composition="W0.5Ta0.3Mo0.2", tier=0)
    assert out["tiers"]["0"]["status"] == "ok"
    assert out["tiers"]["0"]["provenance"]["tier"] == 0

    status_tool = reg.get("evaluation_tier_status")
    st = status_tool.func()
    assert set(st["tiers"]) == {"0", "1", "2", "3"}
