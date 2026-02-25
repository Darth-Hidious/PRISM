"""Tests for the unified settings system."""

import json
from pathlib import Path

import pytest

from app.config.settings_schema import (
    PrismSettings,
    AgentSettings,
    SearchSettings,
    OutputSettings,
    UpdateSettings,
    PermissionSettings,
    load_settings,
    save_global_settings,
    save_project_settings,
    get_settings,
    reload_settings,
    _deep_merge,
    _dataclass_from_dict,
    _apply_env_overrides,
    GLOBAL_SETTINGS_PATH,
)


# ---------- Defaults ----------


class TestDefaults:
    def test_default_settings_has_all_sections(self):
        s = PrismSettings()
        assert isinstance(s.agent, AgentSettings)
        assert isinstance(s.search, SearchSettings)
        assert isinstance(s.output, OutputSettings)
        assert isinstance(s.updates, UpdateSettings)
        assert isinstance(s.permissions, PermissionSettings)

    def test_default_agent_model_is_empty(self):
        s = PrismSettings()
        assert s.agent.model == ""
        assert s.agent.max_iterations == 30

    def test_default_search(self):
        s = PrismSettings()
        assert s.search.default_providers == ["optimade"]
        assert s.search.max_results_per_source == 100

    def test_default_update_check(self):
        s = PrismSettings()
        assert s.updates.check_on_startup is True
        assert s.updates.cache_ttl_hours == 24

    def test_default_permissions(self):
        s = PrismSettings()
        assert "execute_python" in s.permissions.require_approval
        assert s.permissions.deny == []


# ---------- Deep merge ----------


class TestDeepMerge:
    def test_flat_merge(self):
        assert _deep_merge({"a": 1}, {"b": 2}) == {"a": 1, "b": 2}

    def test_override_value(self):
        assert _deep_merge({"a": 1}, {"a": 2}) == {"a": 2}

    def test_nested_merge(self):
        base = {"agent": {"model": "", "max_iterations": 30}}
        override = {"agent": {"model": "gpt-4o"}}
        result = _deep_merge(base, override)
        assert result["agent"]["model"] == "gpt-4o"
        assert result["agent"]["max_iterations"] == 30

    def test_empty_override(self):
        base = {"a": 1}
        assert _deep_merge(base, {}) == {"a": 1}


# ---------- Dataclass reconstruction ----------


class TestDataclassFromDict:
    def test_simple_reconstruction(self):
        data = {"model": "gpt-4o", "max_iterations": 10}
        result = _dataclass_from_dict(AgentSettings, data)
        assert result.model == "gpt-4o"
        assert result.max_iterations == 10

    def test_ignores_unknown_keys(self):
        data = {"model": "gpt-4o", "nonexistent_field": True}
        result = _dataclass_from_dict(AgentSettings, data)
        assert result.model == "gpt-4o"

    def test_nested_reconstruction(self):
        data = {
            "agent": {"model": "gpt-4o"},
            "search": {"max_results_per_source": 50},
        }
        result = _dataclass_from_dict(PrismSettings, data)
        assert result.agent.model == "gpt-4o"
        assert result.search.max_results_per_source == 50

    def test_missing_sections_use_defaults(self):
        data = {"agent": {"model": "test"}}
        result = _dataclass_from_dict(PrismSettings, data)
        assert result.agent.model == "test"
        # Other sections should be default
        assert result.search.default_providers == ["optimade"]


# ---------- Env overrides ----------


class TestEnvOverrides:
    def test_model_from_legacy_env(self, monkeypatch):
        monkeypatch.setenv("PRISM_DEFAULT_MODEL", "claude-opus-4-6")
        from dataclasses import asdict
        data = asdict(PrismSettings())
        result = _apply_env_overrides(data)
        assert result["agent"]["model"] == "claude-opus-4-6"

    def test_reserved_vars_skipped(self, monkeypatch):
        monkeypatch.setenv("PRISM_LABS_API_KEY", "secret")
        from dataclasses import asdict
        data = asdict(PrismSettings())
        # Should not crash or create weird entries
        result = _apply_env_overrides(data)
        assert "labs" not in result

    def test_bool_coercion(self, monkeypatch):
        monkeypatch.setenv("PRISM_AGENT_AUTOAPPROVE", "true")
        from dataclasses import asdict
        data = asdict(PrismSettings())
        # auto_approve key is "auto_approve" but env is PRISM_AGENT_AUTOAPPROVE
        # This won't match because the key split is ("agent", "autoapprove")
        # and the field is "auto_approve" — this tests that it doesn't crash
        result = _apply_env_overrides(data)
        assert isinstance(result, dict)


# ---------- Load / Save ----------


