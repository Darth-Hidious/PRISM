"""Tests for premium labs tools and catalog."""
import json
import pytest
from pathlib import Path
from unittest.mock import patch, MagicMock


class TestLabsCatalog:
    """Test the labs catalog loading."""

    def test_catalog_exists(self):
        catalog_path = Path(__file__).parent.parent / "app" / "plugins" / "labs_catalog.json"
        assert catalog_path.exists()

    def test_catalog_valid_json(self):
        catalog_path = Path(__file__).parent.parent / "app" / "plugins" / "labs_catalog.json"
        data = json.loads(catalog_path.read_text())
        assert "services" in data
        assert len(data["services"]) > 0

    def test_catalog_service_structure(self):
        catalog_path = Path(__file__).parent.parent / "app" / "plugins" / "labs_catalog.json"
        data = json.loads(catalog_path.read_text())
        required_fields = {"name", "category", "provider", "description", "cost_model", "status"}
        for sid, svc in data["services"].items():
            missing = required_fields - set(svc.keys())
            assert not missing, f"Service '{sid}' missing fields: {missing}"

    def test_catalog_categories(self):
        catalog_path = Path(__file__).parent.parent / "app" / "plugins" / "labs_catalog.json"
        data = json.loads(catalog_path.read_text())
        valid_categories = {"a-labs", "dfm", "cloud-dft", "quantum", "synchrotron", "ht-screening"}
        for sid, svc in data["services"].items():
            assert svc["category"] in valid_categories, f"Service '{sid}' has invalid category: {svc['category']}"

    def test_catalog_has_key_services(self):
        catalog_path = Path(__file__).parent.parent / "app" / "plugins" / "labs_catalog.json"
        data = json.loads(catalog_path.read_text())
        sids = set(data["services"].keys())
        assert "matlantis_dft" in sids
        assert "dfm_assessment" in sids
        assert "hqs_quantum" in sids
        assert "alab_synthesis" in sids


class TestLabsTools:
    """Test the labs agent tools."""

    def test_tools_registered(self):
        from app.tools.base import ToolRegistry
        from app.tools.labs import create_labs_tools
        reg = ToolRegistry()
        create_labs_tools(reg)
        names = [t.name for t in reg.list_tools()]
        assert "list_lab_services" in names
        assert "get_lab_service_info" in names
        assert "check_lab_subscriptions" in names
        assert "submit_lab_job" in names

    def test_list_lab_services(self):
        from app.tools.labs import _list_lab_services
        result = _list_lab_services()
        assert "services" in result
        assert "count" in result
        assert result["count"] >= 9

    def test_list_lab_services_filter(self):
        from app.tools.labs import _list_lab_services
        result = _list_lab_services(category="quantum")
        assert result["count"] == 2
        names = [s["name"] for s in result["services"]]
        assert any("HQS" in n for n in names)
        assert any("AQT" in n for n in names)

    def test_get_lab_service_info(self):
        from app.tools.labs import _get_lab_service_info
        result = _get_lab_service_info(service_id="matlantis_dft")
        assert result["name"] == "Matlantis Cloud DFT"
        assert result["category"] == "cloud-dft"
        assert "capabilities" in result
        assert len(result["capabilities"]) > 0

    def test_get_lab_service_info_not_found(self):
        from app.tools.labs import _get_lab_service_info
        result = _get_lab_service_info(service_id="nonexistent")
        assert "error" in result

    def test_check_lab_subscriptions_empty(self):
        from app.tools.labs import _check_lab_subscriptions
        with patch("app.tools.labs._SUBSCRIPTIONS_PATH", Path("/tmp/nonexistent_subs.json")):
            result = _check_lab_subscriptions()
            assert result["count"] == 0
            assert result["subscriptions"] == []

    def test_submit_lab_job_no_service(self):
        from app.tools.labs import _submit_lab_job
        result = _submit_lab_job()
        assert "error" in result

    def test_submit_lab_job_coming_soon(self):
        from app.tools.labs import _submit_lab_job
        result = _submit_lab_job(service_id="matlantis_dft")
        assert "error" in result
        assert "not yet available" in result["error"]

    def test_submit_lab_job_not_found(self):
        from app.tools.labs import _submit_lab_job
        result = _submit_lab_job(service_id="fake_service")
        assert "error" in result

    def test_submit_lab_job_requires_approval(self):
        from app.tools.base import ToolRegistry
        from app.tools.labs import create_labs_tools
        reg = ToolRegistry()
        create_labs_tools(reg)
        tool = reg.get("submit_lab_job")
        assert tool.requires_approval is True


class TestLabsInBootstrap:
    """Test that labs tools appear in the full bootstrap registry."""

    def test_labs_tools_in_bootstrap(self):
        from app.plugins.bootstrap import build_full_registry
        tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in tool_reg.list_tools()}
        assert "list_lab_services" in names
        assert "get_lab_service_info" in names
        assert "submit_lab_job" in names


class TestCalphadUnderModel:
    """Test that CALPHAD is accessible under model calphad."""

    def test_model_calphad_group_exists(self):
        from app.commands.model import model
        assert "calphad" in model.commands

    def test_calphad_subcommands(self):
        from app.commands.model import model
        calphad_grp = model.commands["calphad"]
        cmd_names = list(calphad_grp.commands.keys())
        assert "status" in cmd_names
        assert "databases" in cmd_names
        assert "import" in cmd_names
