"""Precipitation kinetics for PRISM: the Kampmann-Wagner Numerical (KWN)
model, wrapped from the `kawin` package (materialsgenomefoundation/kawin).

KWN couples classical nucleation theory, diffusion-limited growth and
Ostwald coarsening against a CALPHAD thermodynamic description (pycalphad)
to evolve a precipitate size distribution through a heat-treatment
schedule. It is the capability Thermo-Calc sells as TC-PRISMA.

The tools in ``tools.py`` are registered by ``app/plugins/bootstrap.py``
only when :func:`check_precipitation_available` reports kawin + pycalphad
importable. When they are not, the tools are absent from the catalog
entirely — never registered-but-broken.
"""
from __future__ import annotations

__all__ = ["check_precipitation_available"]


def check_precipitation_available() -> bool:
    """True iff every dependency this path needs can actually be imported.

    Bootstrap gates registration on this (mirrors check_qe_available in
    app/tools/simulation/qe/__init__.py). kawin needs its own thermo stack
    (kawin.thermo on pycalphad) plus the precipitation solver; importing
    the subpackages we actually use proves the install end to end.
    """
    try:
        import numpy  # noqa: F401
        import pycalphad  # noqa: F401
        import kawin.thermo  # noqa: F401
        import kawin.precipitation  # noqa: F401
        return True
    except Exception:
        return False
