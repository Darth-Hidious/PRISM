"""Approximate physics screens for laser powder bed fusion (LPBF).

The melt-pool calculation uses Rosenthal's three-dimensional moving point-
source solution, not CFD and not the finite-Gaussian Eagar--Tsai model.  For a
surface point source on a semi-infinite body the temperature rise is

    T - T0 = eta P / (2 pi k R) * exp[-v (x + R) / (2 alpha)]

with ``alpha = k / (rho Cp)``.  The implementation assumes a steady source,
constant isotropic properties, conduction-only heat transfer, a point beam,
no latent heat in the temperature field, and a semi-infinite fully dense body.
It therefore does not model powder-scale absorption, evaporation, recoil
pressure, Marangoni flow, free-surface motion, or keyhole hydrodynamics.  The
beam diameter and latent heat are used by the normalized-enthalpy screen, not
silently folded into the Rosenthal field.

Source for the moving-source solution and its assumptions:
D. Rosenthal, "The Theory of Moving Sources of Heat and Its Application to
Metal Treatments," Transactions of the ASME 68 (1946) 849--866.
"""

from __future__ import annotations

import math
from typing import Sequence

# Defect-screen defaults are intentionally public and caller-tunable.
#
# Tang, Pistorius, and Beuth use melt-pool overlap geometry to predict
# lack-of-fusion porosity: M. Tang, P. C. Pistorius, and J. L. Beuth,
# "Prediction of lack-of-fusion porosity for powder bed fusion," Additive
# Manufacturing 14 (2017) 39--48, doi:10.1016/j.addma.2016.12.001.  The
# semi-ellipse coverage condition adopted here has a unit boundary.
DEFAULT_LOF_OVERLAP_THRESHOLD = 1.0

# King et al. formulate keyhole onset using normalized deposited enthalpy and
# relate the transition to pi*T_b/T_m (approximately 6 for their reported
# Ti-6Al-4V case).  Six is retained only as a documented screening default;
# it is not universal and callers should calibrate it for their material,
# beam-radius convention, and enthalpy convention.  W. E. King et al.,
# "Observation of keyhole-mode laser melting in laser powder-bed fusion
# additive manufacturing," Journal of Materials Processing Technology 214
# (2014) 2915--2925, doi:10.1016/j.jmatprotec.2014.06.005.
DEFAULT_KEYHOLE_ENTHALPY_THRESHOLD = 6.0

# The inviscid Plateau--Rayleigh cylinder becomes unstable for wavelengths
# longer than its circumference, lambda / diameter > pi.  This ideal-cylinder
# value is a screen, not an alloy fit.  Lord Rayleigh, "On the instability of
# jets," Proceedings of the London Mathematical Society s1-10 (1878) 4--13,
# doi:10.1112/plms/s1-10.1.4.
DEFAULT_BALLING_LENGTH_TO_WIDTH_THRESHOLD = math.pi


def _positive_finite(name: str, value: float) -> float:
    number = float(value)
    if not math.isfinite(number) or number <= 0.0:
        raise ValueError(f"{name} must be finite and > 0")
    return number


def _validate_material_inputs(
    *,
    power_w: float,
    scan_velocity_m_per_s: float,
    initial_temperature_k: float,
    solidus_temperature_k: float,
    liquidus_temperature_k: float,
    thermal_conductivity_w_mk: float,
    density_kg_m3: float,
    specific_heat_j_kgk: float,
    latent_heat_j_kg: float,
    absorptivity: float,
) -> dict[str, float]:
    values = {
        "power_w": _positive_finite("power_w", power_w),
        "scan_velocity_m_per_s": _positive_finite(
            "scan_velocity_m_per_s", scan_velocity_m_per_s
        ),
        "initial_temperature_k": _positive_finite(
            "initial_temperature_k", initial_temperature_k
        ),
        "solidus_temperature_k": _positive_finite(
            "solidus_temperature_k", solidus_temperature_k
        ),
        "liquidus_temperature_k": _positive_finite(
            "liquidus_temperature_k", liquidus_temperature_k
        ),
        "thermal_conductivity_w_mk": _positive_finite(
            "thermal_conductivity_w_mk", thermal_conductivity_w_mk
        ),
        "density_kg_m3": _positive_finite("density_kg_m3", density_kg_m3),
        "specific_heat_j_kgk": _positive_finite(
            "specific_heat_j_kgk", specific_heat_j_kgk
        ),
        "latent_heat_j_kg": _positive_finite(
            "latent_heat_j_kg", latent_heat_j_kg
        ),
    }
    eta = float(absorptivity)
    if not math.isfinite(eta) or not 0.0 < eta <= 1.0:
        raise ValueError("absorptivity must be finite and in (0, 1]")
    values["absorptivity"] = eta

    if values["solidus_temperature_k"] <= values["initial_temperature_k"]:
        raise ValueError("solidus_temperature_k must exceed initial_temperature_k")
    if values["liquidus_temperature_k"] < values["solidus_temperature_k"]:
        raise ValueError(
            "liquidus_temperature_k must be >= solidus_temperature_k"
        )
    return values


