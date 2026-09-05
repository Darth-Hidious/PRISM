"""PRISM scientific-tool wrappers for LPBF process screens."""

from __future__ import annotations
from app.tools.evidence import EvidenceSource, stamp_evidence

import math

from app.tools._extras import missing_extra_error
from app.tools.base import Tool, ToolRegistry
from app.tools.manufacturing.lpbf import check_lpbf_available
from app.tools.manufacturing.lpbf.physics import (
    DEFAULT_BALLING_LENGTH_TO_WIDTH_THRESHOLD,
    DEFAULT_KEYHOLE_ENTHALPY_THRESHOLD,
    DEFAULT_LOF_OVERLAP_THRESHOLD,
    generate_printability_map,
    kou_cracking_index,
)


def _lpbf_missing_error() -> dict:
    """Stable structured failure for direct calls outside gated bootstrap.

    Built from `app/tools/_extras.py` so this gate carries `requires_extra` and
    an `install_hint` that resolves: the hand-rolled dict this replaced named
    `pip install 'prism-platform[lpbf]'` as its primary hint, and that
    distribution is not on any index (see `_extras.install_command`).
    """
    return missing_extra_error(
        "lpbf",
        "LPBF scientific dependencies are not available in this PRISM install.",
        missing_capability="LPBF printability and Kou cracking analysis",
    )


# The thermophysical inputs whose provenance decides the result's evidence
# class. Process settings (powers, hatch, layer, beam, preheat) are the
# caller's own choices, not facts about a material.
_THERMOPHYSICAL_PROPERTIES = (
    "solidus_temperature_k",
    "liquidus_temperature_k",
    "thermal_conductivity_w_mk",
    "density_kg_m3",
    "specific_heat_j_kgk",
    "latent_heat_j_kg",
    "absorptivity",
)


def _run_printability_map(**kwargs) -> dict:
    if not check_lpbf_available():
        return _lpbf_missing_error()
    # Where the numbers came from decides what the map is worth. Live on
    # 2026-09-02 the screen ran on properties the model supplied from memory
    # and came back with no evidence class at all. The tool cannot know the
    # numbers; it can know whether each arrived with a source, and say so.
    sources = kwargs.pop("property_sources", None) or {}
    out = generate_printability_map(**kwargs)
    if not isinstance(out, dict) or "error" in out:
        return out
    unsourced = [
        name
        for name in _THERMOPHYSICAL_PROPERTIES
        if not str(sources.get(name, "")).strip()
    ]
    stamp_evidence(
        out,
        EvidenceSource.MODEL_ASSERTION if unsourced else EvidenceSource.LITERATURE_EXTRACTION,
    )
    out["property_sources"] = {k: v for k, v in sources.items() if str(v).strip()}
    out["unsourced_properties"] = unsourced
    if unsourced:
        out["evidence_note"] = (
            "screening over unsourced inputs: "
            + ", ".join(unsourced)
            + " arrived without a source. Pass property_sources={name: citation} "
            "for datasheet or measured values; the class rises to research."
        )
    return out


def _run_kou_index(**kwargs) -> dict:
    if not check_lpbf_available():
        return _lpbf_missing_error()
    return kou_cracking_index(**kwargs)


def _validate_printability(output: dict) -> str:
    if output.get("status") != "ok" or not output.get("points"):
        return "invalid"
    for point in output["points"]:
        pool = point.get("melt_pool", {}).get("liquidus", {})
        if not all(
            math.isfinite(float(pool.get(field, math.nan)))
            and float(pool[field]) > 0.0
            for field in ("width_m", "depth_m", "length_m")
        ):
            return "invalid"
    # A mathematically valid Rosenthal screen still warrants a warning before
    # downstream use because it is not a finite-beam or fluid-flow calculation.
    return "warn"


def _validate_kou(output: dict) -> str:
    index = output.get("kou_index_k")
    if output.get("status") != "ok" or index is None:
        return "invalid"
    if not math.isfinite(float(index)) or float(index) < 0.0:
        return "invalid"
    return "warn" if output.get("n_terminal_intervals", 0) < 2 else "ok"


def _positive_number(description: str) -> dict:
    return {"type": "number", "exclusiveMinimum": 0, "description": description}


