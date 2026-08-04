"""Regression tests for the pycalphad Python 3.14 (PEP 649) shim.

pycalphad 0.11.2's ``Workspace.__init__`` reads ``self.__annotations__``.
PEP 649 made annotations lazy in 3.14 and removed the instance-level
attribute, so *every* equilibrium — including the functional
``pycalphad.equilibrium()`` used across this repo — raised
``AttributeError`` on that interpreter. The existing CALPHAD tests stayed
green because they mock availability or stop short of a solve, so the suite
reported healthy while the capability was dead.

These tests exercise a real solve so that cannot recur.
"""

import sys

import pytest

from app.tools.pycalphad_compat import apply_py314_workspace_shim

pycalphad = pytest.importorskip("pycalphad", reason="[calphad] extra not installed")


def _alzr_tdb(tmp_path):
    """Wang/Jin/Zhao (2001) Al-Zr database, shipped inside kawin's tests."""
    databases = pytest.importorskip(
        "kawin.tests.databases", reason="[precipitation] extra not installed"
    )
    path = tmp_path / "alzr.tdb"
    path.write_text(databases.ALZR_TDB)
    return path


def test_shim_is_idempotent_and_safe_on_older_interpreters():
    apply_py314_workspace_shim()
    apply_py314_workspace_shim()

    from pycalphad.core import workspace

    if sys.version_info >= (3, 14):
        assert getattr(
            workspace.Workspace.__init__, "_prism_py314_annotations_shim", False
        )
    # Below 3.14 the shim must not touch anything at all.
    else:
        assert not getattr(
            workspace.Workspace.__init__, "_prism_py314_annotations_shim", False
        )


def test_functional_equilibrium_solves_after_shim(tmp_path):
    """The exact call shape used by calphad_bridge and materials.calculations."""
    apply_py314_workspace_shim()
    from pycalphad import Database, equilibrium, variables as v

    result = equilibrium(
        Database(str(_alzr_tdb(tmp_path))),
        ["AL", "ZR", "VA"],
        ["FCC_A1", "AL3ZR"],
        {v.T: 723.15, v.P: 101325, v.N: 1, v.X("ZR"): 0.02},
    )
    gibbs = float(result.GM.values.squeeze())
    # Real solve, finite energy — not a mock and not a swallowed failure.
    assert gibbs < 0.0


def test_calphad_bridge_equilibrium_reaches_a_real_solve(tmp_path, monkeypatch):
    """Guards the wiring, not just the shim: the bridge must apply it itself."""
    from app.tools.simulation import calphad_bridge as bridge_module

    store = tmp_path / "databases"
    store.mkdir()
    (store / "alzr_probe.tdb").write_text(_alzr_tdb(tmp_path).read_text())
    monkeypatch.setattr(
        bridge_module.DatabaseStore, "base_dir", store, raising=False
    )

    bridge = bridge_module.CalphadBridge()
    bridge.databases.base_dir = store

    result = bridge.calculate_equilibrium(
        database_name="alzr_probe",
        components=["AL", "ZR"],
        phases=["FCC_A1", "AL3ZR"],
        conditions={"T": 723.15, "P": 101325, "N": 1, "X(ZR)": 0.02},
    )

    assert "error" not in result, result.get("error")
    assert result["phases_present"]
    assert isinstance(result["gibbs_energy"], float)
