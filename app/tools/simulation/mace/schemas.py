"""Pydantic schemas for every MCP tool's input and output.

Convention:
  - ``*Input``  : tool-call parameters
  - ``*Result`` : the resolved result returned via ``get_job`` once a job
                  has reached ``succeeded`` state
  - ``JobHandle``: what the primitive tools return synchronously
                  (the LLM polls ``get_job(job_id)`` for the result)

All compositions are ``{element: atom_count}`` integer maps. Compositions
must sum to ``n_atoms``. Element symbols must be in
:func:`mace_core.lattices.supported_elements`.

Heads correspond to the foundation-MLIP heads shipped with mace-mh-1.
"""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator

# ---------------------------------------------------------------------------
# Shared enums + types
# ---------------------------------------------------------------------------

Head = Literal[
    "omat_pbe",
    "matpes_r2scan",
    "oc20_usemppbe",
    "omol",
    "spice_wB97M",
    "rgd1_b3lyp",
]

Phase = Literal["bcc", "fcc", "hcp", "c14_laves"]
Backend = Literal["hf_jobs", "local", "fake", "auto", "cache"]
JobStatus = Literal[
    "queued",
    "submitted",
    "running",
    "succeeded",
    "failed",
    "cancelling",
    "cancelled",
]
DevicePref = Literal["auto", "gpu", "cpu"]
Dtype = Literal["float32", "float64"]


# Allowed alloying elements — defines schema-level validity for compositions.
# Mirrors mace_core.lattices.A_BCC (the widest table).
ALLOWED_ELEMENTS = frozenset(
    {"Al", "Fe", "Hf", "Mo", "Nb", "Ta", "Ti", "V", "W", "Zr"}
)


class _Base(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)


# ---------------------------------------------------------------------------
# Shared composition + structure-reference types
# ---------------------------------------------------------------------------

class Composition(_Base):
    """``{element: atom_count}`` map. Must sum to ``n_atoms`` in the caller."""

    atoms: dict[str, int] = Field(
        ..., description="Element symbol -> integer atom count.", min_length=1
    )

    @field_validator("atoms")
    @classmethod
    def _check_symbols(cls, v: dict[str, int]) -> dict[str, int]:
        bad = set(v) - ALLOWED_ELEMENTS
        if bad:
            raise ValueError(
                f"unsupported element(s) {sorted(bad)}; "
                f"supported: {sorted(ALLOWED_ELEMENTS)}"
            )
        for el, c in v.items():
            if c < 0:
                raise ValueError(f"{el}: count must be >= 0")
        return {el: int(c) for el, c in v.items() if c > 0}

    def total(self) -> int:
        return sum(self.atoms.values())


class StructureRef(_Base):
    """Either an inline composition+phase, or a cache reference."""

    composition: Composition | None = None
    phase: Phase | None = None
    n_atoms: int = Field(100, ge=8, le=432)
    cache_ref: str | None = Field(
        None,
        description="cache:// URI returned by an earlier tool; if set, overrides "
        "composition/phase/n_atoms.",
    )

    @model_validator(mode="after")
    def _either_or(self) -> "StructureRef":
        has_inline = self.composition is not None and self.phase is not None
        has_ref = self.cache_ref is not None
        if not (has_inline ^ has_ref):
            raise ValueError(
                "StructureRef requires exactly one of "
                "(composition + phase) or cache_ref"
            )
        if has_inline and self.composition.total() != self.n_atoms:
            raise ValueError(
                f"composition sums to {self.composition.total()}, "
                f"expected n_atoms={self.n_atoms}"
            )
        return self


# ---------------------------------------------------------------------------
# Common primitive options
# ---------------------------------------------------------------------------

class PrimitiveOptions(_Base):
    head: Head = "omat_pbe"
    seed: int = 20260506
    backend: Backend = "auto"
    device_preference: DevicePref = "auto"
    dtype: Dtype = "float64"
    progress_token: str | int | None = Field(
        None,
        alias="progressToken",
        description="Echoed in MCP notifications/progress messages.",
    )
    timeout_seconds: int = Field(3600, ge=60, le=14400)


