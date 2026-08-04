"""Tests for the KWN precipitation-kinetics tool
(app/tools/materials/precipitation).

Two groups:

1. Harness-contract tests (run WITHOUT kawin installed): input-validation
   failures must be structured and carry NO numeric keys, and bootstrap
   must gate registration so the tool is absent from the catalog when the
   deps are missing — never registered-but-broken.

2. Physics tests (skipped when kawin/pycalphad cannot be imported here):
   an isothermal Al-Zr run whose volume fraction approaches the pycalphad
   equilibrium value, coarsening behaviour at long time (mean radius
   rising while number density falls), and a temperature-time schedule.
   The Al-Zr TDB is the published Wang/Jin/Zhao 2001 database that kawin
   itself ships in kawin.tests.databases — nothing invented here.

   The long isothermal solve is shared through a module-scoped fixture;
   on this machine it takes ~1.5 minutes of wall time.
"""
from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import pytest

from app.plugins.bootstrap import build_full_registry
from app.tools.base import ToolRegistry
from app.tools.materials.precipitation import check_precipitation_available
from app.tools.materials.precipitation.kinetics import (
    NUMERIC_RESULT_KEYS,
    run_kwn,
)


# ---------------------------------------------------------------------------
# Shared fixtures / helpers
# ---------------------------------------------------------------------------

def _registry() -> ToolRegistry:
    reg = ToolRegistry()
    from app.tools.materials.precipitation.tools import create_precipitation_tools

    create_precipitation_tools(reg)
    return reg


def _skip_without_kawin():
    if not check_precipitation_available():
        pytest.skip("kawin/pycalphad not importable in this interpreter")


def _write_alzr_tdb(tmp_path: Path) -> Path:
    """kawin ships the Wang/Jin/Zhao 2001 Al-Zr database for its own unit
    tests; reuse that real, published TDB instead of inventing one."""
    from kawin.tests.databases import ALZR_TDB

    p = tmp_path / "alzr_wang2001.tdb"
    p.write_text(ALZR_TDB)
    return p


def _base_inputs(tdb: Path) -> dict:
    """Isothermal Al-2at%Zr hold at 450 C (723.15 K) with the Zr-in-FCC-Al
    diffusivity kawin's own example uses (D0 = 0.0768 m^2/s,
    Q = 242 kJ/mol) and dislocation nucleation in a 1 um grain size,
    1e15 m/m^3 dislocation density microstructure."""
    return {
        "database": str(tdb),
        "components": ["AL", "ZR"],
        "matrix_phase": "FCC_A1",
        "precipitates": [{
            "phase": "AL3ZR",
            "interfacial_energy_J_per_m2": 0.05,
            "molar_volume_m3_per_mol": 1.00e-5,
            "atoms_per_unit_cell": 4,
            "nucleation_site": "DISLOCATIONS",
        }],
        "matrix_composition": {"ZR": 0.02},
        "diffusivity": {"D0_m2_per_s": 0.0768, "Q_J_per_mol": 242000.0},
        "temperature_K": 723.15,
        "time_s": 3.6e6,
        "matrix_molar_volume_m3_per_mol": 1.00e-5,
        "matrix_atoms_per_unit_cell": 4,
        "grain_size_um": 1.0,
        "dislocation_density_m_per_m3": 1e15,
    }


def _assert_no_numeric_content(result: dict):
    """A failure must be structurally obvious: none of the physical output
    keys, and no numeric values anywhere at the top level of the dict."""
    for key in NUMERIC_RESULT_KEYS:
        assert key not in result, f"failure dict leaked numeric key {key!r}"
    for k, v in result.items():
        assert not isinstance(v, (int, float)) or isinstance(v, bool), \
            f"failure dict carries a numeric value at {k!r}: {v!r}"


@pytest.fixture(scope="module")
def long_isothermal_run(tmp_path_factory):
    """One long isothermal KWN solve shared by the equilibrium and
    coarsening tests (the solve itself takes ~1-2 minutes)."""
    _skip_without_kawin()
    tmp_path = tmp_path_factory.mktemp("kwn")
    tdb = _write_alzr_tdb(tmp_path)
    result = run_kwn(**_base_inputs(tdb))
    assert result["status"] == "ok", result.get("error")
    return result, tdb


# ---------------------------------------------------------------------------
# Harness contract: absence, gating, structured failure
# ---------------------------------------------------------------------------