_PRINTABILITY_PROPERTIES = {
    "powers_w": {
        "type": "array",
        "items": {"type": "number", "exclusiveMinimum": 0},
        "minItems": 1,
        "description": "Laser powers forming the P-grid axis, in W.",
    },
    "scan_velocities_m_per_s": {
        "type": "array",
        "items": {"type": "number", "exclusiveMinimum": 0},
        "minItems": 1,
        "description": "Scan velocities forming the v-grid axis, in m/s.",
    },
    "hatch_spacing_m": _positive_number("Caller-supplied hatch spacing, in m."),
    "layer_thickness_m": _positive_number("Caller-supplied layer thickness, in m."),
    "beam_diameter_m": _positive_number(
        "Caller-supplied full characteristic beam diameter in m; the enthalpy "
        "criterion uses radius = diameter/2. State the experimental beam "
        "convention when reporting results."
    ),
    "initial_temperature_k": _positive_number(
        "Caller-supplied initial/preheat temperature, in K."
    ),
    "solidus_temperature_k": _positive_number(
        "Caller-supplied solidus temperature, in K; no alloy default."
    ),
    "liquidus_temperature_k": _positive_number(
        "Caller-supplied liquidus temperature, in K; no alloy default."
    ),
    "thermal_conductivity_w_mk": _positive_number(
        "Caller-supplied constant thermal conductivity, in W/(m K)."
    ),
    "density_kg_m3": _positive_number(
        "Caller-supplied constant density, in kg/m^3."
    ),
    "specific_heat_j_kgk": _positive_number(
        "Caller-supplied constant specific heat, in J/(kg K)."
    ),
    "latent_heat_j_kg": _positive_number(
        "Caller-supplied latent heat of fusion, in J/kg. Used in normalized "
        "enthalpy; omitted from Rosenthal's temperature field by model assumption."
    ),
    "absorptivity": {
        "type": "number",
        "exclusiveMinimum": 0,
        "maximum": 1,
        "description": "Caller-supplied effective laser absorptivity in (0, 1].",
    },
    "property_sources": {
        "type": "object",
        "additionalProperties": {"type": "string"},
        "description": (
            "Where each thermophysical value came from — {property_name: citation}, "
            "e.g. a datasheet, handbook table or measurement. Every one of the seven "
            "sourced → evidence class research; any missing → indeterminate (RED), "
            "with the gaps named. A value recalled from memory has no source."
        ),
    },
    "lof_overlap_threshold": _positive_number(
        "Tunable semi-ellipse overlap boundary. Default 1.0 from the geometric "
        "coverage condition described by Tang, Pistorius, and Beuth (2017)."
    ),
    "keyhole_enthalpy_threshold": _positive_number(
        "Tunable normalized-enthalpy keyhole boundary. Default 6.0 is the "
        "documented King et al. (2014) screening value, not universal."
    ),
    "balling_length_to_width_threshold": _positive_number(
        "Tunable track length/width boundary. Default pi is the ideal "
        "Plateau-Rayleigh cylinder limit."
    ),
}

_PRINTABILITY_REQUIRED = [
    "powers_w",
    "scan_velocities_m_per_s",
    "hatch_spacing_m",
    "layer_thickness_m",
    "beam_diameter_m",
    "initial_temperature_k",
    "solidus_temperature_k",
    "liquidus_temperature_k",
    "thermal_conductivity_w_mk",
    "density_kg_m3",
    "specific_heat_j_kgk",
    "latent_heat_j_kg",
    "absorptivity",
]

_PRINTABILITY_DESCRIPTION = (
    "Generate an LPBF power/velocity printability map using Rosenthal's 3D "
    "moving surface point-source solution and screen lack of fusion, keyholing, "
    "and balling. This is NOT CFD: Rosenthal assumes a point source, constant "
    "properties, no latent heat in its temperature field, conduction-only "
    "transport, and a semi-infinite body. Lack of fusion uses semi-ellipse "
    "melt-pool overlap (Tang, Pistorius & Beuth, Addit. Manuf. 14, 2017, "
    "39-48); keyholing uses normalized enthalpy (King et al., JMPT 214, 2014, "
    "2915-2925); balling uses the ideal Plateau-Rayleigh length/width limit. "
    "Every thermophysical property is required from the caller; no alloy "
    "constants are supplied, and the result's evidence class says whether each "
    "arrived with a source (property_sources) or not. Thresholds are tunable "
    "and require experimental calibration before process qualification."
)

_KOU_DESCRIPTION = (
    "Compute Kou's solidification-cracking susceptibility index, max "
    "|dT/d(sqrt(f_s))| near terminal solidification, directly from a caller-"
    "supplied temperature/solid-fraction path (for example, Scheil output). "
    "No pycalphad dependency is required. Source: S. Kou, Acta Materialia 88 "
    "(2015) 366-374, doi:10.1016/j.actamat.2015.01.034. Larger values imply "
    "greater comparative susceptibility; no universal pass/fail threshold is "
    "invented. terminal_solid_fraction_min is an exposed numerical window."
)


