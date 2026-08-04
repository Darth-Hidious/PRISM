"""Pyiron-free Quantum ESPRESSO I/O for PRISM.

Input generation (``write_input``) and output parsing (``parse_output``)
for pw.x, built on ASE's mature espresso formats plus pymatgen structure
handling. Pseudopotentials are resolved from a local directory of UPF
files; nothing is ever downloaded. QE itself is NOT executed here — there
is no pw.x binary in this environment; this package produces and consumes
the files a real HPC allocation runs.

The tools in ``tools.py`` are registered by ``app/plugins/bootstrap.py``
only when :func:`check_qe_available` reports the dependencies importable.
"""
from __future__ import annotations

from app.tools.simulation.qe.input_writer import CALCULATION_TYPES, write_input
from app.tools.simulation.qe.output_parser import parse_output
from app.tools.simulation.qe.pseudos import (
    PseudopotentialNotFoundError,
    resolve_pseudopotentials,
)

__all__ = [
    "CALCULATION_TYPES",
    "write_input",
    "parse_output",
    "resolve_pseudopotentials",
    "PseudopotentialNotFoundError",
    "check_qe_available",
]


def check_qe_available() -> bool:
    """True iff every dependency this path needs can actually be imported.

    Bootstrap gates registration on this — when False the QE tools are
    absent from the catalog entirely rather than registered-but-broken.
    """
    try:
        import ase.io  # noqa: F401
        from ase.io.formats import ioformats
        if "espresso-in" not in ioformats or "espresso-out" not in ioformats:
            return False
        import pymatgen.core  # noqa: F401
        from pymatgen.io.ase import AseAtomsAdaptor  # noqa: F401
        return True
    except Exception:
        return False
