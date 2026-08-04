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
    # Hand calculation for the liquidus transverse isotherm:
    # alpha = 10/(1000*1000) = 1e-5 m2/s. Choose r=1e-4 m and
    # beta*r=ln(2), then exp(-beta*r)=1/2. Choose A=eta*P/(2*pi*k)
    # = 2*DeltaT*r = 0.2 K m. Therefore
    # DeltaT=A/r*exp(-beta*r)=0.2/1e-4/2=1000 K exactly,
    # so liquidus depth=r and width=2r.
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
    assert result["liquidus"]["depth_m"] == pytest.approx(1.0e-4, rel=1e-12)
    assert result["liquidus"]["width_m"] == pytest.approx(2.0e-4, rel=1e-12)
    assert result["liquidus"]["rear_length_m"] == pytest.approx(2.0e-4)
    assert result["latent_heat_used_in_temperature_field"] is False


def test_lack_of_fusion_boundary_triggers_and_does_not_trigger():
    safe = classify_parameter_set(**_classification_inputs())
    assert safe["metrics"]["lack_of_fusion_overlap_index"] < 1.0
    assert safe["defects"]["lack_of_fusion"] is False

    insufficient_overlap = classify_parameter_set(
        **{
            **_classification_inputs(),
            "hatch_spacing_m": 2.2e-4,
            "layer_thickness_m": 1.0e-5,
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
    with patch(
        "app.tools.manufacturing.lpbf.tools.check_lpbf_available",
        return_value=False,
    ):
        result = _run_kou_index(
            temperatures_k=[810.0, 800.0], solid_fractions=[0.9, 1.0]
        )

    assert "error" in result
    assert result["install_hint"] == "pip install 'prism-platform[lpbf]'"


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