@patch("app.tools.materials.precipitation.check_precipitation_available",
       return_value=False)
def test_tool_absent_from_registry_when_deps_missing(mock_check):
    """With check_precipitation_available() == False the tool must not be
    in the catalog at all (no registered-but-broken ghost)."""
    registry, _prov, _agents = build_full_registry(enable_mcp=False,
                                                   enable_plugins=False)
    names = {t.name for t in registry.list_tools()}
    assert "precipitation_kinetics" not in names


def test_tool_present_when_deps_available():
    """Positive half of the gate: with kawin + pycalphad importable the
    tool IS in the catalog."""
    if not check_precipitation_available():
        pytest.skip("kawin/pycalphad not importable in this interpreter")
    registry, _prov, _agents = build_full_registry(enable_mcp=False,
                                                   enable_plugins=False)
    names = {t.name for t in registry.list_tools()}
    assert "precipitation_kinetics" in names


def test_bootstrap_contains_gated_precipitation_registration():
    """bootstrap.py must gate registration on check_precipitation_available
    with the gate preceding the create_* call and no sidecar fallback
    (mirrors the QE gating test in tests/test_qe_io.py)."""
    src = Path("app/plugins/bootstrap.py").read_text()
    assert ("from app.tools.materials.precipitation import "
            "check_precipitation_available") in src
    assert "create_precipitation_tools" in src
    gate_idx = src.index("check_precipitation_available()")
    reg_idx = src.index("create_precipitation_tools(registry)")
    assert gate_idx < reg_idx
    assert "_sidecar_proxy" not in src[gate_idx:reg_idx]


def test_missing_database_is_structured_failure(tmp_path):
    """A nonexistent TDB must halt with an explicit failure and no numbers —
    never a plausible-looking radius invented to keep the pipeline moving."""
    inputs = _base_inputs(tmp_path / "does_not_exist.tdb")
    result = run_kwn(**inputs)
    assert result["status"] == "failed"
    assert result["converged"] is False
    assert result["stage"] == "input_validation"
    assert "error" in result
    _assert_no_numeric_content(result)


def test_missing_interfacial_energy_is_structured_failure(tmp_path):
    """Interfacial energy is physics the tool must be given; without it the
    run halts explicitly rather than defaulting to a made-up gamma."""
    pytest.importorskip("kawin")
    tdb = _write_alzr_tdb(tmp_path)
    inputs = _base_inputs(tdb)
    del inputs["precipitates"][0]["interfacial_energy_J_per_m2"]
    result = run_kwn(**inputs)
    assert result["status"] == "failed"
    assert result["stage"] == "input_validation"
    assert "interfacial_energy" in result["error"]
    _assert_no_numeric_content(result)


def test_bad_composition_is_structured_failure(tmp_path):
    pytest.importorskip("kawin")
    tdb = _write_alzr_tdb(tmp_path)
    inputs = _base_inputs(tdb)
    inputs["matrix_composition"] = {"ZR": 1.4}  # impossible mole fraction
    result = run_kwn(**inputs)
    assert result["status"] == "failed"
    assert result["stage"] == "input_validation"
    _assert_no_numeric_content(result)


def test_unknown_phase_fails_explicitly(tmp_path):
    """A phase absent from the TDB must surface as a structured failure
    from the thermodynamics stage — not an exception, not a fake solve."""
    pytest.importorskip("kawin")
    tdb = _write_alzr_tdb(tmp_path)
    inputs = _base_inputs(tdb)
    inputs["precipitates"][0]["phase"] = "NOT_A_PHASE"
    result = run_kwn(**inputs)
    assert result["status"] == "failed"
    assert result["stage"] in ("thermodynamics_setup", "model_setup", "solve")
    assert "error" in result
    _assert_no_numeric_content(result)


def test_tool_carries_units_schema_example_and_gate():
    """The registered tool carries the scientific authoring contract:
    units, output_schema, an example and a validity gate."""
    reg = _registry()
    tool = reg.get("precipitation_kinetics")
    assert tool.units and "phases.mean_radius_m" in tool.units
    assert tool.units["phases.mean_radius_m"] == "EMMO:metre"
    assert tool.output_schema is not None
    assert tool.examples
    assert tool.validate is not None


# ---------------------------------------------------------------------------
# Physics (needs kawin + pycalphad importable)
# ---------------------------------------------------------------------------