def _rosenthal_isotherm_geometry(
    *, amplitude_k_m: float,
    advection_per_m: float,
    temperature_rise_k: float,
) -> dict[str, float]:
    """Return exact Rosenthal isotherm extents using the Lambert W function."""
    from scipy.special import lambertw

    argument = advection_per_m * amplitude_k_m / temperature_rise_k
    transverse_radius = float(lambertw(argument).real / advection_per_m)
    rear_length = amplitude_k_m / temperature_rise_k
    front_length = float(
        lambertw(2.0 * argument).real / (2.0 * advection_per_m)
    )
    values = {
        "width_m": 2.0 * transverse_radius,
        # In the isotropic surface-point-source solution the transverse
        # isotherm is circular; only its lower half lies in the body.
        "depth_m": transverse_radius,
        "length_m": rear_length + front_length,
        "rear_length_m": rear_length,
        "front_length_m": front_length,
    }
    if not all(math.isfinite(value) and value > 0.0 for value in values.values()):
        raise ValueError("Rosenthal isotherm is non-finite for these inputs")
    return values


def rosenthal_melt_pool_geometry(
    *,
    power_w: float,
    scan_velocity_m_per_s: float,
    initial_temperature_k: float,
    solidus_temperature_k: float,
    liquidus_temperature_k: float,
    thermal_conductivity_w_mk: float,
    density_kg_m3: float,
    specific_heat_j_kgk: float,
    latent_heat_j_kg: float,
    absorptivity: float,
) -> dict:
    """Calculate solidus and liquidus isotherm dimensions with Rosenthal.

    This is the analytical point-source model stated in the module docstring.
    It is valid only when the constant-property, steady, conduction-dominated,
    semi-infinite approximation is defensible.  It is not a finite-beam model
    and is not a CFD prediction.  ``latent_heat_j_kg`` is required so the same
    explicit material record can feed the enthalpy criterion, but Rosenthal's
    temperature solution omits latent heat by assumption.
    """
    values = _validate_material_inputs(
        power_w=power_w,
        scan_velocity_m_per_s=scan_velocity_m_per_s,
        initial_temperature_k=initial_temperature_k,
        solidus_temperature_k=solidus_temperature_k,
        liquidus_temperature_k=liquidus_temperature_k,
        thermal_conductivity_w_mk=thermal_conductivity_w_mk,
        density_kg_m3=density_kg_m3,
        specific_heat_j_kgk=specific_heat_j_kgk,
        latent_heat_j_kg=latent_heat_j_kg,
        absorptivity=absorptivity,
    )
    alpha = values["thermal_conductivity_w_mk"] / (
        values["density_kg_m3"] * values["specific_heat_j_kgk"]
    )
    amplitude = (
        values["absorptivity"]
        * values["power_w"]
        / (2.0 * math.pi * values["thermal_conductivity_w_mk"])
    )
    beta = values["scan_velocity_m_per_s"] / (2.0 * alpha)

    def geometry_at(temperature_k: float) -> dict[str, float]:
        return _rosenthal_isotherm_geometry(
            amplitude_k_m=amplitude,
            advection_per_m=beta,
            temperature_rise_k=temperature_k
            - values["initial_temperature_k"],
        )

    return {
        "model": "Rosenthal 3D moving surface point source",
        "thermal_diffusivity_m2_per_s": alpha,
        "solidus": geometry_at(values["solidus_temperature_k"]),
        "liquidus": geometry_at(values["liquidus_temperature_k"]),
        "latent_heat_used_in_temperature_field": False,
    }


