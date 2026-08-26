"""Tests for LPBF Rosenthal printability and Kou cracking tools."""

import builtins
import math
from unittest.mock import patch

import pytest

from app.tools.base import ToolRegistry
from app.tools.manufacturing.lpbf import (
    check_lpbf_available,
    classify_parameter_set,
    generate_printability_map,
    kou_cracking_index,
    rosenthal_melt_pool_geometry,
)
from app.tools.manufacturing.lpbf.tools import (
    _run_kou_index,
    create_lpbf_tools,
)


# Dimensionally consistent analytical fixture only. These values are not
# reference properties for an alloy.
def _synthetic_material() -> dict:
    return {
        "initial_temperature_k": 300.0,
        "solidus_temperature_k": 1100.0,
        "liquidus_temperature_k": 1300.0,
        "thermal_conductivity_w_mk": 10.0,
        "density_kg_m3": 1000.0,
        "specific_heat_j_kgk": 1000.0,
        "latent_heat_j_kg": 100000.0,
        "absorptivity": 0.5,
    }


def _analytical_process() -> dict:
    # A clean process point, chosen so the field constants are exact:
    # alpha = 10/(1000*1000) = 1e-5 m2/s, beta*r = ln(2) at r = 1e-4 m, and
    # A = eta*P/(2*pi*k) = 0.2 K m, giving a liquidus rise of exactly 1000 K.
    #
    # NOTE: r = 1e-4 is the isotherm radius directly beneath the source
    # (xi = 0). That is NOT the melt pool depth or half-width — a moving
    # source drags its pool backwards, so the widest point sits behind the
    # beam. The transverse extents are pinned in the tests against an
    # independent root-find, not against this number.
    radius_m = 1.0e-4
    alpha = 1.0e-5
    beta = math.log(2.0) / radius_m
    amplitude_k_m = 0.2
    conductivity = 10.0
    absorptivity = 0.5
    return {
        "power_w": amplitude_k_m * 2.0 * math.pi * conductivity / absorptivity,
        "scan_velocity_m_per_s": 2.0 * alpha * beta,
    }


def _classification_inputs() -> dict:
    return {
        **_analytical_process(),
        "hatch_spacing_m": 4.0e-5,
        "layer_thickness_m": 2.0e-5,
        "beam_diameter_m": 1.0e-4,
        **_synthetic_material(),
    }


def test_rosenthal_known_analytical_transverse_isotherm():
    result = rosenthal_melt_pool_geometry(
        **_analytical_process(), **_synthetic_material()
    )

    assert result["thermal_diffusivity_m2_per_s"] == pytest.approx(1.0e-5)
    # Transverse extents are the MAXIMUM over the pool, which lies behind the
    # beam — not the isotherm radius at xi = 0 (which would be 1.0e-4 here).
    # Values cross-checked against a per-xi root-find in the test below.
    assert result["liquidus"]["depth_m"] == pytest.approx(1.11153157e-4, rel=1e-8)
    assert result["liquidus"]["width_m"] == pytest.approx(2.22306314e-4, rel=1e-8)
    # Rear length is exactly A/DeltaT and is unaffected by the above.
    assert result["liquidus"]["rear_length_m"] == pytest.approx(2.0e-4)
    assert result["latent_heat_used_in_temperature_field"] is False