def test_isothermal_volume_fraction_approaches_equilibrium(long_isothermal_run):
    """Physical sanity check: after a long isothermal hold the KWN volume
    fraction of AL3ZR must approach the equilibrium phase fraction computed
    independently with pycalphad on the same TDB (equal molar volumes make
    mole fraction == volume fraction here)."""
    result, tdb = long_isothermal_run

    vf = result["phases"]["AL3ZR"]["volume_fraction"]
    assert vf[-1] > 0.0, "no precipitation occurred at all"

    # Independent equilibrium reference via pycalphad.
    from app.tools.materials.precipitation.compat import apply_py314_workspace_shim

    apply_py314_workspace_shim()
    from pycalphad import Database, equilibrium, variables as v

    db = Database(str(tdb))
    eq = equilibrium(db, ["AL", "ZR", "VA"], ["FCC_A1", "AL3ZR"],
                     {v.T: 723.15, v.P: 101325.0, v.X("ZR"): 0.02})
    import numpy as np

    phase_vals = [str(p).strip() for p in eq.Phase.values.squeeze().flat]
    frac_vals = eq.NP.values.squeeze().flat
    eq_frac = None
    for phase, frac in zip(phase_vals, frac_vals):
        if phase == "AL3ZR" and np.isfinite(frac):
            eq_frac = float(frac)
    assert eq_frac is not None and eq_frac > 0, \
        "pycalphad reference did not find AL3ZR at equilibrium"

    # KWN conserves mass, so it cannot meaningfully overshoot the
    # equilibrium fraction; after a long hold it must have closed most of
    # the gap (discretization allows a small ~2% overshoot band).
    assert vf[-1] <= eq_frac * 1.02
    assert vf[-1] == pytest.approx(eq_frac, rel=0.35), \
        f"final volume fraction {vf[-1]:.4g} far from equilibrium {eq_frac:.4g}"

    # Matrix composition trajectory must deplete toward equilibrium too.
    zr = result["matrix_composition"]["ZR"]
    assert zr[-1] < zr[0]
    assert zr[-1] >= 0.0


def test_coarsening_at_long_time(long_isothermal_run):
    """In the coarsening regime the mean radius grows while the number
    density falls (Ostwald ripening). Check the last quarter of the
    trajectory moves in that sense."""
    result, _tdb = long_isothermal_run

    r = result["phases"]["AL3ZR"]["mean_radius_m"]
    n = result["phases"]["AL3ZR"]["number_density_m3"]
    assert n[-1] > 0.0, "no precipitates to coarsen"

    i0 = int(len(r) * 0.75)
    r_tail = [x for x in r[i0:] if x > 0]
    n_tail = [x for x in n[i0:] if x > 0]
    assert len(r_tail) >= 2 and len(n_tail) >= 2
    assert r_tail[-1] > r_tail[0], \
        f"mean radius not growing in tail: {r_tail[0]:.3e} -> {r_tail[-1]:.3e}"
    assert n_tail[-1] < n_tail[0], \
        f"number density not falling in tail: {n_tail[0]:.3e} -> {n_tail[-1]:.3e}"


def test_temperature_time_schedule(tmp_path):
    """A two-step T-t profile (30 min at 723.15 K, then 10 min at 743.15 K)
    must run and record the temperature trajectory faithfully.

    Note the seconds->hours conversion exercised here: kawin's
    TemperatureParameters interpolates its array times in hours
    (np.interp(t/3600, ...) in setTemperatureArray); the wrapper converts
    so this tool's contract stays pure SI seconds. Without the conversion
    the profile silently flattens to its first temperature."""
    _skip_without_kawin()
    tdb = _write_alzr_tdb(tmp_path)
    inputs = _base_inputs(tdb)
    inputs.pop("temperature_K")
    inputs.pop("time_s")
    inputs["schedule"] = {
        "times_s": [0.0, 1800.0, 2400.0],
        "temperatures_K": [723.15, 723.15, 743.15],
    }
    result = run_kwn(**inputs)
    assert result["status"] == "ok", result.get("error")
    assert result["schedule"]["type"] == "temperature_time_profile"
    temps = result["temperature_K"]
    assert temps[0] == pytest.approx(723.15)
    assert max(temps) >= 743.0, "trajectory never reached the second plateau"
    assert result["phases"]["AL3ZR"]["volume_fraction"][-1] > 0.0
