"""Tests for the phase analysis skill."""

from unittest.mock import MagicMock, patch

import pytest

from app.skills.phase_analysis import PHASE_ANALYSIS_SKILL, _analyze_phases


class TestPhaseAnalysisSkill:
    def test_skill_metadata(self):
        assert PHASE_ANALYSIS_SKILL.name == "analyze_phases"
        assert PHASE_ANALYSIS_SKILL.category == "thermodynamics"
        tool = PHASE_ANALYSIS_SKILL.to_tool()
        assert tool.name == "analyze_phases"

    def test_skill_has_required_fields(self):
        schema = PHASE_ANALYSIS_SKILL.input_schema
        assert "database_name" in schema["required"]
        assert "components" in schema["required"]

    def test_error_when_pycalphad_missing(self):
        result = _analyze_phases(
            database_name="test",
            components=["Al", "Ni"],
        )
        assert "error" in result
        assert "pycalphad" in result["error"]

    @patch("app.simulation.calphad_bridge.check_calphad_available", return_value=True)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_analyze_with_mocked_bridge(self, mock_bridge_fn, mock_available):
        mock_bridge = MagicMock()
        mock_bridge.databases.get_phases.return_value = ["FCC_A1", "BCC_A2", "LIQUID"]
        mock_bridge.calculate_equilibrium.return_value = {
            "phases_present": ["FCC_A1"],
            "phase_fractions": {"FCC_A1": 1.0},
            "gibbs_energy": -50000.0,
        }
        mock_bridge.calculate_phase_diagram.return_value = {
            "n_points": 18,
            "data_points": [],
        }
        mock_bridge_fn.return_value = mock_bridge

        result = _analyze_phases(
            database_name="sgte",
            components=["Al", "Ni"],
            temperature=1200,
        )

        assert result["database"] == "sgte"
        assert result["available_phases"] == 3
        assert "stable_phases" in result
        assert "FCC_A1" in result["stable_phases"]

    @patch("app.simulation.calphad_bridge.check_calphad_available", return_value=True)
    @patch("app.simulation.calphad_bridge.get_calphad_bridge")
    def test_database_not_found(self, mock_bridge_fn, mock_available):
        mock_bridge = MagicMock()
        mock_bridge.databases.get_phases.return_value = None
        mock_bridge_fn.return_value = mock_bridge

        result = _analyze_phases(
            database_name="nonexistent",
            components=["Al", "Ni"],
        )
        assert "error" in result
