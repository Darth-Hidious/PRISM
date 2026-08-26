"""Agent tool for KWN precipitation kinetics (the TC-PRISMA capability),
wrapped from kawin.

One tool: ``precipitation_kinetics`` — given a TDB, a system definition,
interfacial energies, molar volumes, nucleation sites and a heat-treatment
schedule, it evolves the precipitate size distribution (nucleation +
growth + coarsening) and returns volume fraction, mean radius, number
density, the size distribution and the matrix composition trajectory as
functions of time.

Registration is gated in ``app/plugins/bootstrap.py`` on kawin + pycalphad
being importable (check_precipitation_available); when they are not, this
tool does not appear in the catalog at all.
"""
from __future__ import annotations

from app.tools._extras import missing_extra_error
from app.tools.base import Tool, ToolRegistry


def _missing_dep_error() -> dict:
    """The one missing-dependency shape (app/tools/_extras.py).

    The hand-rolled dict this replaced carried no `requires_extra` for a caller
    to branch on, and its primary `install_hint` named a distribution that is
    not on any index (see `_extras.install_command`).
    """
    return missing_extra_error(
        "precipitation",
        "Precipitation kinetics (KWN) is not available in this PRISM "
        "install: kawin and/or pycalphad cannot be imported.",
        converged=False,
    )


def _precipitation_kinetics(**kwargs) -> dict:
    # Defense in depth: bootstrap only registers this when the deps import,
    # but a direct call (tests, sidecar) must still fail structured.
    from app.tools.materials.precipitation import check_precipitation_available

    if not check_precipitation_available():
        return _missing_dep_error()

    from app.tools.materials.precipitation.kinetics import run_kwn

    return run_kwn(**kwargs)


def _validate_kwn_result(output: dict) -> str:
    """Scientific-validity gate (Tool contract): a solve that did not
    complete is 'invalid' — downstream steps must not consume its numbers.
    A completed solve with a physically degenerate trajectory (e.g. zero
    precipitates everywhere, which can mean the driving force never
    exceeded the nucleation barrier in-window) is downgraded to 'warn'
    rather than presented as a quantitative answer."""
    if not output.get("converged"):
        return "invalid"
    final = output.get("final") or {}
    for phase_out in final.values():
        vf = phase_out.get("volume_fraction")
        nd = phase_out.get("number_density_m3")
        if vf is not None and vf == 0 and nd is not None and nd == 0:
            return "warn"
    return "ok"


_DESCRIPTION = (
    "Simulate precipitation kinetics with the Kampmann-Wagner Numerical "
    "(KWN) model via kawin — the open equivalent of TC-PRISMA. Couples "
    "nucleation, growth and coarsening against a CALPHAD database "
    "(pycalphad) to evolve a precipitate size distribution through a heat "
    "treatment. Inputs: `database` (path to a .tdb file or a name in "
    "~/.prism/databases — nothing is downloaded), `components` (element "
    "symbols, solvent first, e.g. ['AL','ZR']), `matrix_phase` (parent "
    "phase in the TDB, e.g. 'FCC_A1'), `precipitates` (list of {phase, "
    "interfacial_energy_J_per_m2, molar_volume_m3_per_mol, "
    "atoms_per_unit_cell, nucleation_site}), `matrix_composition` "
    "({solute: mole fraction}), `diffusivity` ({D0_m2_per_s, Q_J_per_mol} "
    "for all solutes, or per-solute {EL: {...}}), and EITHER an isothermal "
    "hold (`temperature_K` + `time_s`) OR a `schedule` "
    "({times_s: [...], temperatures_K: [...]}). Also accepts the matrix "
    "molar volume, grain size and dislocation density that set nucleation "
    "site densities. Outputs (all SI, units in the field names): time, "
    "volume fraction, mean radius (m and nm), number density (1/m^3), "
    "nucleation rate, the size distribution and the matrix composition "
    "trajectory. FAILS EXPLICITLY with no numeric keys when inputs are "
    "invalid, the database is missing, or the integration does not "
    "complete — it never invents a precipitate radius."
)

