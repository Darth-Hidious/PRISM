"""Tests for prism setup CLI command."""

import json

from click.testing import CliRunner

from app.cli import cli


def test_setup_defaults(tmp_path, monkeypatch):
    """Setup wizard with all defaults produces valid preferences."""
    prefs_path = tmp_path / "preferences.json"
    monkeypatch.setattr("app.config.preferences.PRISM_DIR", tmp_path)
    monkeypatch.setattr("app.config.preferences.PREFERENCES_PATH", prefs_path)

    runner = CliRunner()
    # Press enter for each prompt to accept defaults (7 prompts for local budget)
    result = runner.invoke(cli, ["setup"], input="\n\n\n\n\n\n")

    assert result.exit_code == 0
    assert "Preferences saved" in result.output
    assert prefs_path.exists()

    data = json.loads(prefs_path.read_text())
    assert data["output_format"] == "csv"
    assert data["default_algorithm"] == "random_forest"


def test_setup_custom_values(tmp_path, monkeypatch):
    """Setup wizard with custom inputs."""
    prefs_path = tmp_path / "preferences.json"
    monkeypatch.setattr("app.config.preferences.PRISM_DIR", tmp_path)
    monkeypatch.setattr("app.config.preferences.PREFERENCES_PATH", prefs_path)

    runner = CliRunner()
    # csv, optimade,mp, 50, gradient_boosting, markdown, local
    result = runner.invoke(
        cli, ["setup"], input="csv\noptimade,mp\n50\ngradient_boosting\nmarkdown\nlocal\n"
    )

    assert result.exit_code == 0
    data = json.loads(prefs_path.read_text())
    assert data["output_format"] == "csv"
    assert data["default_algorithm"] == "gradient_boosting"
    assert data["max_results_per_source"] == 50


def test_setup_hpc_prompts(tmp_path, monkeypatch):
    """HPC budget triggers extra prompts for queue and cores."""
    prefs_path = tmp_path / "preferences.json"
    monkeypatch.setattr("app.config.preferences.PRISM_DIR", tmp_path)
    monkeypatch.setattr("app.config.preferences.PREFERENCES_PATH", prefs_path)

    runner = CliRunner()
    # parquet, optimade, 100, random_forest, markdown, hpc, gpu_queue, 32
    result = runner.invoke(
        cli,
        ["setup"],
        input="parquet\noptimade\n100\nrandom_forest\nmarkdown\nhpc\ngpu_queue\n32\n",
    )

    assert result.exit_code == 0
    data = json.loads(prefs_path.read_text())
    assert data["compute_budget"] == "hpc"
    assert data["hpc_queue"] == "gpu_queue"
    assert data["hpc_cores"] == 32