def test_rosenthal_transverse_extents_match_independent_root_find():
    """Guard the melt-pool maximum against a method that shares no algebra.

    The implementation reduces the widest-point condition to a scalar root in
    s = beta*R. This test instead solves T(xi, rho) = dT for rho at each xi and
    maximises over xi, so a mistake in that reduction cannot hide.
    """
    from scipy.optimize import brentq, minimize_scalar

    material = _synthetic_material()
    process = _analytical_process()
    result = rosenthal_melt_pool_geometry(**process, **material)

    alpha = material["thermal_conductivity_w_mk"] / (
        material["density_kg_m3"] * material["specific_heat_j_kgk"]
    )
    amplitude = (
        material["absorptivity"]
        * process["power_w"]
        / (2.0 * math.pi * material["thermal_conductivity_w_mk"])
    )
    beta = process["scan_velocity_m_per_s"] / (2.0 * alpha)

    for isotherm in ("solidus", "liquidus"):
        rise = material[f"{isotherm}_temperature_k"] - material[
            "initial_temperature_k"
        ]

        def radius_at(xi: float) -> float:
            def residual(rho: float) -> float:
                radial = math.sqrt(xi * xi + rho * rho)
                return (amplitude / radial) * math.exp(
                    -beta * (radial + xi)
                ) - rise

            if residual(1e-12) < 0.0:
                return 0.0
            return brentq(residual, 1e-12, 1e-2, xtol=1e-16, rtol=1e-15)

        found = minimize_scalar(
            lambda xi: -radius_at(xi),
            bounds=(-5e-3, 0.0),
            method="bounded",
            options={"xatol": 1e-12},
        )
        half_width = -found.fun

        assert result[isotherm]["depth_m"] == pytest.approx(half_width, rel=1e-9)
        assert result[isotherm]["width_m"] == pytest.approx(
            2.0 * half_width, rel=1e-9
        )
        # The widest point is strictly behind the beam, so it must exceed the
        # xi = 0 radius. This is the assertion the original code failed.
        radius_beneath_beam = radius_at(0.0)
        assert half_width > radius_beneath_beam * 1.05


def test_lack_of_fusion_boundary_triggers_and_does_not_trigger():
    safe = classify_parameter_set(**_classification_inputs())
    assert safe["metrics"]["lack_of_fusion_overlap_index"] < 1.0
    assert safe["defects"]["lack_of_fusion"] is False

    # Hatch wider than the pool and a layer thicker than half its depth, so
    # (h/W)^2 + (t/D)^2 clears 1.0 with margin rather than sitting on it. The
    # earlier values sat at 0.987 and only "triggered" because the melt pool
    # was being undersized by ~40%.
    insufficient_overlap = classify_parameter_set(
        **{
            **_classification_inputs(),
            "hatch_spacing_m": 2.4e-4,
            "layer_thickness_m": 6.0e-5,
        }
    )
    assert insufficient_overlap["metrics"]["lack_of_fusion_overlap_index"] > 1.0
    assert insufficient_overlap["defects"]["lack_of_fusion"] is True


def test_keyhole_boundary_triggers_and_does_not_trigger():
    baseline = classify_parameter_set(**_classification_inputs())
    value = baseline["metrics"]["normalized_enthalpy"]

    below_boundary = classify_parameter_set(
        **_classification_inputs(), keyhole_enthalpy_threshold=2.0 * value
    )
    above_boundary = classify_parameter_set(
        **_classification_inputs(), keyhole_enthalpy_threshold=0.5 * value
    )
    assert below_boundary["defects"]["keyholing"] is False
    assert above_boundary["defects"]["keyholing"] is True


def test_balling_boundary_triggers_and_does_not_trigger():
    baseline = classify_parameter_set(**_classification_inputs())
    value = baseline["metrics"]["liquidus_length_to_width_ratio"]

    below_boundary = classify_parameter_set(
        **_classification_inputs(), balling_length_to_width_threshold=2.0 * value
    )
    above_boundary = classify_parameter_set(
        **_classification_inputs(), balling_length_to_width_threshold=0.5 * value
    )
    assert below_boundary["defects"]["balling"] is False
    assert above_boundary["defects"]["balling"] is True


def test_printability_map_is_cartesian_power_velocity_grid():
    process = _classification_inputs()
    power = process.pop("power_w")
    velocity = process.pop("scan_velocity_m_per_s")
    result = generate_printability_map(
        powers_w=[power, 2.0 * power],
        scan_velocities_m_per_s=[velocity, 2.0 * velocity],
        **process,
    )

    assert result["grid_shape"] == [2, 2]
    assert len(result["points"]) == 4
    assert len(result["regime_grid"]) == 2
    assert all(len(row) == 2 for row in result["regime_grid"])
    assert result["limitations"]


