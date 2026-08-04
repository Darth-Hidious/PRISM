"""Tests for the pyiron-free Quantum ESPRESSO I/O path (app/tools/simulation/qe).

Covers, per the task brief:
  - write_input produces a real pw.x .in file (all cards, real values,
    readable back by ASE's espresso-in reader);
  - the UPF resolver maps elements to files and errors clearly on gaps;
  - parse_output extracts energy/forces/stress/wall time from a real
    (hand-constructed, QE 7.x-format) pw.x stdout sample;
  - the non-converged / crashed / truncated cases return an explicit
    failure with NO numeric fields — never a plausible-looking number;
  - tool registration and dispatch behave the same way.

Requires: ase + pymatgen installed (they are, per the `qe` extra).
"""
import pytest

ase_io = pytest.importorskip("ase.io")  # pragma: no cover

from ase.units import Ry as ASE_RY
from pymatgen.core import Lattice, Structure

from app.tools.base import ToolRegistry
from app.tools.simulation.qe import (
    check_qe_available,
    parse_output,
    resolve_pseudopotentials,
    write_input,
    PseudopotentialNotFoundError,
)
from app.tools.simulation.qe.tools import create_qe_tools


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

def si_structure() -> Structure:
    """Diamond-cubic Si, conventional-ish cubic cell with 2 atoms."""
    return Structure(
        Lattice.cubic(5.43),
        ["Si", "Si"],
        [[0.0, 0.0, 0.0], [0.25, 0.25, 0.25]],
    )


def make_pseudo_dir(tmp_path, names=("Si.pbe-n-rrkjus_psl.1.0.0.UPF",
                                     "O.pbe-n-rrkjus_psl.1.0.0.UPF")):
    d = tmp_path / "pseudos"
    d.mkdir()
    for n in names:
        (d / n).write_text("<UPF fake but present on disk>")
    return d


def converged_qe_output() -> str:
    """A hand-constructed pw.x (QE 7.x) stdout sample carrying every marker
    the parser requires. Numbers are consistent: -22.68191071 Ry total
    energy, two Si atoms, diagonal tensile stress, 12 s wall time."""
    return """     Program PWSCF v.7.3 starts on  5Aug2026 at 10: 0: 0

     This program is part of the Quantum ESPRESSO suite

     bravais-lattice index     =            0
     lattice parameter (alat)  =      10.2611  a.u.
     unit-cell volume          =     270.0000 (a.u.)^3
     number of atoms/cell      =            2
     number of atomic types    =            1
     number of electrons       =         8.00
     number of Kohn-Sham states=            8

     celldm(1)=  10.2611  celldm(2)=   0.0000  celldm(3)=   1.0000
     celldm(4)=   0.0000  celldm(5)=   0.0000  celldm(6)=   0.0000

     crystal axes: (cart. coord. in units of alat)
               a(1) = (   1.000000   0.000000   0.000000 )
               a(2) = (   0.000000   1.000000   0.000000 )
               a(3) = (   0.000000   0.000000   1.000000 )

     reciprocal axes: (cart. coord. in units 2 pi/alat)
               b(1) = (  1.000000  0.000000  0.000000 )
               b(2) = (  0.000000  1.000000  0.000000 )
               b(3) = (  0.000000  0.000000  1.000000 )

     kinetic-energy cutoff     =      60.0000  Ry
     charge density cutoff     =     480.0000  Ry
     Exchange-correlation: PBE

     Atomic positions and unit cell

     site n.     atom                  positions (alat units)
         1           Si  tau(   1) = (   0.0000000   0.0000000   0.0000000  )
         2           Si  tau(   2) = (   0.2500000   0.2500000   0.2500000  )

     number of k points=     1  gaussian smearing, width (Ry)=  0.0200
     Cartesian axes
     k = 0.0000 0.0000 0.0000     weight = 1.000000

     Self-consistent Calculation

     iteration #  1     ecut=    60.00 Ry     beta= 0.70
     Davidson diagonalization with overlap
     total cpu time spent up to now is        3.2 secs

     End of self-consistent calculation

          k = 0.0000 0.0000 0.0000 (  1234 PWs)   bands (ev):

    -5.8039   6.4337   6.4337   6.4337  11.0372

     the Fermi energy is     6.4337 ev

!    total energy              =     -22.68191071 Ry

     estimated scf accuracy    <       0.00000001 Ry

     convergence has been achieved in   8 iterations

     Forces acting on atoms (cartesian axes, Ry/au):

     atom    1 type  1   force =     0.00000000    0.00000000    0.00000000
     atom    2 type  1   force =     0.00000100    0.00000000    0.00000000

     Total force =     0.000001     Total SCF correction =     0.000000

     Computing stress (Cartesian axis) and pressure

          total   stress  (Ry/bohr**2)                   (kbar)     P=        1.23
   0.00000833   0.00000000   0.00000000            1.23        0.00        0.00
   0.00000000   0.00000833   0.00000000            0.00        1.23        0.00
   0.00000000   0.00000000   0.00000833            0.00        0.00        1.23

     Writing all to output format dir ./si.save/

     JOB DONE.

     WALL: 0h 0m12s CPU: 0h 0m10s
"""


