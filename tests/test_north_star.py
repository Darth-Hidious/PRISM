"""North star integration test: end-to-end materials discovery pipeline.

Tests the full workflow: "Find alloys with W and Rh" →
acquire → predict → visualize → report.
"""

from pathlib import Path
from unittest.mock import MagicMock, patch

import pandas as pd
import pytest


# ---- Test: Skills appear in ToolRegistry ----

class TestSkillsInToolRegistry:
    def test_skills_in_autonomous_tools(self):
        from app.agent.autonomous import _make_tools

        tools = _make_tools(enable_mcp=False)
        names = {t.name for t in tools.list_tools()}

        assert "acquire_materials" in names
        assert "predict_properties" in names
        assert "visualize_dataset" in names
        assert "generate_report" in names
        assert "select_materials" in names
        assert "materials_discovery" in names
        assert "plan_simulations" in names

    def test_all_8_skills_registered(self):
        from app.skills.registry import load_builtin_skills

        reg = load_builtin_skills()
        assert len(reg.list_skills()) == 8


# ---- Test: System prompts mention skills ----

class TestSystemPrompts:
    def test_default_prompt_mentions_skills(self):
        from app.agent.core import DEFAULT_SYSTEM_PROMPT

        assert "materials_discovery" in DEFAULT_SYSTEM_PROMPT
        assert "acquire_materials" in DEFAULT_SYSTEM_PROMPT
        assert "skills" in DEFAULT_SYSTEM_PROMPT.lower()

    def test_autonomous_prompt_mentions_skills(self):
        from app.agent.autonomous import AUTONOMOUS_SYSTEM_PROMPT

        assert "materials_discovery" in AUTONOMOUS_SYSTEM_PROMPT
        assert "Skills" in AUTONOMOUS_SYSTEM_PROMPT


# ---- Test: MCP server includes skills ----

try:
    import fastmcp  # noqa: F401
    _HAS_FASTMCP = True
except ImportError:
    _HAS_FASTMCP = False


@pytest.mark.skipif(not _HAS_FASTMCP, reason="fastmcp not installed")
class TestMCPSkills:
    def test_build_registry_includes_skills(self):
        with patch("app.mcp_server.check_pyiron_available", return_value=False):
            from app.mcp_server import _build_registry

            registry = _build_registry()
            names = {t.name for t in registry.list_tools()}
            assert "acquire_materials" in names
            assert "materials_discovery" in names


# ---- Test: End-to-end discovery pipeline (mocked) ----

class TestNorthStarPipeline:
    @patch("app.skills.reporting._generate_report")
    @patch("app.skills.visualization._visualize_dataset")
    @patch("app.skills.prediction._predict_properties")
    @patch("app.skills.acquisition._acquire_materials")
    def test_w_rh_discovery(
        self, mock_acquire, mock_predict, mock_viz, mock_report
    ):
        """North star: 'Find alloys with W and Rh that are stable'."""
        # Mock acquisition
        mock_acquire.return_value = {
            "dataset_name": "w_rh_discovery",
            "total_records": 10,
            "columns": ["formula", "band_gap", "formation_energy_per_atom"],
            "sources_queried": ["optimade"],
        }

        # Mock prediction
        mock_predict.return_value = {
            "dataset_name": "w_rh_discovery",
            "predictions": {
                "band_gap": "predicted_band_gap",
                "formation_energy_per_atom": "predicted_formation_energy_per_atom",
            },
            "algorithm": "random_forest",
            "rows": 10,
        }

        # Mock visualization
        mock_viz.return_value = {
            "dataset_name": "w_rh_discovery",
            "plots": ["dist_band_gap.png", "comparison.png"],
            "columns_plotted": ["band_gap", "formation_energy_per_atom"],
        }

        # Mock report
        mock_report.return_value = {
            "report_path": "/tmp/w_rh_discovery_report.md",
            "format": "markdown",
        }

        from app.skills.discovery import _materials_discovery

        result = _materials_discovery(
            elements=["W", "Rh"],
            title="W-Rh Alloy Discovery",
        )

        # Verify chain executed
        assert result["dataset_name"] == "w_rh_discovery"
        assert "acquisition" in result["results"]
        assert "prediction" in result["results"]
        assert "visualization" in result["results"]
        assert "report" in result["results"]

        # Verify each step produced expected output
        acq = result["results"]["acquisition"]
        assert acq["total_records"] == 10

        pred = result["results"]["prediction"]
        assert "predicted_band_gap" in pred["predictions"].values()

        viz = result["results"]["visualization"]
        assert len(viz["plots"]) == 2

        report = result["results"]["report"]
        assert report["format"] == "markdown"

    @patch("app.data.store.DataStore.save")
    @patch("app.data.store.DataStore.load")
    def test_selection_after_discovery(self, mock_load, mock_save):
        """After discovery, select top candidates."""
        df = pd.DataFrame(
            {
                "formula": ["W3Rh", "WRh3", "W2Rh", "WRh", "W4Rh"],
                "band_gap": [0.5, 1.0, 0.3, 0.8, 0.2],
                "formation_energy_per_atom": [-0.5, -1.0, -0.3, -0.8, -0.2],
            }
        )
        mock_load.return_value = df
        mock_save.return_value = "/tmp/test.parquet"

        from app.skills.selection import _select_materials

        result = _select_materials(
            dataset_name="w_rh_discovery",
            criteria={"formation_energy_per_atom_max": -0.4},
            sort_by="formation_energy_per_atom",
            top_n=3,
        )

        assert result["selected_count"] <= 3
        assert result["dataset_name"] == "w_rh_discovery_selected"

    @patch("app.data.store.DataStore.load")
    def test_simulation_plan_for_candidates(self, mock_load):
        """Plan simulations for selected candidates."""
        df = pd.DataFrame(
            {
                "formula": ["W3Rh", "WRh3"],
                "band_gap": [0.5, 1.0],
            }
        )
        mock_load.return_value = df

        from app.config.preferences import UserPreferences
        from app.skills.simulation_plan import _plan_simulations

        with patch(
            "app.skills.simulation_plan.UserPreferences.load",
            return_value=UserPreferences(compute_budget="local"),
        ):
            result = _plan_simulations(
                dataset_name="w_rh_selected",
                simulation_types=["energy_minimization", "md"],
            )

        assert result["planned_jobs"] == 4  # 2 materials * 2 sim types
        assert all(j["status"] == "planned" for j in result["jobs"])
