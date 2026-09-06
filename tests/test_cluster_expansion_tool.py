"""cluster_expansion_fit / monte_carlo_sro on Ni-Cu with EMT (never MACE).

Two campaigns fitted a cluster expansion on a parent lattice of
a_eff * sqrt(2) (about 4.02 A, aluminium-sized) for a nickel alloy, and one
of them sampled it on a different lattice than it was fitted on. These
tests pin the three guards: derive a from volume, refuse an implausible a,
and sample on exactly the fitted lattice.
"""

from __future__ import annotations

import pytest
from ase.build import bulk
from ase.calculators.emt import EMT

from app.tools import cluster_expansion
from app.tools.base import ToolRegistry

NI_CU = {"atoms": {"Ni": 4, "Cu": 4}}
FAST = dict(n_structures=8, supercell=2, cutoffs_A=[4.0], seed=7)


@pytest.fixture(autouse=True)
def emt_and_tmp_dir(tmp_path, monkeypatch):
    monkeypatch.setattr(cluster_expansion, "_calc_factory", lambda: EMT())
    monkeypatch.setattr(cluster_expansion, "_CE_DIR", tmp_path / "ce")


def _fit(**kw):
    return cluster_expansion._cluster_expansion_fit(composition=NI_CU, **FAST, **kw)


def test_fit_derives_the_lattice_from_volume_not_a_eff(monkeypatch):
    # 32-atom conventional fcc supercell at a=3.56: V/atom = a^3/4, so the
    # right answer is (4V)^(1/3) = 3.56 and the a_eff*sqrt(2) answer is 4.02.
    relaxed = bulk("Ni", "fcc", a=3.56, cubic=True).repeat(2)
    monkeypatch.setattr(cluster_expansion, "_atoms_from_cache_ref", lambda ref: relaxed)
    out = _fit(relaxed_cache_ref="cache://relaxed")
    assert "error" not in out, out
    assert out["parent_lattice_a_A"] == pytest.approx(3.56, abs=1e-3)
    assert out["lattice_source"] == "relaxed_cache_ref"


def test_fit_refuses_an_aluminium_sized_lattice_for_a_nickel_alloy():
    out = _fit(lattice_a_A=4.02)
    assert "error" in out
    assert "cube root" in out["hint"]
    assert out["reference_a_A"] == pytest.approx(3.565, abs=1e-3)
    assert "ce_ref" not in out


def test_fit_reports_cv_and_gate():
    out = _fit(lattice_a_A=3.56)
    assert "error" not in out, out
    assert isinstance(out["cv_rmse_meV_per_atom"], float)
    assert isinstance(out["usable"], bool)
    assert out["ce_ref"].startswith("ce://")
    assert out["provenance"]["wasGeneratedBy"]["engine"] == "icet"


def test_monte_carlo_uses_exactly_the_fitted_lattice():
    fit = _fit(lattice_a_A=3.56)
    mc = cluster_expansion._monte_carlo_sro(
        ce_ref=fit["ce_ref"], temperature_K=800, n_cells=3, n_sweeps=10, seed=7
    )
    assert "error" not in mc, mc
    assert mc["parent_lattice_a_A"] == fit["parent_lattice_a_A"]
    wc = mc["warren_cowley"]
    key = "Cu-Ni" if "Cu-Ni" in wc else "Ni-Cu"
    assert isinstance(wc[key], float)
    assert mc["fit_cv_rmse_meV_per_atom"] == fit["cv_rmse_meV_per_atom"]
    assert mc["provenance"]["wasGeneratedBy"]["engine"] == "mchammer"


def test_tools_register():
    reg = ToolRegistry()
    cluster_expansion.create_cluster_expansion_tools(reg)
    tools = reg.list_tools()
    assert {t.name for t in tools} == {"cluster_expansion_fit", "monte_carlo_sro"}
    assert all(t.requires_approval for t in tools)