def classify_parameter_set(
    *,
    power_w: float,
    scan_velocity_m_per_s: float,
    hatch_spacing_m: float,
    layer_thickness_m: float,
    beam_diameter_m: float,
    initial_temperature_k: float,
    solidus_temperature_k: float,
    liquidus_temperature_k: float,
    thermal_conductivity_w_mk: float,
    density_kg_m3: float,
    specific_heat_j_kgk: float,
    latent_heat_j_kg: float,
    absorptivity: float,
    lof_overlap_threshold: float = DEFAULT_LOF_OVERLAP_THRESHOLD,
    keyhole_enthalpy_threshold: float = DEFAULT_KEYHOLE_ENTHALPY_THRESHOLD,
    balling_length_to_width_threshold: float = (
        DEFAULT_BALLING_LENGTH_TO_WIDTH_THRESHOLD
    ),
) -> dict:
    """Classify one LPBF point with three approximate defect screens.

    Lack of fusion
        The liquidus cross-section is treated as a semi-ellipse.  Coverage at
        the midpoint between tracks is insufficient when
        ``(hatch/width)^2 + (layer/depth)^2 >= lof_overlap_threshold``.  The
        default unit boundary follows the geometric-overlap approach of Tang,
        Pistorius, and Beuth (2017), cited beside the public default above.

    Keyholing
        ``delta_H / h_s`` is compared with a caller-tunable threshold, where
        ``delta_H = eta*P / (pi*sqrt(alpha*v*r_b^3))`` and
        ``h_s = rho*(Cp*(T_liquidus-T0) + L_f)``.  This is an explicit
        sensible-plus-latent enthalpy adaptation of the normalized-enthalpy
        screen from King et al. (2014), cited above.  The default 6.0 is a
        literature screening value, not a universal transition constant.

    Balling
        Rosenthal liquidus length/width is compared with the ideal
        Plateau--Rayleigh limit pi, cited above.  A supported melt track is not
        an inviscid free cylinder, so the threshold is exposed for calibration.

    Multiple booleans may be true.  ``regime`` preserves overlaps rather than
    hiding them behind a priority order.
    """
    hatch = _positive_finite("hatch_spacing_m", hatch_spacing_m)
    layer = _positive_finite("layer_thickness_m", layer_thickness_m)
    diameter = _positive_finite("beam_diameter_m", beam_diameter_m)
    lof_threshold = _positive_finite(
        "lof_overlap_threshold", lof_overlap_threshold
    )
    keyhole_threshold = _positive_finite(
        "keyhole_enthalpy_threshold", keyhole_enthalpy_threshold
    )
    balling_threshold = _positive_finite(
        "balling_length_to_width_threshold", balling_length_to_width_threshold
    )

    geometry = rosenthal_melt_pool_geometry(
        power_w=power_w,
        scan_velocity_m_per_s=scan_velocity_m_per_s,
        initial_temperature_k=initial_temperature_k,
        solidus_temperature_k=solidus_temperature_k,
        liquidus_temperature_k=liquidus_temperature_k,
        thermal_conductivity_w_mk=thermal_conductivity_w_mk,
        density_kg_m3=density_kg_m3,
        specific_heat_j_kgk=specific_heat_j_kgk,
        latent_heat_j_kg=latent_heat_j_kg,
        absorptivity=absorptivity,
    )
    liquidus = geometry["liquidus"]
    overlap_index = (hatch / liquidus["width_m"]) ** 2 + (
        layer / liquidus["depth_m"]
    ) ** 2

    alpha = geometry["thermal_diffusivity_m2_per_s"]
    beam_radius = diameter / 2.0
    deposited_enthalpy_density = float(absorptivity) * float(power_w) / (
        math.pi
        * math.sqrt(alpha * float(scan_velocity_m_per_s) * beam_radius**3)
    )
    melting_enthalpy_density = float(density_kg_m3) * (
        float(specific_heat_j_kgk)
        * (float(liquidus_temperature_k) - float(initial_temperature_k))
        + float(latent_heat_j_kg)
    )
    normalized_enthalpy = deposited_enthalpy_density / melting_enthalpy_density
    length_to_width = liquidus["length_m"] / liquidus["width_m"]

    defects = {
        "lack_of_fusion": overlap_index >= lof_threshold,
        "keyholing": normalized_enthalpy >= keyhole_threshold,
        "balling": length_to_width >= balling_threshold,
    }
    active = [name for name, triggered in defects.items() if triggered]
    return {
        "power_w": float(power_w),
        "scan_velocity_m_per_s": float(scan_velocity_m_per_s),
        "regime": "+".join(active) if active else "printable",
        "defects": defects,
        "metrics": {
            "lack_of_fusion_overlap_index": overlap_index,
            "normalized_enthalpy": normalized_enthalpy,
            "deposited_enthalpy_density_j_per_m3": deposited_enthalpy_density,
            "melting_enthalpy_density_j_per_m3": melting_enthalpy_density,
            "liquidus_length_to_width_ratio": length_to_width,
        },
        "thresholds": {
            "lack_of_fusion_overlap_index": lof_threshold,
            "normalized_enthalpy": keyhole_threshold,
            "liquidus_length_to_width_ratio": balling_threshold,
        },
        "melt_pool": geometry,
    }


