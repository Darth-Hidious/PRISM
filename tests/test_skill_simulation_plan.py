"""Tests for the simulation planning skill."""

from unittest.mock import patch

import pandas as pd
import pytest

from app.skills.simulation_plan import SIM_PLAN_SKILL, _plan_simulations


@pytest.fixture
def mock_prefs(monkeypatch):
    from app.config.preferences import UserPreferences

    prefs = UserPreferences(compute_budget="local", hpc_queue="default", hpc_cores=4)
    monkeypatch.setattr(
        "app.skills.simulation_plan.UserPreferences.load", lambda: prefs
    )
    return prefs


@pytest.fixture
def sample_df():
    return pd.DataFrame(
        {
            "formula": ["Fe2O3", "Al2O3", "SiO2"],
            "band_gap": [2.0, 8.8, 9.0],
        }
    )


class TestSimPlanSkill:
    def test_skill_metadata(self):
        assert SIM_PLAN_SKILL.name == "plan_simulations"
        tool = SIM_PLAN_SKILL.to_tool()
        assert tool.name == "plan_simulations"

    @patch("app.data.store.DataStore.load")
    def test_plan_local(self, mock_load, mock_prefs, sample_df):
        mock_load.return_value = sample_df

        result = _plan_simulations(dataset_name="test_data")

        assert result["planned_jobs"] == 3
        assert result["compute_budget"] == "local"
        assert all(j["status"] == "planned" for j in result["jobs"])
        assert "hpc" not in result["jobs"][0]

    @patch("app.data.store.DataStore.load")
    def test_plan_hpc(self, mock_load, mock_prefs, sample_df):
        mock_prefs.compute_budget = "hpc"
        mock_prefs.hpc_queue = "gpu"
        mock_prefs.hpc_cores = 32
        mock_load.return_value = sample_df

        result = _plan_simulations(
            dataset_name="test_data", compute_budget="hpc"
        )

        assert result["compute_budget"] == "hpc"
        assert result["jobs"][0]["hpc"]["queue"] == "gpu"
        assert result["jobs"][0]["hpc"]["cores"] == 32

    @patch("app.data.store.DataStore.load")
    def test_max_jobs(self, mock_load, mock_prefs, sample_df):
        mock_load.return_value = sample_df

        result = _plan_simulations(dataset_name="test_data", max_jobs=2)
        assert result["planned_jobs"] == 2

    @patch("app.data.store.DataStore.load")
    def test_multiple_sim_types(self, mock_load, mock_prefs, sample_df):
        mock_load.return_value = sample_df

        result = _plan_simulations(
            dataset_name="test_data",
            simulation_types=["energy_minimization", "md"],
        )
        # 3 materials * 2 sim types
        assert result["planned_jobs"] == 6

    @patch("app.data.store.DataStore.load")
    def test_dataset_not_found(self, mock_load, mock_prefs):
        mock_load.side_effect = FileNotFoundError()

        result = _plan_simulations(dataset_name="nonexistent")
        assert "error" in result

    @patch("app.data.store.DataStore.load")
    def test_no_formula_column(self, mock_load, mock_prefs):
        df = pd.DataFrame({"value": [1.0]})
        mock_load.return_value = df

        result = _plan_simulations(dataset_name="no_formula")
        assert "error" in result
