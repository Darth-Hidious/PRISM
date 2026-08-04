"""Quantum ESPRESSO (pw.x) stdout parser.

``parse_output`` reads a pw.x ``.out`` file and returns structured results:
total energy (eV), forces (eV/Angstrom), stress (eV/Angstrom^3), the
convergence flag and the wall time.

Failure policy (brief rule: never emit a plausible-looking number for a run
that did not converge):

- If the run hit a QE runtime error (``Error in routine ...``), did not
  print QE's convergence marker, printed ``convergence NOT achieved``, or
  never reached ``JOB DONE.`` (killed / truncated run), the result is
  ``{"status": "failed", "converged": False, "reason": ...}`` with NO
  numeric fields at all.
- Only files that carry all markers are parsed for numbers, using ASE's
  ``espresso-out`` reader (``ase.io.read(..., format="espresso-out")``)
  which converts QE's Ry/Bohr units to eV/Angstrom.

Stress sign convention follows ASE (opposite of the stress tensor QE prints);
the unit conversion is ASE's, not ours.
"""
from __future__ import annotations

import re
from pathlib import Path
from typing import Dict, Union

# QE prints e.g. "WALL: 0h 0m12s CPU: 0h 0m10s" at the end of a run.
_WALL_RE = re.compile(r"WALL:\s*(\d+)h\s*(\d+)m\s*(\d+)s")
_QE_ERROR_RE = re.compile(r"Error in routine\s+(\S+)")

_CONVERGED_MARKER = "convergence has been achieved"
_NOT_CONVERGED_MARKER = "convergence NOT achieved"
_JOB_DONE_MARKER = "JOB DONE."


def _failed(reason: str) -> Dict:
    return {"status": "failed", "converged": False, "reason": reason}


def _wall_time_seconds(text: str) -> float | None:
    m = _WALL_RE.search(text)
    if not m:
        return None
    h, mn, s = (int(g) for g in m.groups())
    return float(h * 3600 + mn * 60 + s)


def parse_output(path: Union[str, Path]) -> Dict:
    """Parse a pw.x output file into structured results.

    Returns
    -------
    dict
        On success (``status == "ok"``)::

            {
              "status": "ok",
              "converged": True,
              "total_energy_ev": float,
              "forces_ev_per_angstrom": [[x, y, z], ...] | None,
              "stress_ev_per_angstrom3": [[xx, xy, xz],
                                          [yx, yy, yz],
                                          [zx, zy, zz]] | None,
              "wall_time_seconds": float | None,
              "n_atoms": int,
              "formula": str,
            }

        ``forces``/``stress`` are None when the run did not print them
        (e.g. tprnfor/tstress were off) — absent, not invented.

        On any failure::

            {"status": "failed", "converged": False, "reason": "..."}

        with no numeric fields.
    """
    path = Path(path)
    if not path.is_file():
        return _failed(f"Output file not found: {path}")

    try:
        text = path.read_text(errors="replace")
    except OSError as e:
        return _failed(f"Could not read {path}: {e}")

    err = _QE_ERROR_RE.search(text)
    if err:
        return _failed(
            f"pw.x died with an internal error in routine {err.group(1)}; "
            f"no trustworthy results exist in {path.name}"
        )

    if _NOT_CONVERGED_MARKER in text:
        return _failed(
            "pw.x printed 'convergence NOT achieved' — the SCF cycle hit its "
            "iteration limit without converging"
        )

    if _CONVERGED_MARKER not in text:
        return _failed(
            "No convergence marker ('convergence has been achieved') found — "
            "the output is truncated or the run never finished a SCF cycle"
        )

    if _JOB_DONE_MARKER not in text:
        return _failed(
            "Run did not finish: no 'JOB DONE.' marker — the file is from a "
            "killed or still-running calculation"
        )

    # All markers present: extract numbers with ASE's espresso-out reader.
    from ase.io import read as ase_read

    try:
        atoms = ase_read(path, format="espresso-out")
    except Exception as e:
        return _failed(
            f"Output carries convergence markers but ASE could not parse "
            f"it ({type(e).__name__}: {e})"
        )

    try:
        energy = float(atoms.get_potential_energy())
    except Exception as e:
        return _failed(
            f"Parsed output has no total energy ({type(e).__name__}: {e})"
        )

    try:
        forces = atoms.get_forces().tolist()
    except Exception:
        forces = None  # honest absence, not a fabricated zero

    try:
        stress = atoms.get_stress(voigt=False).tolist()
    except Exception:
        stress = None

    return {
        "status": "ok",
        "converged": True,
        "total_energy_ev": energy,
        "forces_ev_per_angstrom": forces,
        "stress_ev_per_angstrom3": stress,
        "wall_time_seconds": _wall_time_seconds(text),
        "n_atoms": len(atoms),
        "formula": atoms.get_chemical_formula(),
    }