def generate_printability_map(
    *,
    powers_w: Sequence[float],
    scan_velocities_m_per_s: Sequence[float],
    hatch_spacing_m: float,
    layer_thickness_m: float,
    beam_diameter_m: float,
    initial_temperature_k: float,
    solidus_temperature_k: float,
    liquidus_temperature_k: float,
    thermal_conductivity_w_mk: float,
    density_kg_m3: float,
    specific_heat_j_kgk: float,
    latent_heat_j_kg: float,
    absorptivity: float,
    lof_overlap_threshold: float = DEFAULT_LOF_OVERLAP_THRESHOLD,
    keyhole_enthalpy_threshold: float = DEFAULT_KEYHOLE_ENTHALPY_THRESHOLD,
    balling_length_to_width_threshold: float = (
        DEFAULT_BALLING_LENGTH_TO_WIDTH_THRESHOLD
    ),
) -> dict:
    """Evaluate the Cartesian product of power and velocity inputs."""
    import numpy as np

    powers = np.asarray(powers_w, dtype=float)
    velocities = np.asarray(scan_velocities_m_per_s, dtype=float)
    if powers.ndim != 1 or powers.size == 0:
        raise ValueError("powers_w must be a non-empty one-dimensional array")
    if velocities.ndim != 1 or velocities.size == 0:
        raise ValueError(
            "scan_velocities_m_per_s must be a non-empty one-dimensional array"
        )
    if not np.all(np.isfinite(powers)) or np.any(powers <= 0.0):
        raise ValueError("every power in powers_w must be finite and > 0")
    if not np.all(np.isfinite(velocities)) or np.any(velocities <= 0.0):
        raise ValueError(
            "every velocity in scan_velocities_m_per_s must be finite and > 0"
        )

    shared = {
        "hatch_spacing_m": hatch_spacing_m,
        "layer_thickness_m": layer_thickness_m,
        "beam_diameter_m": beam_diameter_m,
        "initial_temperature_k": initial_temperature_k,
        "solidus_temperature_k": solidus_temperature_k,
        "liquidus_temperature_k": liquidus_temperature_k,
        "thermal_conductivity_w_mk": thermal_conductivity_w_mk,
        "density_kg_m3": density_kg_m3,
        "specific_heat_j_kgk": specific_heat_j_kgk,
        "latent_heat_j_kg": latent_heat_j_kg,
        "absorptivity": absorptivity,
        "lof_overlap_threshold": lof_overlap_threshold,
        "keyhole_enthalpy_threshold": keyhole_enthalpy_threshold,
        "balling_length_to_width_threshold": (
            balling_length_to_width_threshold
        ),
    }
    points = []
    regime_grid = []
    for velocity in velocities.tolist():
        row = []
        for power in powers.tolist():
            point = classify_parameter_set(
                power_w=power,
                scan_velocity_m_per_s=velocity,
                **shared,
            )
            points.append(point)
            row.append(point["regime"])
        regime_grid.append(row)

    return {
        "status": "ok",
        "model": "Rosenthal 3D moving surface point source",
        "grid_shape": [int(velocities.size), int(powers.size)],
        "powers_w": powers.tolist(),
        "scan_velocities_m_per_s": velocities.tolist(),
        "regime_grid": regime_grid,
        "points": points,
        "limitations": [
            "Analytical screening map, not CFD or a qualification result.",
            "Rosenthal assumes a point source, constant properties, no latent "
            "heat in the temperature field, and a semi-infinite body.",
            "Defect boundaries require calibration against experiments for the "
            "material, powder, machine, atmosphere, and beam definition.",
        ],
    }


