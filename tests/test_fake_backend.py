"""FakeBackend determinism."""

from __future__ import annotations

import pytest

from app.tools.simulation.mace.backends import FakeBackend
from app.tools.simulation.mace.backends.base import BackendJob


def _job(tool: str, ip: dict, key: str = "k1") -> BackendJob:
    return BackendJob(tool_name=tool, input_payload=ip, cache_key=key, seed=42)


def test_relax_determinism() -> None:
    fb = FakeBackend()
    ip = {
        "composition": {"atoms": {"Fe": 50, "Ti": 50}},
        "phase": "bcc",
        "n_atoms": 100,
        "options": {"head": "omat_pbe"},
    }
    r1 = fb.execute(_job("relax_structure", ip))
    r2 = fb.execute(_job("relax_structure", ip))
    assert r1["energy_per_atom_eV"] == pytest.approx(r2["energy_per_atom_eV"])
    assert r1["volume_per_atom_A3"] == pytest.approx(r2["volume_per_atom_A3"])


def test_relax_composition_sensitivity() -> None:
    fb = FakeBackend()
    ip_fe = {"composition": {"atoms": {"Fe": 100}}, "phase": "bcc", "n_atoms": 100, "options": {}}
    ip_mo = {"composition": {"atoms": {"Mo": 100}}, "phase": "bcc", "n_atoms": 100, "options": {}}
    r_fe = fb.execute(_job("relax_structure", ip_fe))
    r_mo = fb.execute(_job("relax_structure", ip_mo))
    # Mo should be more strongly bound than Fe in our fake table.
    assert r_mo["energy_per_atom_eV"] < r_fe["energy_per_atom_eV"]


def test_elastic_yields_full_summary() -> None:
    fb = FakeBackend()
    ip = {
        "structure": {
            "composition": {"atoms": {"Mo": 50, "Nb": 50}},
            "phase": "bcc",
            "n_atoms": 100,
        },
        "options": {},
    }
    r = fb.execute(_job("compute_elastic", ip))
    assert "C_GPa" in r and len(r["C_GPa"]) == 6
    assert r["K_VRH_GPa"] > 0
    assert r["pugh_verdict"] in {"ductile", "brittle"}


def test_dilute_solute_default_displaced() -> None:
    fb = FakeBackend()
    ip = {
        "matrix_composition": {"atoms": {"Mo": 25, "Nb": 25, "Ta": 25, "V": 25}},
        "matrix_phase": "bcc",
        "n_atoms": 100,
        "solute_element": "Fe",
        "options": {},
    }
    r = fb.execute(_job("compute_dilute_solute", ip))
    assert r["solute_element"] == "Fe"
    assert r["displaced_element"] in {"Mo", "Nb", "Ta", "V"}
    assert r["verdict"] in {"favourable", "unfavourable"}


def test_md_progress_calls() -> None:
    fb = FakeBackend()
    ip = {
        "structure": {
            "composition": {"atoms": {"Fe": 50, "Ti": 50}},
            "phase": "bcc",
            "n_atoms": 100,
        },
        "T_K": 1500.0,
        "n_steps": 100,
        "options": {},
    }
    captured: list[tuple[float, str, int, int]] = []
    fb.execute(_job("md_equilibrate", ip), progress=lambda p, m, s, t: captured.append((p, m, s, t)))
    assert captured  # at least one progress call
    assert captured[-1][0] == pytest.approx(100.0)


def test_phonon_temperatures_propagate() -> None:
    fb = FakeBackend()
    ip = {
        "structure": {
            "composition": {"atoms": {"Mo": 50, "Nb": 50}},
            "phase": "bcc",
            "n_atoms": 100,
        },
        "temperatures_K": [0.0, 500.0, 1500.0],
        "options": {},
    }
    r = fb.execute(_job("phonon_harmonic", ip))
    assert r["temperatures_K"] == [0.0, 500.0, 1500.0]
    assert len(r["F_vib_eV_per_atom"]) == 3
    assert r["is_dynamically_stable"] is True