# ---------------------------------------------------------------------------
# write_input
# ---------------------------------------------------------------------------

def test_write_input_produces_complete_pw_input(tmp_path):
    pseudo_dir = make_pseudo_dir(tmp_path)
    pseudos = resolve_pseudopotentials(["Si"], pseudo_dir)
    out = tmp_path / "si_scf.in"

    path = write_input(
        structure=si_structure(),
        pseudopotentials=pseudos,
        cutoffs={"ecutwfc": 60.0, "ecutrho": 480.0},
        kpoints=(8, 8, 8),
        calculation_type="scf",
        path=out,
        pseudo_dir=pseudo_dir,
    )
    assert path == out
    text = out.read_text()

    # All required cards with real values — no placeholders.
    for card in ("&CONTROL", "&SYSTEM", "&ELECTRONS", "ATOMIC_SPECIES",
                 "ATOMIC_POSITIONS", "K_POINTS", "CELL_PARAMETERS"):
        assert card in text, f"missing card {card}"
    assert "calculation      = 'scf'" in text
    assert "ecutwfc          = 60.0" in text
    assert "ecutrho          = 480.0" in text
    assert f"pseudo_dir       = '{pseudo_dir}'" in text
    assert "Si 28.085 Si.pbe-n-rrkjus_psl.1.0.0.UPF" in text
    assert "8 8 8" in text

    # The file must be machine-valid: ASE's own espresso-in reader parses
    # it back into the same structure.
    atoms = ase_io.read(out, format="espresso-in")
    assert len(atoms) == 2
    assert atoms.get_chemical_formula() == "Si2"
    assert pytest.approx(atoms.cell.lengths(), abs=1e-6) == (5.43, 5.43, 5.43)


def test_write_input_relax_adds_ion_dynamics(tmp_path):
    out = tmp_path / "si_relax.in"
    write_input(
        structure=si_structure(),
        pseudopotentials={"Si": "Si.upf"},
        cutoffs={"ecutwfc": 40.0},
        kpoints=None,  # Gamma-only
        calculation_type="vc-relax",
        path=out,
    )
    text = out.read_text()
    assert "calculation      = 'vc-relax'" in text
    assert "ion_dynamics     = 'bfgs'" in text
    assert "cell_dynamics    = 'bfgs'" in text
    assert "K_POINTS gamma" in text


def test_write_input_rejects_unknown_calculation_type(tmp_path):
    with pytest.raises(ValueError, match="Unsupported calculation_type"):
        write_input(si_structure(), {"Si": "Si.upf"}, {"ecutwfc": 40.0},
                    (2, 2, 2), "md", tmp_path / "x.in")


