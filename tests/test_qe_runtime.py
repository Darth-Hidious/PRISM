"""Quantum ESPRESSO as a standard run: found, configured, run, parsed — or an
honest account of which of those is missing.

Before this, the QE package wrote and parsed pw.x files "for a real HPC
allocation to run" and nothing in PRISM ran pw.x. With QE provisioned into
~/.prism/qe and a pseudopotential set with a manifest under
~/.prism/pseudopotentials, the runtime resolves the binary, the pseudos and
the defaults (cutoff, k-spacing, smearing, processes) from the environment,
the user config and the PRISM home, in that order, and `qe_run` does the
whole thing for a structure — write, run, parse — stamping execution
evidence and full provenance.
"""

import json
import os
import stat
from pathlib import Path

import pytest

from app.tools.simulation.qe import runtime


from tests.test_qe_io import converged_qe_output

CANNED_OUT = converged_qe_output()


def _fake_pw(tmp_path: Path) -> Path:
    """A stand-in pw.x that writes a canned converged output to stdout."""
    tmp_path.mkdir(parents=True, exist_ok=True)
    pw = tmp_path / "pw.x"
    pw.write_text("#!/bin/sh\ncat <<'EOF'\n" + CANNED_OUT + "\nEOF\n")
    pw.chmod(pw.stat().st_mode | stat.S_IEXEC)
    return pw


def test_pw_resolution_order_env_then_config_then_home_then_path(tmp_path, monkeypatch):
    env_pw = _fake_pw(tmp_path / "env")
    home_pw = tmp_path / "home" / ".prism" / "qe" / "bin" / "pw.x"
    home_pw.parent.mkdir(parents=True)
    home_pw.write_text("#!/bin/sh\n")
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    monkeypatch.delenv("PRISM_QE_PW", raising=False)
    monkeypatch.setenv("PATH", "")
    assert runtime.find_pw_x(config={}) == home_pw
    assert runtime.find_pw_x(config={"pw_path": str(env_pw)}) == env_pw
    monkeypatch.setenv("PRISM_QE_PW", str(env_pw))
    assert runtime.find_pw_x(config={"pw_path": str(home_pw)}) == env_pw, "env wins"
    monkeypatch.delenv("PRISM_QE_PW")
    home_pw.unlink()
    assert runtime.find_pw_x(config={}) is None, "nothing found is None, never a guess"


def test_defaults_are_stated_and_overridable():
    s = runtime.settings(config={})
    # None is the honest default: the cutoff is derived per species from the
    # pseudopotential set's own hints. 60 Ry is only a fallback, and one that
    # cutoff_for labels "convergence unverified" — never a silent number.
    assert s["ecutwfc_ry"] is None
    assert runtime.FALLBACK_ECUTWFC_RY == 60.0
    assert s["ecutrho_ratio"] == 4.0
    assert s["kspacing_inv_angstrom"] == pytest.approx(0.15)
    assert s["smearing"] == "mv"
    assert s["degauss_ry"] == pytest.approx(0.01)
    assert s["nproc"] >= 1
    s2 = runtime.settings(config={"ecutwfc_ry": 80, "nproc": 2})
    assert s2["ecutwfc_ry"] == 80.0 and s2["nproc"] == 2


def test_kpoints_from_spacing_is_a_ceiling_never_below_one():
    # Cubic cell a=3.52 Å (fcc Ni conventional): 2π/(3.52·0.15) ≈ 11.9 → 12.
    from pymatgen.core import Lattice, Structure

    s = Structure(Lattice.cubic(3.52), ["Ni"] * 4, [[0, 0, 0], [0.5, 0.5, 0], [0.5, 0, 0.5], [0, 0.5, 0.5]])
    assert runtime.kpoints_for(s, 0.15) == (12, 12, 12)
    assert runtime.kpoints_for(s, 10.0) == (1, 1, 1)