_EXAMPLE_INPUT = {
    "database": "alzr_wang2001.tdb",
    "components": ["AL", "ZR"],
    "matrix_phase": "FCC_A1",
    "precipitates": [{
        "phase": "AL3ZR",
        "interfacial_energy_J_per_m2": 0.05,
        "molar_volume_m3_per_mol": 1.00e-5,
        "atoms_per_unit_cell": 4,
        "nucleation_site": "DISLOCATIONS",
    }],
    "matrix_composition": {"ZR": 0.02},
    "diffusivity": {"D0_m2_per_s": 0.0768, "Q_J_per_mol": 242000.0},
    "temperature_K": 723.15,
    "time_s": 3.6e6,
    "matrix_molar_volume_m3_per_mol": 1.00e-5,
    "matrix_atoms_per_unit_cell": 4,
    "grain_size_um": 1.0,
    "dislocation_density_m_per_m3": 1e15,
}


def create_precipitation_tools(registry: ToolRegistry) -> None:
    """Register the precipitation_kinetics tool. Callers must have verified
    kawin + pycalphad are importable first (check_precipitation_available)."""
    registry.register(Tool(
        name="precipitation_kinetics",
        description=_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "database": {
                    "type": "string",
                    "description": "Path to a TDB thermodynamic database "
                                   "file, or the name of one in "
                                   "~/.prism/databases.",
                },
                "components": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Element symbols, solvent (matrix) "
                                   "element first, e.g. ['AL', 'ZR'].",
                },
                "matrix_phase": {
                    "type": "string",
                    "description": "Parent/matrix phase name as it appears "
                                   "in the TDB, e.g. 'FCC_A1'.",
                },
                "precipitates": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "phase": {"type": "string"},
                            "interfacial_energy_J_per_m2": {"type": "number"},
                            "molar_volume_m3_per_mol": {"type": "number"},
                            "atoms_per_unit_cell": {"type": "integer"},
                            "nucleation_site": {
                                "type": "string",
                                "enum": ["BULK", "DISLOCATIONS",
                                         "GRAIN BOUNDARIES", "GRAIN EDGES",
                                         "GRAIN CORNERS"],
                            },
                        },
                        "required": ["phase", "interfacial_energy_J_per_m2",
                                     "molar_volume_m3_per_mol",
                                     "atoms_per_unit_cell"],
                    },
                    "description": "Precipitate phase definitions. The "
                                   "interfacial energy is required and is "
                                   "never defaulted.",
                },
                "matrix_composition": {
                    "type": "object",
                    "description": "Initial solute mole fractions, e.g. "
                                   "{'ZR': 0.004}. Keys must be the solute "
                                   "elements (all but the first component).",
                },
                "diffusivity": {
                    "type": "object",
                    "description": "Arrhenius diffusivity in the matrix: "
                                   "{D0_m2_per_s, Q_J_per_mol} for all "
                                   "solutes, or per-solute "
                                   "{EL: {D0_m2_per_s, Q_J_per_mol}}.",
                },
                "temperature_K": {
                    "type": "number",
                    "description": "Isothermal hold temperature (K). Use "
                                   "with time_s, or pass schedule instead.",
                },
                "time_s": {
                    "type": "number",
                    "description": "Isothermal hold duration (s).",
                },
                "schedule": {
                    "type": "object",
                    "properties": {
                        "times_s": {"type": "array", "items": {"type": "number"}},
                        "temperatures_K": {"type": "array",
                                           "items": {"type": "number"}},
                    },
                    "required": ["times_s", "temperatures_K"],
                    "description": "Temperature-time profile; alternative "
                                   "to the isothermal hold.",
                },
                "matrix_molar_volume_m3_per_mol": {
                    "type": "number",
                    "description": "Molar volume of the matrix phase "
                                   "(m^3/mol), e.g. ~1e-5 for FCC Al.",
                },
                "matrix_atoms_per_unit_cell": {
                    "type": "integer",
                    "description": "Atoms per matrix unit cell (4 for FCC).",
                },
                "grain_size_um": {
                    "type": "number",
                    "description": "Average grain size (micrometres); sets "
                                   "heterogeneous nucleation site density. "
                                   "Default 100.",
                },
                "dislocation_density_m_per_m3": {
                    "type": "number",
                    "description": "Dislocation density (m of line per m^3) "
                                   "for dislocation nucleation. Default 5e12.",
                },
                "bulk_site_density_m3": {
                    "type": "number",
                    "description": "Optional override of the bulk nucleation "
                                   "site density (1/m^3). Default: derived "
                                   "from composition.",
                },
                "pbm": {
                    "type": "object",
                    "description": "Optional size-class (population balance) "
                                   "grid: {cMin, cMax} in metres, {bins, "
                                   "minBins, maxBins} integers.",
                },
            },
            "required": ["database", "components", "matrix_phase",
                         "precipitates", "matrix_composition", "diffusivity",
                         "matrix_molar_volume_m3_per_mol",
                         "matrix_atoms_per_unit_cell"],
            "additionalProperties": False,
        },
        func=_precipitation_kinetics,
        output_schema={
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": ["ok", "failed"]},
                "converged": {"type": "boolean"},
                "n_steps": {"type": "integer"},
                "time_s": {"type": "array", "items": {"type": "number"}},
                "temperature_K": {"type": "array", "items": {"type": "number"}},
                "phases": {
                    "type": "object",
                    "description": "Per precipitate phase: volume_fraction, "
                                   "mean_radius_m, mean_radius_nm, "
                                   "number_density_m3, nucleation_rate_m3_s, "
                                   "critical_radius_m, "
                                   "driving_force_J_per_mol trajectories.",
                },
                "matrix_composition": {
                    "type": "object",
                    "description": "Solute mole-fraction trajectories.",
                },
                "size_distribution": {
                    "type": "object",
                    "description": "Final PSD per phase: radius_m bin "
                                   "centres, number_density_per_m3 per bin.",
                },
                "final": {"type": "object"},
            },
        },
        units={
            "time_s": "EMMO:second",
            "temperature_K": "EMMO:kelvin",
            "phases.volume_fraction": "EMMO:dimensionless",
            "phases.mean_radius_m": "EMMO:metre",
            "phases.mean_radius_nm": "EMMO:nanometre",
            "phases.number_density_m3": "EMMO:number-density",
            "phases.nucleation_rate_m3_s": "EMMO:number-per-metre-cubed-per-second",
            "phases.critical_radius_m": "EMMO:metre",
            "phases.driving_force_J_per_mol": "EMMO:joule-per-mole",
            "matrix_composition": "EMMO:mole-fraction",
            "size_distribution.radius_m": "EMMO:metre",
            "size_distribution.number_density_per_m3": "EMMO:number-density",
        },
        examples=[{
            # Values below are real output of this exact input: Al-2at%Zr,
            # Wang/Jin/Zhao 2001 TDB, 1000 h at 723.15 K (17330 adaptive
            # steps; ~85 s wall on a 12-core arm64 laptop).
            "input": _EXAMPLE_INPUT,
            "output": {
                "status": "ok",
                "converged": True,
                "n_steps": 17330,
                "time_s": [0.0, 3600000.0],
                "temperature_K": [723.15, 723.15],
                "phases": {
                    "AL3ZR": {
                        "volume_fraction": [0.0, 0.08072],
                        "mean_radius_m": [0.0, 8.669e-09],
                        "mean_radius_nm": [0.0, 8.669],
                        "number_density_m3": [0.0, 2.129e22],
                    },
                },
                "matrix_composition": {"ZR": [0.02, 0.0]},
            },
        }],
        validate=_validate_kwn_result,
    ))