def test_write_input_requires_every_element_covered(tmp_path):
    sio2 = Structure(Lattice.cubic(5.0), ["Si", "O", "O"],
                     [[0, 0, 0], [0.5, 0.5, 0.0], [0.5, 0.0, 0.5]])
    with pytest.raises(ValueError, match="No pseudopotential supplied for"):
        write_input(sio2, {"Si": "Si.upf"}, {"ecutwfc": 60.0}, (4, 4, 4),
                    "scf", tmp_path / "x.in")


# ---------------------------------------------------------------------------
# Pseudopotential resolver
# ---------------------------------------------------------------------------

def test_resolve_pseudopotentials_maps_elements(tmp_path):
    d = make_pseudo_dir(tmp_path)
    mapping = resolve_pseudopotentials(["Si", "O"], d)
    assert mapping == {
        "Si": "Si.pbe-n-rrkjus_psl.1.0.0.UPF",
        "O": "O.pbe-n-rrkjus_psl.1.0.0.UPF",
    }


def test_resolve_pseudopotentials_exact_stem_wins(tmp_path):
    d = make_pseudo_dir(tmp_path, names=("Si.UPF", "Si.special.UPF"))
    mapping = resolve_pseudopotentials(["Si"], d)
    assert mapping["Si"] == "Si.UPF"


def test_resolve_pseudopotentials_errors_on_missing_element(tmp_path):
    d = make_pseudo_dir(tmp_path)
    with pytest.raises(PseudopotentialNotFoundError) as excinfo:
        resolve_pseudopotentials(["Si", "Fe"], d)
    msg = str(excinfo.value)
    assert "Fe" in msg                      # names what is missing
    assert "Si.pbe-n-rrkjus_psl" in msg     # lists what is present
    assert "does not download" in msg       # and says so plainly


def test_resolve_pseudopotentials_errors_on_empty_directory(tmp_path):
    d = tmp_path / "empty"
    d.mkdir()
    with pytest.raises(PseudopotentialNotFoundError, match="No .upf files"):
        resolve_pseudopotentials(["Si"], d)


# ---------------------------------------------------------------------------
# parse_output
# ---------------------------------------------------------------------------

def test_parse_output_converged_run(tmp_path):
    out = tmp_path / "si.out"
    out.write_text(converged_qe_output())
    result = parse_output(out)

    assert result["status"] == "ok"
    assert result["converged"] is True
    assert result["n_atoms"] == 2
    assert result["formula"] == "Si2"
    # -22.68191071 Ry converted by ASE to eV.
    assert result["total_energy_ev"] == pytest.approx(-22.68191071 * ASE_RY)
    forces = result["forces_ev_per_angstrom"]
    assert forces is not None and len(forces) == 2
    assert forces[0] == pytest.approx([0.0, 0.0, 0.0], abs=1e-12)
    assert forces[1][0] > 0.0  # 1e-6 Ry/au converted, sign preserved
    stress = result["stress_ev_per_angstrom3"]
    assert stress is not None and len(stress) == 3
    assert stress[0][0] == stress[1][1] == stress[2][2]
    assert result["wall_time_seconds"] == pytest.approx(12.0)


def test_parse_output_unconverged_returns_explicit_failure(tmp_path):
    text = converged_qe_output()
    text = text.replace(
        "convergence has been achieved in   8 iterations",
        "convergence NOT achieved within   50 iterations",
    )
    out = tmp_path / "si_bad.out"
    out.write_text(text)

    result = parse_output(out)
    assert result["status"] == "failed"
    assert result["converged"] is False
    assert "NOT achieved" in result["reason"]
    # The whole point: no plausible-looking numbers on a failed run.
    assert "total_energy_ev" not in result
    assert "forces_ev_per_angstrom" not in result
    assert "stress_ev_per_angstrom3" not in result


def test_parse_output_truncated_file_fails(tmp_path):
    text = converged_qe_output()
    # Cut before the final energy line, as a killed run would.
    truncated = text.split("!    total energy")[0]
    out = tmp_path / "si_truncated.out"
    out.write_text(truncated)

    result = parse_output(out)
    assert result["status"] == "failed"
    assert result["converged"] is False
    assert "total_energy_ev" not in result