def test_qe_run_writes_runs_parses_and_says_where_everything_came_from(tmp_path, monkeypatch):
    from pymatgen.core import Lattice, Structure

    pw = _fake_pw(tmp_path)
    pseudo_dir = tmp_path / "pseudos"
    pseudo_dir.mkdir()
    (pseudo_dir / "Si.upf").write_text("<UPF/>")
    (pseudo_dir / "MANIFEST.json").write_text(json.dumps({"set": "test set", "license": "CC BY 4.0"}))
    structure = Structure(Lattice.cubic(5.43), ["Si", "Si"], [[0, 0, 0], [0.25, 0.25, 0.25]])
    out = runtime.qe_run(
        structure,
        calculation="scf",
        settings=runtime.settings(config={"pw_path": str(pw), "pseudo_dir": str(pseudo_dir), "nproc": 1}),
        workdir=tmp_path / "run",
        mpirun=None,
    )
    assert out["status"] == "ok", out
    assert out["converged"] is True
    assert out["total_energy_ev"] == pytest.approx(-22.68191071 * 13.605693122994)
    assert out["evidence_class"] == "reference_validated", out["evidence_class"]
    prov = out["provenance"]
    assert prov["pw_x"] == str(pw)
    assert prov["pseudopotentials"]["Si"] == "Si.upf"
    assert prov["pseudopotential_set"]["set"] == "test set"
    assert prov["cutoffs"]["ecutwfc"] == 60.0
    assert prov["kpoints"] == list(runtime.kpoints_for(structure, 0.15))
    assert Path(prov["input_path"]).exists() and Path(prov["output_path"]).exists()


def test_qe_run_without_a_binary_is_a_named_failure_not_a_guess(tmp_path):
    from pymatgen.core import Lattice, Structure

    structure = Structure(Lattice.cubic(5.43), ["Si", "Si"], [[0, 0, 0], [0.25, 0.25, 0.25]])
    out = runtime.qe_run(
        structure,
        calculation="scf",
        settings=runtime.settings(config={"pw_path": str(tmp_path / "missing" / "pw.x"), "pseudo_dir": str(tmp_path)}),
        workdir=tmp_path / "run",
        mpirun=None,
    )
    assert out["status"] == "unavailable"
    assert "pw.x" in out["reason"] and "prism provision qe" in out["remedy"]


# ---------------------------------------------------------------------------
# The tools and the command line the palette drives.
# ---------------------------------------------------------------------------


def test_qe_tools_register_run_and_status():
    from app.tools.base import ToolRegistry
    from app.tools.simulation.qe.tools import create_qe_tools

    reg = ToolRegistry()
    create_qe_tools(reg)
    names = {t.name for t in reg.list_tools()} if hasattr(reg, "list_tools") else set(reg.tools)
    assert {"qe_run", "qe_status"} <= names, names
    run = reg.get("qe_run") if hasattr(reg, "get") else reg.tools["qe_run"]
    schema = run.input_schema if hasattr(run, "input_schema") else run["input_schema"]
    for key in ("structure", "calculation", "ecutwfc_ry", "kspacing_inv_angstrom", "nproc"):
        assert key in schema["properties"], key


def test_qe_cli_status_and_settings_are_json(tmp_path, monkeypatch):
    import json
    import subprocess
    import sys

    monkeypatch.setenv("HOME", str(tmp_path))
    out = subprocess.run(
        [sys.executable, "-m", "app.tools.simulation.qe.cli", "status"],
        capture_output=True, text=True, cwd=".",
    )
    assert out.returncode == 0, out.stderr
    status = json.loads(out.stdout)
    assert status["ready"] is False and "prism provision qe" in status["remedy"]
    out = subprocess.run(
        [sys.executable, "-m", "app.tools.simulation.qe.cli", "settings", "--set", "ecutwfc_ry=80", "--set", "nproc=2"],
        capture_output=True, text=True, cwd=".",
    )
    assert out.returncode == 0, out.stderr
    settings = json.loads(out.stdout)
    assert settings["ecutwfc_ry"] == 80.0 and settings["nproc"] == 2
    # Persisted: a second status read sees the new defaults.
    again = json.loads(subprocess.run([sys.executable, "-m", "app.tools.simulation.qe.cli", "status"], capture_output=True, text=True, cwd=".").stdout)
    assert again["defaults"]["ecutwfc_ry"] == 80.0
    assert (tmp_path / ".prism" / "qe" / "settings.toml").exists()