# ---------------------------------------------------------------------------
# JobHandle — what every primitive returns synchronously
# ---------------------------------------------------------------------------

class JobHandle(_Base):
    job_id: str
    status: JobStatus = "queued"
    tool_name: str
    estimated_seconds: int = 0
    estimated_usd: float = 0.0
    cache_hit: bool = False
    # Populated only when cache_hit=True (skip the poll loop).
    result: Any | None = None
    provenance_ref: str | None = None
    cache_key: str | None = None


# ---------------------------------------------------------------------------
# Tool 1: relax_structure
# ---------------------------------------------------------------------------

class RelaxStructureInput(_Base):
    composition: Composition
    phase: Phase = "bcc"
    n_atoms: int = Field(100, ge=8, le=432)
    fmax_eV_per_A: float = Field(0.05, gt=0.0, le=0.5)
    max_steps: int = Field(200, ge=10, le=2000)
    options: PrimitiveOptions = Field(default_factory=PrimitiveOptions)

    @model_validator(mode="after")
    def _sum_check(self) -> "RelaxStructureInput":
        if self.composition.total() != self.n_atoms:
            raise ValueError(
                f"composition sums to {self.composition.total()}, "
                f"expected n_atoms={self.n_atoms}"
            )
        return self


class RelaxStructureResult(_Base):
    energy_per_atom_eV: float
    volume_per_atom_A3: float
    lattice_a_eff_A: float
    n_steps: int
    fmax_final_eV_per_A: float
    structure_cif_ref: str
    head: Head
    wall_time_s: float
    provenance_ref: str


# ---------------------------------------------------------------------------
# Tool 2: compute_elastic
# ---------------------------------------------------------------------------

class ComputeElasticInput(_Base):
    structure: StructureRef
    strain_amplitude: float = Field(0.005, gt=0.0, le=0.05)
    options: PrimitiveOptions = Field(default_factory=PrimitiveOptions)


class ComputeElasticResult(_Base):
    C_GPa: list[list[float]]  # 6x6 stiffness tensor
    K_VRH_GPa: float
    G_VRH_GPa: float
    E_Young_GPa: float
    nu_Poisson: float
    pugh_G_over_B: float
    cauchy_pressure_GPa: float
    pugh_verdict: Literal["ductile", "brittle"]
    am_manufacturability_passed: bool
    wall_time_s: float
    provenance_ref: str


# ---------------------------------------------------------------------------
# Tool 3: compute_dilute_solute
# ---------------------------------------------------------------------------

class ComputeDiluteSoluteInput(_Base):
    matrix_composition: Composition
    matrix_phase: Phase = "bcc"
    n_atoms: int = Field(100, ge=8, le=432)
    solute_element: str
    displaced_element: str | None = Field(
        None,
        description="Element to swap out for the solute. If None, the most "
        "abundant non-solute element is chosen.",
    )
    options: PrimitiveOptions = Field(default_factory=PrimitiveOptions)

    @field_validator("solute_element")
    @classmethod
    def _check_solute(cls, v: str) -> str:
        if v not in ALLOWED_ELEMENTS:
            raise ValueError(f"unsupported solute {v!r}")
        return v

    @field_validator("displaced_element")
    @classmethod
    def _check_displaced(cls, v: str | None) -> str | None:
        if v is not None and v not in ALLOWED_ELEMENTS:
            raise ValueError(f"unsupported displaced element {v!r}")
        return v

    @model_validator(mode="after")
    def _sum_check(self) -> "ComputeDiluteSoluteInput":
        if self.matrix_composition.total() != self.n_atoms:
            raise ValueError(
                f"matrix sums to {self.matrix_composition.total()}, "
                f"expected n_atoms={self.n_atoms}"
            )
        return self


class ComputeDiluteSoluteResult(_Base):
    E_sub_eV: float
    verdict: Literal["favourable", "unfavourable"]
    matrix_E_per_atom_eV: float
    pure_solute_E_per_atom_eV: float
    pure_displaced_E_per_atom_eV: float
    solute_element: str
    displaced_element: str
    wall_time_s: float
    provenance_ref: str