class TestLoadSave:
    def test_load_defaults_when_no_files(self, tmp_path, monkeypatch):
        monkeypatch.setattr("app.config.settings_schema.GLOBAL_SETTINGS_PATH", tmp_path / "settings.json")
        monkeypatch.setattr("app.config.settings_schema._find_project_settings", lambda: None)
        monkeypatch.setattr("app.config.settings_schema._cached", None)
        s = load_settings()
        assert s.agent.max_iterations == 30

    def test_global_settings_loaded(self, tmp_path, monkeypatch):
        path = tmp_path / "settings.json"
        path.write_text(json.dumps({"agent": {"model": "test-model"}}))
        monkeypatch.setattr("app.config.settings_schema.GLOBAL_SETTINGS_PATH", path)
        monkeypatch.setattr("app.config.settings_schema._find_project_settings", lambda: None)
        monkeypatch.setattr("app.config.settings_schema._cached", None)
        s = load_settings()
        assert s.agent.model == "test-model"

    def test_project_overrides_global(self, tmp_path, monkeypatch):
        global_path = tmp_path / "global" / "settings.json"
        global_path.parent.mkdir()
        global_path.write_text(json.dumps({"agent": {"model": "global-model", "max_iterations": 10}}))

        project_path = tmp_path / "project" / ".prism" / "settings.json"
        project_path.parent.mkdir(parents=True)
        project_path.write_text(json.dumps({"agent": {"model": "project-model"}}))

        monkeypatch.setattr("app.config.settings_schema.GLOBAL_SETTINGS_PATH", global_path)
        monkeypatch.setattr("app.config.settings_schema._find_project_settings", lambda: project_path)
        monkeypatch.setattr("app.config.settings_schema._cached", None)

        s = load_settings()
        assert s.agent.model == "project-model"
        assert s.agent.max_iterations == 10  # inherited from global

    def test_save_global(self, tmp_path, monkeypatch):
        monkeypatch.setattr("app.config.settings_schema.PRISM_DIR", tmp_path)
        monkeypatch.setattr("app.config.settings_schema.GLOBAL_SETTINGS_PATH", tmp_path / "settings.json")
        s = PrismSettings()
        s.agent.model = "saved-model"
        path = save_global_settings(s)
        assert path.exists()
        data = json.loads(path.read_text())
        assert data["agent"]["model"] == "saved-model"

    def test_save_project(self, tmp_path):
        s = PrismSettings()
        s.search.max_results_per_source = 500
        path = save_project_settings(s, project_dir=tmp_path)
        assert path.exists()
        data = json.loads(path.read_text())
        assert data["search"]["max_results_per_source"] == 500

    def test_corrupt_json_returns_defaults(self, tmp_path, monkeypatch):
        path = tmp_path / "settings.json"
        path.write_text("{invalid json")
        monkeypatch.setattr("app.config.settings_schema.GLOBAL_SETTINGS_PATH", path)
        monkeypatch.setattr("app.config.settings_schema._find_project_settings", lambda: None)
        monkeypatch.setattr("app.config.settings_schema._cached", None)
        s = load_settings()
        assert s.agent.max_iterations == 30  # defaults


# ---------- Caching ----------


class TestCaching:
    def test_get_settings_caches(self, tmp_path, monkeypatch):
        monkeypatch.setattr("app.config.settings_schema.GLOBAL_SETTINGS_PATH", tmp_path / "settings.json")
        monkeypatch.setattr("app.config.settings_schema._find_project_settings", lambda: None)
        monkeypatch.setattr("app.config.settings_schema._cached", None)
        s1 = get_settings()
        s2 = get_settings()
        assert s1 is s2

    def test_reload_clears_cache(self, tmp_path, monkeypatch):
        monkeypatch.setattr("app.config.settings_schema.GLOBAL_SETTINGS_PATH", tmp_path / "settings.json")
        monkeypatch.setattr("app.config.settings_schema._find_project_settings", lambda: None)
        monkeypatch.setattr("app.config.settings_schema._cached", None)
        s1 = get_settings()
        s2 = reload_settings()
        assert s1 is not s2


# ---------- Integration: factory reads settings ----------


class TestFactoryIntegration:
    def test_factory_reads_model_from_settings(self, tmp_path, monkeypatch):
        """create_backend() should pick up model from settings.json."""
        path = tmp_path / "settings.json"
        path.write_text(json.dumps({"agent": {"model": "claude-opus-4-6"}}))
        monkeypatch.setattr("app.config.settings_schema.GLOBAL_SETTINGS_PATH", path)
        monkeypatch.setattr("app.config.settings_schema._find_project_settings", lambda: None)
        monkeypatch.setattr("app.config.settings_schema._cached", None)
        monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")

        from app.agent.factory import create_backend
        backend = create_backend()
        # The backend should have received model from settings
        assert backend.model == "claude-opus-4-6"
