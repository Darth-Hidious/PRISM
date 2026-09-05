"""The MACE tier accepted ten elements. The model covers the periodic table.

Measured 2026-09-02 in a live research session: `evaluate_candidate`'s MACE
wrapper refused Ni/Co/Cr, so a γ′-superalloy proxy was relaxed by hand in the
notebook kernel instead. The gate was not the model — MACE-MP-0 and MACE-MH-1
are trained on the whole periodic table — but a ten-row hand table of starting
lattice parameters for supercell construction.

The tables are now derived from ASE's experimental reference states for every
element whose ground state is a simple metal lattice (fcc, bcc, hcp). A phase
the element does not adopt in nature is estimated at equal atomic volume — a
STARTING GUESS for relaxation, which is all the tables ever were. Elements with
complex ground states (α-Mn, diamond Si, bct Sn, …) stay unsupported, with an
honest message.
"""

import math

import pytest

from app.tools.simulation.mace.core import lattices
from app.tools.simulation.mace.schemas import ALLOWED_ELEMENTS, Composition


def test_the_original_ten_keep_their_hand_values():
    """Existing runs and their tests must not shift under the widening."""
    assert lattices.A_BCC["Fe"] == pytest.approx(2.87)
    assert lattices.A_BCC["W"] == pytest.approx(3.16)
    assert lattices.A_FCC["Al"] == pytest.approx(4.05)
    assert lattices.A_HCP["Ti"] == pytest.approx(2.95)


def test_common_structural_metals_are_supported():
    for element in ["Ni", "Co", "Cr", "Cu", "Re", "Mg", "Pt", "Ag", "Au", "Pd", "Ir", "Ru", "Os", "Rh", "Pb", "Ca", "Sc", "Y"]:
        assert element in lattices.supported_elements(), element
        for phase in lattices.PHASES:
            a = lattices.lookup_a(element, phase)
            assert 2.0 < a < 6.5, (element, phase, a)


def test_derived_values_agree_with_the_reference_structure():
    """ASE's own lattice constant is used for the element's ground state."""
    assert lattices.lookup_a("Ni", "fcc") == pytest.approx(3.52, abs=0.01)
    assert lattices.lookup_a("Cr", "bcc") == pytest.approx(2.88, abs=0.01)
    assert lattices.lookup_a("Cu", "fcc") == pytest.approx(3.61, abs=0.01)
    # A foreign phase is an equal-volume estimate: Ni bcc from Ni fcc.
    v_atom = 3.52**3 / 4
    assert lattices.lookup_a("Ni", "bcc") == pytest.approx((2 * v_atom) ** (1 / 3), rel=0.01)


def test_complex_ground_states_are_refused_honestly():
    for element in ["Si", "Mn", "Sn", "C", "S"]:
        assert element not in lattices.supported_elements(), element


def test_the_schema_gate_is_the_lattice_table_not_a_second_list():
    assert ALLOWED_ELEMENTS == frozenset(lattices.supported_elements())
    Composition(atoms={"Ni": 60, "Co": 20, "Cr": 20})  # must validate
    with pytest.raises(ValueError):
        Composition(atoms={"Si": 64})


def test_a_superalloy_proxy_builds_an_fcc_cell():
    a = lattices.avg_a({"Ni": 60, "Co": 20, "Cr": 20}, "fcc")
    assert 3.5 < a < 3.7, a
    assert math.isfinite(a)