def test_parse_output_missing_job_done_fails(tmp_path):
    text = converged_qe_output().replace("JOB DONE.", "")
    out = tmp_path / "si_nodone.out"
    out.write_text(text)

    result = parse_output(out)
    assert result["status"] == "failed"
    assert "did not finish" in result["reason"]
    assert "total_energy_ev" not in result


def test_parse_output_qe_internal_error_fails(tmp_path):
    text = converged_qe_output() + (
        "\n     Error in routine c_bands (1):\n"
        "     Too many bands\n"
    )
    out = tmp_path / "si_err.out"
    out.write_text(text)

    result = parse_output(out)
    assert result["status"] == "failed"
    assert "c_bands" in result["reason"]
    assert "total_energy_ev" not in result


def test_parse_output_missing_file_fails(tmp_path):
    result = parse_output(tmp_path / "nope.out")
    assert result["status"] == "failed"
    assert "not found" in result["reason"]


# ---------------------------------------------------------------------------
# Tool registration and dispatch
# ---------------------------------------------------------------------------

def _registry():
    reg = ToolRegistry()
    create_qe_tools(reg)
    return reg


def test_create_qe_tools_registers_three_tools():
    names = {t.name for t in _registry().list_tools()}
    assert names == {
        "qe_resolve_pseudopotentials",
        "qe_write_input",
        "qe_parse_output",
    }


def test_tool_write_and_parse_roundtrip(tmp_path):
    reg = _registry()
    pseudo_dir = make_pseudo_dir(tmp_path)
    result = reg._tools["qe_write_input"].execute(
        structure={
            "lattice": [[5.43, 0, 0], [0, 5.43, 0], [0, 0, 5.43]],
            "species": ["Si", "Si"],
            "coords": [[0, 0, 0], [0.25, 0.25, 0.25]],
        },
        cutoffs={"ecutwfc": 60.0, "ecutrho": 480.0},
        kpoints=[8, 8, 8],
        pseudo_dir=str(pseudo_dir),
        output_path=str(tmp_path / "si.in"),
    )
    assert result["status"] == "ok"
    assert result["n_atoms"] == 2
    assert (tmp_path / "si.in").exists()


def test_tool_parse_unconverged_surfaces_error_not_numbers(tmp_path):
    reg = _registry()
    text = converged_qe_output().replace(
        "convergence has been achieved in   8 iterations",
        "convergence NOT achieved within   50 iterations",
    )
    out = tmp_path / "bad.out"
    out.write_text(text)

    result = reg._tools["qe_parse_output"].execute(path=str(out))
    assert "error" in result
    assert result["converged"] is False
    assert "total_energy_ev" not in result


def test_tool_parse_validity_gate_marks_converged_ok(tmp_path):
    reg = _registry()
    out = tmp_path / "good.out"
    out.write_text(converged_qe_output())
    result = reg._tools["qe_parse_output"].execute(path=str(out))
    assert result.get("scientific_validity") == "ok"


def test_availability_gate_true_with_deps_installed():
    assert check_qe_available() is True


def test_bootstrap_contains_gated_qe_registration():
    """bootstrap.py must gate QE tool registration on check_qe_available
    (same pattern as the MACE block). Full build_registry() is not
    exercised here because it talks to the platform; this asserts the
    wiring text and that the gated call registers exactly the QE tools."""
    from pathlib import Path
    src = Path("app/plugins/bootstrap.py").read_text()
    assert "from app.tools.simulation.qe import check_qe_available" in src
    assert "create_qe_tools" in src
    # The gate must precede registration, and there must be no sidecar
    # fallback for these tools (no ghost registrations).
    gate_idx = src.index("check_qe_available()")
    reg_idx = src.index("create_qe_tools(registry)")
    assert gate_idx < reg_idx
    assert "_sidecar_proxy" not in src[gate_idx:reg_idx]
    assert _registry().list_tools()  # sanity: creation itself works
