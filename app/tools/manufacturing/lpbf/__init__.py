"""LPBF printability and Kou solidification-cracking tools.

Registration is dependency-gated: when NumPy or SciPy is unavailable these
scientific tools are absent from PRISM's catalog instead of appearing as
registered-but-broken entries.
"""

from __future__ import annotations

from app.tools.manufacturing.lpbf.physics import (
    DEFAULT_BALLING_LENGTH_TO_WIDTH_THRESHOLD,
    DEFAULT_KEYHOLE_ENTHALPY_THRESHOLD,
    DEFAULT_LOF_OVERLAP_THRESHOLD,
    classify_parameter_set,
    generate_printability_map,
    kou_cracking_index,
    rosenthal_melt_pool_geometry,
)

__all__ = [
    "DEFAULT_BALLING_LENGTH_TO_WIDTH_THRESHOLD",
    "DEFAULT_KEYHOLE_ENTHALPY_THRESHOLD",
    "DEFAULT_LOF_OVERLAP_THRESHOLD",
    "check_lpbf_available",
    "classify_parameter_set",
    "generate_printability_map",
    "kou_cracking_index",
    "rosenthal_melt_pool_geometry",
]


def check_lpbf_available() -> bool:
    """Return True only when every LPBF numerical dependency imports."""
    try:
        import numpy  # noqa: F401
        from scipy.special import lambertw  # noqa: F401

        return True
    except Exception:
        return False