def kou_cracking_index(
    *,
    temperatures_k: Sequence[float],
    solid_fractions: Sequence[float],
    terminal_solid_fraction_min: float = 0.9,
) -> dict:
    """Compute Kou's terminal-solidification cracking susceptibility index.

    The returned index is ``max |dT/d(sqrt(f_s))|`` over the supplied path at
    ``f_s >= terminal_solid_fraction_min``.  The derivative is the slope of
    each piecewise-linear segment in ``sqrt(f_s)``; the path is interpolated at
    the lower terminal-window boundary when needed.

    Source: S. Kou, "A criterion for cracking during solidification," Acta
    Materialia 88 (2015) 366--374, doi:10.1016/j.actamat.2015.01.034.  Kou's
    criterion is comparative: larger terminal slopes indicate greater
    susceptibility.  The paper does not establish one universal index cutoff,
    so this function does not fabricate a pass/fail threshold.  The default
    ``f_s >= 0.9`` is an exposed numerical terminal window, not a claimed
    material constant.
    """
    import numpy as np

    temperatures = np.asarray(temperatures_k, dtype=float)
    fractions = np.asarray(solid_fractions, dtype=float)
    if temperatures.ndim != 1 or fractions.ndim != 1:
        raise ValueError("temperatures_k and solid_fractions must be 1D arrays")
    if temperatures.size != fractions.size or temperatures.size < 2:
        raise ValueError(
            "temperatures_k and solid_fractions must have equal length >= 2"
        )
    if not np.all(np.isfinite(temperatures)) or np.any(temperatures <= 0.0):
        raise ValueError("temperatures_k must contain finite Kelvin values > 0")
    if not np.all(np.isfinite(fractions)) or np.any(
        (fractions < 0.0) | (fractions > 1.0)
    ):
        raise ValueError("solid_fractions must contain finite values in [0, 1]")
    if np.any(np.diff(fractions) <= 0.0):
        raise ValueError("solid_fractions must be strictly increasing")
    if np.any(np.diff(temperatures) > 0.0):
        raise ValueError(
            "temperatures_k must be non-increasing along the solidification path"
        )
    terminal_min = float(terminal_solid_fraction_min)
    if not math.isfinite(terminal_min) or not 0.0 <= terminal_min < 1.0:
        raise ValueError("terminal_solid_fraction_min must be in [0, 1)")
    if fractions[-1] <= terminal_min:
        raise ValueError(
            "solidification path must extend above terminal_solid_fraction_min"
        )

    sqrt_fractions = np.sqrt(fractions)
    terminal_x = math.sqrt(terminal_min)
    first = int(np.searchsorted(sqrt_fractions, terminal_x, side="left"))
    selected_x = sqrt_fractions[first:].tolist()
    selected_t = temperatures[first:].tolist()

    if first > 0 and sqrt_fractions[first] > terminal_x:
        x0, x1 = sqrt_fractions[first - 1], sqrt_fractions[first]
        t0, t1 = temperatures[first - 1], temperatures[first]
        boundary_t = t0 + (t1 - t0) * (terminal_x - x0) / (x1 - x0)
        selected_x.insert(0, terminal_x)
        selected_t.insert(0, float(boundary_t))

    if len(selected_x) < 2:
        raise ValueError(
            "at least two path points are required in the terminal window"
        )

    slopes = np.diff(np.asarray(selected_t)) / np.diff(np.asarray(selected_x))
    absolute_slopes = np.abs(slopes)
    maximum_index = int(np.argmax(absolute_slopes))
    intervals = [
        {
            "solid_fraction_start": float(selected_x[i] ** 2),
            "solid_fraction_end": float(selected_x[i + 1] ** 2),
            "dT_d_sqrt_fs_k": float(slopes[i]),
        }
        for i in range(slopes.size)
    ]
    return {
        "status": "ok",
        "kou_index_k": float(absolute_slopes[maximum_index]),
        "terminal_solid_fraction_min": terminal_min,
        "critical_interval": intervals[maximum_index],
        "terminal_intervals": intervals,
        "n_terminal_intervals": len(intervals),
        "interpretation": (
            "Comparative index only: larger values indicate greater cracking "
            "susceptibility; no universal pass/fail cutoff is applied."
        ),
    }
