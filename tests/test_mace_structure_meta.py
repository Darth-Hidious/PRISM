"""A relaxed structure in the MACE cache must say what it is.

Seen live 2026-09-05 in the Structures tab: "◇ unknown · 100 atoms · unknown ·
unknown" for a cell the agent had just relaxed. The runner wrote tool_name,
composition (when the INPUT carried one) and n_atoms, but never the formula
of the cell it had in hand — and a job whose input was a cache reference
carried no composition at all.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from app.tools.simulation.mace.jobs.runner import _formula_from_cif, _structure_meta

BCC_FE = """data_Fe
_cell_length_a 2.8665
_cell_length_b 2.8665
_cell_length_c 2.8665
_cell_angle_alpha 90
_cell_angle_beta 90
_cell_angle_gamma 90
_symmetry_space_group_name_H-M 'P 1'
loop_
_atom_site_label
_atom_site_type_symbol
_atom_site_fract_x
_atom_site_fract_y
_atom_site_fract_z
Fe1 Fe 0.0 0.0 0.0
Fe2 Fe 0.5 0.5 0.5
"""


def test_the_formula_is_read_off_the_cell_itself():
    assert _formula_from_cif(BCC_FE) == "Fe2"
    assert _formula_from_cif("not a cif") is None
    assert _formula_from_cif(None) is None


def test_the_meta_names_the_structure_even_when_the_input_did_not():
    meta = _structure_meta(
        tool_name="mace_relax_structure",
        job_id="job-1",
        head="omat_pbe",
        input_payload={"structure_ref": "cache://abc/structure.cif", "options": {}},
        cif_text=BCC_FE,
    )
    assert meta["tool_name"] == "mace_relax_structure"
    assert meta["formula"] == "Fe2", meta
    assert meta["n_atoms"] == 2, "counted from the cell when the input did not say"
    # An input that names its composition keeps it; the formula is still the cell's.
    meta2 = _structure_meta(
        tool_name="mace_relax_structure",
        job_id="job-2",
        head="omat_pbe",
        input_payload={"composition": {"atoms": {"Fe": 100}}, "n_atoms": 100},
        cif_text=None,
    )
    assert meta2["n_atoms"] == 100
    assert "formula" not in meta2, "no cell, no invented formula"
