"""Tests for unified plugin catalog."""
import json
from pathlib import Path


def test_catalog_file_exists():
    path = Path(__file__).parent.parent / "app" / "plugins" / "catalog.json"
    assert path.exists(), "app/plugins/catalog.json not found"


def test_catalog_is_valid_json():
    path = Path(__file__).parent.parent / "app" / "plugins" / "catalog.json"
    data = json.loads(path.read_text())
    assert "plugins" in data
    assert "_meta" in data


def test_catalog_entries_have_type():
    path = Path(__file__).parent.parent / "app" / "plugins" / "catalog.json"
    data = json.loads(path.read_text())
    for pid, entry in data["plugins"].items():
        assert "type" in entry, f"catalog entry {pid} missing 'type' field"


def test_catalog_provider_entries_have_base_url():
    path = Path(__file__).parent.parent / "app" / "plugins" / "catalog.json"
    data = json.loads(path.read_text())
    for pid, entry in data["plugins"].items():
        if entry.get("type") == "provider":
            assert "base_url" in entry, f"provider {pid} missing base_url"


def test_catalog_agent_entries_have_system_prompt():
    path = Path(__file__).parent.parent / "app" / "plugins" / "catalog.json"
    data = json.loads(path.read_text())
    for pid, entry in data["plugins"].items():
        if entry.get("type") == "agent":
            assert "system_prompt" in entry, f"agent {pid} missing system_prompt"


def test_old_marketplace_json_deleted():
    path = Path(__file__).parent.parent / "app" / "search" / "marketplace.json"
    assert not path.exists(), "app/search/marketplace.json should be deleted (moved to catalog)"