def create_lpbf_tools(registry: ToolRegistry) -> None:
    """Register LPBF tools after :func:`check_lpbf_available` succeeds."""
    registry.register(
        Tool(
            name="lpbf_printability_map",
            description=_PRINTABILITY_DESCRIPTION,
            input_schema={
                "type": "object",
                "properties": _PRINTABILITY_PROPERTIES,
                "required": _PRINTABILITY_REQUIRED,
                "additionalProperties": False,
            },
            func=_run_printability_map,
            output_schema={
                "type": "object",
                "required": ["status", "model", "grid_shape", "points"],
                "properties": {
                    "status": {"type": "string", "enum": ["ok"]},
                    "model": {"type": "string"},
                    "grid_shape": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "minItems": 2,
                        "maxItems": 2,
                    },
                    "powers_w": {"type": "array", "items": {"type": "number"}},
                    "scan_velocities_m_per_s": {
                        "type": "array",
                        "items": {"type": "number"},
                    },
                    "regime_grid": {"type": "array"},
                    "points": {"type": "array", "items": {"type": "object"}},
                    "limitations": {
                        "type": "array",
                        "items": {"type": "string"},
                    },
                },
            },
            units={
                "powers_w": "QUDT:W",
                "scan_velocities_m_per_s": "QUDT:M-PER-SEC",
                "points.melt_pool.liquidus.width_m": "QUDT:M",
                "points.melt_pool.liquidus.depth_m": "QUDT:M",
                "points.melt_pool.liquidus.length_m": "QUDT:M",
                "points.metrics.deposited_enthalpy_density_j_per_m3": (
                    "QUDT:J-PER-M3"
                ),
                "points.metrics.melting_enthalpy_density_j_per_m3": (
                    "QUDT:J-PER-M3"
                ),
            },
            examples=[
                {
                    "description": (
                        "Dimensionally consistent synthetic verification case; "
                        "these are not reference properties for any alloy."
                    ),
                    "input": {
                        "powers_w": [20.0],
                        "scan_velocities_m_per_s": [0.1],
                        "hatch_spacing_m": 0.00005,
                        "layer_thickness_m": 0.00002,
                        "beam_diameter_m": 0.0001,
                        "initial_temperature_k": 300.0,
                        "solidus_temperature_k": 1100.0,
                        "liquidus_temperature_k": 1300.0,
                        "thermal_conductivity_w_mk": 10.0,
                        "density_kg_m3": 1000.0,
                        "specific_heat_j_kgk": 1000.0,
                        "latent_heat_j_kg": 100000.0,
                        "absorptivity": 0.5,
                    },
                    # Abbreviated for readability, but every value below is
                    # what this input actually produces — and it carries
                    # `points`, which `output_schema` requires and the validity
                    # gate checks. The previous example stopped at `grid_shape`,
                    # so the tool's own documented output failed both its own
                    # schema and its own gate.
                    "output": {
                        "status": "ok",
                        "model": "Rosenthal 3D moving surface point source",
                        "grid_shape": [1, 1],
                        "points": [
                            {
                                "power_w": 20.0,
                                "scan_velocity_m_per_s": 0.1,
                                "regime": "keyholing",
                                "melt_pool": {
                                    "liquidus": {
                                        "width_m": 2.0790402734371954e-04,
                                        "depth_m": 1.0395201367185977e-04,
                                        "length_m": 2.3425741587390116e-04,
                                    }
                                },
                                "metrics": {
                                    "normalized_enthalpy": 8.184693783246416,
                                    "deposited_enthalpy_density_j_per_m3": (
                                        9003163161.571058
                                    ),
                                },
                            }
                        ],
                    },
                }
            ],
            validate=_validate_printability,
        )
    )

    registry.register(
        Tool(
            name="lpbf_kou_cracking_index",
            description=_KOU_DESCRIPTION,
            input_schema={
                "type": "object",
                "properties": {
                    "temperatures_k": {
                        "type": "array",
                        "items": {"type": "number", "exclusiveMinimum": 0},
                        "minItems": 2,
                        "description": (
                            "Non-increasing temperatures in K along solidification."
                        ),
                    },
                    "solid_fractions": {
                        "type": "array",
                        "items": {"type": "number", "minimum": 0, "maximum": 1},
                        "minItems": 2,
                        "description": "Strictly increasing solid fractions.",
                    },
                    "terminal_solid_fraction_min": {
                        "type": "number",
                        "minimum": 0,
                        "exclusiveMaximum": 1,
                        "default": 0.9,
                        "description": (
                            "Start of the terminal window. Default 0.9 is an "
                            "exposed numerical choice, not a material constant."
                        ),
                    },
                },
                "required": ["temperatures_k", "solid_fractions"],
                "additionalProperties": False,
            },
            func=_run_kou_index,
            output_schema={
                "type": "object",
                "required": [
                    "status",
                    "kou_index_k",
                    "critical_interval",
                    "terminal_intervals",
                ],
                "properties": {
                    "status": {"type": "string", "enum": ["ok"]},
                    "kou_index_k": {"type": "number", "minimum": 0},
                    "terminal_solid_fraction_min": {"type": "number"},
                    "critical_interval": {"type": "object"},
                    "terminal_intervals": {
                        "type": "array",
                        "items": {"type": "object"},
                    },
                    "n_terminal_intervals": {"type": "integer", "minimum": 1},
                    "interpretation": {"type": "string"},
                },
            },
            units={
                "kou_index_k": "QUDT:K",
                "critical_interval.dT_d_sqrt_fs_k": "QUDT:K",
            },
            examples=[
                {
                    "description": (
                        "Synthetic linear T(sqrt(f_s)) path with exact slope "
                        "-200 K; not material reference data."
                    ),
                    "input": {
                        "temperatures_k": [820.0, 810.0, 800.0],
                        "solid_fractions": [0.81, 0.9025, 1.0],
                        "terminal_solid_fraction_min": 0.81,
                    },
                    "output": {"status": "ok", "kou_index_k": 200.0},
                }
            ],
            validate=_validate_kou,
        )
    )
