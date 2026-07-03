"""Validate pydantic schemas for every tool's input/output."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from app.tools.simulation.mace.schemas import (
    Composition,
    ComputeDiluteSoluteInput,
    ComputeElasticInput,
    EstimateCostInput,
    JobHandle,
    MdEquilibrateInput,
    PhononHarmonicInput,
    PrimitiveOptions,
    RelaxStructureInput,
    StructureRef,
)


def test_composition_must_sum_to_n_atoms() -> None:
    with pytest.raises(ValidationError):
        RelaxStructureInput(
            composition=Composition(atoms={"Fe": 50, "Ti": 49}),
            phase="bcc",
            n_atoms=100,
        )


def test_composition_unsupported_element() -> None:
    with pytest.raises(ValidationError):
        Composition(atoms={"Xx": 100})


def test_extra_field_rejected() -> None:
    with pytest.raises(ValidationError):
        RelaxStructureInput.model_validate(
            {
                "composition": {"atoms": {"Fe": 50, "Ti": 50}},
                "phase": "bcc",
                "n_atoms": 100,
                "garbage": 7,
            }
        )


def test_relax_happy_path() -> None:
    inp = RelaxStructureInput(
        composition=Composition(atoms={"Fe": 50, "Ti": 50}),
        phase="bcc",
        n_atoms=100,
    )
    assert inp.options.head == "omat_pbe"
    assert inp.fmax_eV_per_A == pytest.approx(0.05)


def test_structure_ref_xor() -> None:
    """Exactly one of (composition+phase) or cache_ref must be present."""
    # composition+phase only — ok
    sr = StructureRef(
        composition=Composition(atoms={"Fe": 50, "Ti": 50}),
        phase="bcc",
        n_atoms=100,
    )
    assert sr.cache_ref is None
    # cache_ref only — ok
    sr2 = StructureRef(cache_ref="cache://abc/structure.cif")
    assert sr2.composition is None
    # Both — error
    with pytest.raises(ValidationError):
        StructureRef(
            composition=Composition(atoms={"Fe": 50, "Ti": 50}),
            phase="bcc",
            cache_ref="cache://abc/structure",
        )
    # Neither — error
    with pytest.raises(ValidationError):
        StructureRef()


def test_compute_elastic_input() -> None:
    ce = ComputeElasticInput(
        structure=StructureRef(cache_ref="cache://abc/structure"),
    )
    assert ce.strain_amplitude == pytest.approx(0.005)


def test_compute_dilute_solute_input() -> None:
    ip = ComputeDiluteSoluteInput(
        matrix_composition=Composition(atoms={"Mo": 25, "Nb": 25, "Ta": 25, "V": 25}),
        matrix_phase="bcc",
        n_atoms=100,
        solute_element="Fe",
    )
    assert ip.displaced_element is None
    with pytest.raises(ValidationError):
        ComputeDiluteSoluteInput(
            matrix_composition=Composition(atoms={"Mo": 100}),
            n_atoms=100,
            solute_element="Xx",
        )


def test_md_input() -> None:
    md = MdEquilibrateInput(
        structure=StructureRef(cache_ref="cache://abc/structure"),
        T_K=1500.0,
    )
    assert md.ensemble == "nvt_langevin"
    assert md.n_steps == 1000


def test_phonon_input_default_temps() -> None:
    ph = PhononHarmonicInput(
        structure=StructureRef(cache_ref="cache://abc/structure"),
    )
    assert ph.q_mesh == (4, 4, 4)
    assert 1000.0 in ph.temperatures_K


def test_job_handle_minimal() -> None:
    jh = JobHandle(job_id="01HXY", status="queued", tool_name="relax_structure")
    assert jh.cache_hit is False
    assert jh.estimated_seconds == 0


def test_estimate_cost_input() -> None:
    ec = EstimateCostInput(
        tool_name="relax_structure",
        arguments={"composition": {"atoms": {"Fe": 50, "Ti": 50}}, "phase": "bcc", "n_atoms": 100},
    )
    assert ec.tool_name == "relax_structure"


def test_primitive_options_backend_default() -> None:
    po = PrimitiveOptions()
    assert po.backend == "auto"
    assert po.dtype == "float64"
    assert po.head == "omat_pbe"


def test_primitive_options_progress_token_alias() -> None:
    po = PrimitiveOptions.model_validate({"progressToken": "tok-1"})
    assert po.progress_token == "tok-1"
