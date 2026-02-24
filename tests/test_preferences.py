"""Tests for UserPreferences."""

import json
from pathlib import Path

import pytest
from app.config.preferences import UserPreferences


@pytest.fixture
def tmp_prefs(tmp_path, monkeypatch):
    """Redirect preferences to a temp directory."""
    prefs_path = tmp_path / "preferences.json"
    monkeypatch.setattr("app.config.preferences.PRISM_DIR", tmp_path)
    monkeypatch.setattr("app.config.preferences.PREFERENCES_PATH", prefs_path)
    return prefs_path


class TestUserPreferences:
    def test_defaults(self):
        p = UserPreferences()
        assert p.output_format == "csv"
        assert p.default_algorithm == "random_forest"
        assert p.compute_budget == "local"
        assert p.hpc_cores == 4
        assert "optimade" in p.default_providers

    def test_load_defaults_when_missing(self, tmp_prefs):
        p = UserPreferences.load()
        assert p.output_format == "csv"

    def test_save_and_load_roundtrip(self, tmp_prefs):
        p = UserPreferences(
            output_format="csv",
            default_algorithm="gradient_boosting",
            hpc_cores=16,
        )
        p.save()

        loaded = UserPreferences.load()
        assert loaded.output_format == "csv"
        assert loaded.default_algorithm == "gradient_boosting"
        assert loaded.hpc_cores == 16

    def test_field_overrides(self, tmp_prefs):
        tmp_prefs.write_text(json.dumps({"output_format": "both", "hpc_queue": "gpu"}))
        p = UserPreferences.load()
        assert p.output_format == "both"
        assert p.hpc_queue == "gpu"
        # defaults for unspecified fields
        assert p.default_algorithm == "random_forest"

    def test_ignores_unknown_keys(self, tmp_prefs):
        tmp_prefs.write_text(json.dumps({"output_format": "csv", "unknown_key": 42}))
        p = UserPreferences.load()
        assert p.output_format == "csv"

    def test_handles_corrupt_json(self, tmp_prefs):
        tmp_prefs.write_text("not valid json{{{")
        p = UserPreferences.load()
        assert p.output_format == "csv"  # falls back to defaults

    def test_save_creates_directory(self, tmp_path, monkeypatch):
        nested = tmp_path / "sub" / "dir"
        prefs_path = nested / "preferences.json"
        monkeypatch.setattr("app.config.preferences.PRISM_DIR", nested)
        monkeypatch.setattr("app.config.preferences.PREFERENCES_PATH", prefs_path)

        p = UserPreferences(output_format="csv")
        path = p.save()
        assert path.exists()
        assert json.loads(path.read_text())["output_format"] == "csv"