def test_kou_index_has_known_synthetic_answer():
    # T = 1000 - 200*sqrt(f_s), so |dT/d(sqrt(f_s))| = 200 K.
    result = kou_cracking_index(
        temperatures_k=[820.0, 810.0, 800.0],
        solid_fractions=[0.81, 0.9025, 1.0],
    )

    assert result["kou_index_k"] == pytest.approx(200.0, rel=1e-12)
    assert result["n_terminal_intervals"] == 2
    assert result["critical_interval"]["dT_d_sqrt_fs_k"] == pytest.approx(-200.0)
    assert "no universal" in result["interpretation"]


def test_kou_rejects_non_solidification_path():
    with pytest.raises(ValueError, match="non-increasing"):
        kou_cracking_index(
            temperatures_k=[800.0, 810.0],
            solid_fractions=[0.9, 1.0],
        )


def test_availability_gate_detects_missing_dependency():
    real_import = builtins.__import__

    def import_without_scipy(name, *args, **kwargs):
        if name == "scipy.special" or name.startswith("scipy"):
            raise ImportError("simulated missing scipy")
        return real_import(name, *args, **kwargs)

    with patch("builtins.__import__", side_effect=import_without_scipy):
        assert check_lpbf_available() is False


def test_direct_tool_call_returns_structured_missing_dependency_result():
    """OLD ORACLE (wrong): this pinned install_hint to
    ``pip install 'prism-platform[lpbf]'``. `app/tools/_extras.py` records that
    `prism-platform` is on no index (HTTP 404), which is exactly why
    `missing_extra_error` puts the distribution names in `install_hint` and
    demotes the extra form to `install_extra_hint` — and why
    `tests/test_marketplace_catalog.py::test_missing_extra_error_shape` asserts
    ``"prism-platform[" not in install_hint``. The old oracle froze the 404 in
    place and, by asserting only `error` and `install_hint`, let the gate ship
    with no `requires_extra` for a caller to branch on. Guard the real rule:
    this gate must produce the one shape from `_extras`.
    """
    from app.tools import _extras

    with patch(
        "app.tools.manufacturing.lpbf.tools.check_lpbf_available",
        return_value=False,
    ):
        result = _run_kou_index(
            temperatures_k=[810.0, 800.0], solid_fractions=[0.9, 1.0]
        )

    assert "error" in result
    assert result["requires_extra"] == "lpbf"
    assert result["install_hint"] == _extras.install_command("lpbf")
    assert "prism-platform[" not in result["install_hint"]
    assert result["install_extra_hint"] == "pip install 'prism-platform[lpbf]'"
    assert result["missing_capability"]


def test_tools_absent_from_registry_when_dependencies_missing(monkeypatch):
    monkeypatch.setenv("PRISM_DISABLE_MEMORY", "1")
    with patch(
        "app.tools.manufacturing.lpbf.check_lpbf_available", return_value=False
    ):
        from app.plugins.bootstrap import build_full_registry

        registry, _providers, _agents = build_full_registry(
            enable_mcp=False, enable_plugins=False
        )

    names = {tool.name for tool in registry.list_tools()}
    assert "lpbf_printability_map" not in names
    assert "lpbf_kou_cracking_index" not in names


def test_lpbf_tools_carry_scientific_tool_contract():
    registry = ToolRegistry()
    create_lpbf_tools(registry)

    names = {tool.name for tool in registry.list_tools()}
    assert names == {"lpbf_printability_map", "lpbf_kou_cracking_index"}
    for tool in registry.list_tools():
        assert tool.units
        assert tool.output_schema
        assert tool.examples
        assert tool.validate

    printability = registry.get("lpbf_printability_map")
    assert set(_synthetic_material()).issubset(
        set(printability.input_schema["required"])
    )