# ---------------------------------------------------------------------------
# Tool 4: md_equilibrate
# ---------------------------------------------------------------------------

class MdEquilibrateInput(_Base):
    structure: StructureRef
    T_K: float = Field(..., gt=0.0, le=5000.0)
    n_steps: int = Field(1000, ge=100, le=200_000)
    timestep_fs: float = Field(1.0, gt=0.0, le=5.0)
    friction_per_fs: float = Field(0.01, gt=0.0, le=1.0)
    ensemble: Literal["nvt_langevin"] = "nvt_langevin"
    sample_every: int | None = Field(None, ge=1)
    options: PrimitiveOptions = Field(default_factory=PrimitiveOptions)


class MdEquilibrateResult(_Base):
    mean_E_per_atom_eV: float
    std_E_per_atom_eV: float
    mean_T_K: float
    final_structure_cif_ref: str
    traj_ref: str
    rdf_r_A: list[float]
    rdf_g: list[float]
    is_dynamically_stable: bool
    wall_time_s: float
    provenance_ref: str


# ---------------------------------------------------------------------------
# Tool 5: phonon_harmonic
# ---------------------------------------------------------------------------

class PhononHarmonicInput(_Base):
    structure: StructureRef
    displacement_A: float = Field(0.01, gt=0.0, le=0.1)
    temperatures_K: list[float] = Field(
        default_factory=lambda: [0.0, 300.0, 1000.0, 1500.0],
        min_length=1,
        max_length=64,
    )
    q_mesh: tuple[int, int, int] = (4, 4, 4)
    options: PrimitiveOptions = Field(default_factory=PrimitiveOptions)


class PhononHarmonicResult(_Base):
    temperatures_K: list[float]
    F_vib_eV_per_atom: list[float]
    n_imaginary_modes: int
    is_dynamically_stable: bool
    phonon_dos_omega_THz: list[float]
    phonon_dos_g: list[float]
    quality_tier: str
    wall_time_s: float
    provenance_ref: str


# ---------------------------------------------------------------------------
# Control plane
# ---------------------------------------------------------------------------

class GetJobInput(_Base):
    job_id: str


class JobProgress(_Base):
    percent: float = 0.0
    message: str = ""
    step: int = 0
    total: int = 0


class JobRecord(_Base):
    job_id: str
    tool_name: str
    status: JobStatus
    backend: Backend | None = None
    progress: JobProgress = Field(default_factory=JobProgress)
    result: Any | None = None
    error: dict[str, Any] | None = None
    started_at: str | None = None
    finished_at: str | None = None
    hf_job_id: str | None = None
    hf_job_url: str | None = None
    cache_key: str | None = None
    provenance_ref: str | None = None
    summary_input: dict[str, Any] | None = None


class CancelJobInput(_Base):
    job_id: str


class CancelJobResult(_Base):
    job_id: str
    status: JobStatus
    message: str = ""


class ListJobsInput(_Base):
    limit: int = Field(50, ge=1, le=500)
    status_filter: JobStatus | None = None
    since_iso8601: str | None = None


class ListJobsResult(_Base):
    jobs: list[JobRecord]


class EstimateCostInput(_Base):
    tool_name: Literal[
        "relax_structure",
        "compute_elastic",
        "compute_dilute_solute",
        "md_equilibrate",
        "phonon_harmonic",
    ]
    arguments: dict[str, Any] = Field(
        ..., description="Raw tool-call arguments — same shape as you'd pass directly."
    )


class EstimateCostResult(_Base):
    estimated_wall_seconds: int
    estimated_gpu_seconds: int
    estimated_usd: float
    backend_recommended: Backend
    cache_hit: bool
    notes: str = ""


class GetCachedStructureInput(_Base):
    cache_ref: str = Field(..., description="cache:// URI from an earlier tool.")


class GetCachedStructureResult(_Base):
    cif_text: str
    n_atoms: int
    composition: dict[str, int]
    phase: Phase | None = None
    head: Head | None = None
    source_job_id: str | None = None
    created_at: str | None = None
