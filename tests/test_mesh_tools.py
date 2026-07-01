"""Tests for mesh networking tools.

These tests verify tool registration, schema validation, and offline
behavior without requiring a running PRISM node.  No real network
calls — the node is expected to be down, so all tools should return
structured offline errors.
"""
import json
import os
from pathlib import Path
from unittest.mock import patch, MagicMock

import pytest

from app.plugins.bootstrap import build_full_registry
from app.tools.base import ToolRegistry
from app.tools.mesh import (
    create_mesh_tools,
    _mesh_peers,
    _mesh_health,
    _mesh_publish,
    _mesh_subscribe,
    _mesh_unsubscribe,
    _mesh_subscriptions,
    _node_url,
    _resolve_node_port,
    _session_token,
)


# ── Registration tests ─────────────────────────────────────────────


class TestMeshToolRegistration:
    """Verify all 6 mesh tools are registered with correct metadata."""

    def test_six_mesh_tools_registered(self):
        reg = ToolRegistry()
        create_mesh_tools(reg)
        names = {t.name for t in reg.list_tools()}
        assert names == {
            "mesh_peers",
            "mesh_health",
            "mesh_subscriptions",
            "mesh_publish",
            "mesh_subscribe",
            "mesh_unsubscribe",
        }

    def test_read_only_tools_no_approval(self):
        reg = ToolRegistry()
        create_mesh_tools(reg)
        for t in reg.list_tools():
            if t.name in ("mesh_peers", "mesh_health", "mesh_subscriptions"):
                assert not t.requires_approval, f"{t.name} should not require approval"

    def test_state_changing_tools_require_approval(self):
        reg = ToolRegistry()
        create_mesh_tools(reg)
        for t in reg.list_tools():
            if t.name in ("mesh_publish", "mesh_subscribe", "mesh_unsubscribe"):
                assert t.requires_approval, f"{t.name} should require approval"

    def test_schemas_have_required_fields(self):
        reg = ToolRegistry()
        create_mesh_tools(reg)
        tool = reg.get("mesh_publish")
        assert "name" in tool.input_schema.get("required", [])
        tool = reg.get("mesh_subscribe")
        assert "dataset_name" in tool.input_schema.get("required", [])
        assert "publisher" in tool.input_schema.get("required", [])

    def test_bootstrap_registers_mesh_tools(self):
        """Verify mesh tools are registered in the full bootstrap registry."""
        reg, _, _ = build_full_registry(enable_mcp=False)
        names = {t.name for t in reg.list_tools() if t.name.startswith("mesh_")}
        assert len(names) == 6

    def test_descriptions_are_keyword_rich(self):
        """Descriptions must mention mesh, peers, subscribe, publish, node."""
        reg = ToolRegistry()
        create_mesh_tools(reg)
        for t in reg.list_tools():
            desc = t.description.lower()
            assert len(desc) > 50, f"{t.name} description too short"
            # Each description should mention "mesh" or "node"
            assert "mesh" in desc or "node" in desc, f"{t.name} description lacks mesh/node keyword"


# ── Offline behavior tests ──────────────────────────────────────────


class TestMeshOfflineBehavior:
    """When the node is down, tools return structured offline errors."""

    def test_peers_offline(self):
        result = _mesh_peers()
        assert result.get("offline") is True
        assert "error" in result
        assert "node" in result["error"].lower()

    def test_health_offline(self):
        result = _mesh_health()
        assert result.get("offline") is True

    def test_subscriptions_offline(self):
        result = _mesh_subscriptions()
        assert result.get("offline") is True

    def test_publish_offline(self):
        result = _mesh_publish(name="test-dataset")
        assert result.get("offline") is True

    def test_subscribe_offline(self):
        result = _mesh_subscribe(dataset_name="ds", publisher="node-uuid")
        assert result.get("offline") is True

    def test_unsubscribe_offline(self):
        result = _mesh_unsubscribe(dataset_name="ds", publisher="node-uuid")
        assert result.get("offline") is True


# ── Input validation tests ─────────────────────────────────────────


class TestMeshInputValidation:
    """Verify missing required args return structured errors, not crashes."""

    def test_publish_missing_name(self):
        result = _mesh_publish(name="")
        assert "error" in result
        assert "name" in result["error"]

    def test_subscribe_missing_dataset_name(self):
        result = _mesh_subscribe(dataset_name="", publisher="node")
        assert "error" in result
        assert "dataset_name" in result["error"]

    def test_subscribe_missing_publisher(self):
        result = _mesh_subscribe(dataset_name="ds", publisher="")
        assert "error" in result
        assert "publisher" in result["error"]

    def test_unsubscribe_missing_dataset_name(self):
        result = _mesh_unsubscribe(dataset_name="", publisher="node")
        assert "error" in result

    def test_unsubscribe_missing_publisher(self):
        result = _mesh_unsubscribe(dataset_name="ds", publisher="")
        assert "error" in result


# ── Mock node tests ─────────────────────────────────────────────────


class TestMeshMockNode:
    """Test with a mocked HTTP response to verify JSON parsing."""

    def test_peers_with_mock_node(self):
        """Simulate a running node returning peer data."""
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = {
            "online": True,
            "node_id": "test-node-123",
            "peers": [
                {"name": "peer-1", "address": "10.0.0.5", "port": 7327, "last_seen": "2026-06-29"},
            ],
        }
        with patch("app.tools.mesh.requests.get", return_value=mock_resp):
            result = _mesh_peers()
        assert result["online"] is True
        assert result["node_id"] == "test-node-123"
        assert len(result["peers"]) == 1
        assert result["peers"][0]["name"] == "peer-1"

    def test_health_with_mock_node(self):
        """Simulate a running node with peers."""
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = {
            "online": True,
            "node_id": "node-abc",
            "peers": [
                {"name": "p1"},
                {"name": "p2"},
            ],
        }
        with patch("app.tools.mesh.requests.get", return_value=mock_resp):
            result = _mesh_health()
        assert result["online"] is True
        assert result["peer_count"] == 2

    def test_subscriptions_with_mock_node(self):
        """Simulate a node with published and subscribed datasets."""
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = {
            "published": [
                {"name": "my-dataset", "schema_version": "1.0", "subscriber_count": 2},
            ],
            "subscribed": [
                {"name": "remote-ds", "publisher": "node-xyz"},
            ],
        }
        with patch("app.tools.mesh.requests.get", return_value=mock_resp):
            result = _mesh_subscriptions()
        assert len(result["published"]) == 1
        assert result["published"][0]["name"] == "my-dataset"
        assert len(result["subscribed"]) == 1

    def test_publish_with_mock_node(self):
        """Simulate successful publish."""
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.text = '{"status":"published"}'
        mock_resp.json.return_value = {"status": "published"}
        with patch("app.tools.mesh.requests.post", return_value=mock_resp):
            result = _mesh_publish(name="test-ds", schema_version="2.0")
        assert result.get("status") == "published"

    def test_subscribe_with_mock_node(self):
        """Simulate successful subscribe."""
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.text = '{"status":"subscribed"}'
        mock_resp.json.return_value = {"status": "subscribed"}
        with patch("app.tools.mesh.requests.post", return_value=mock_resp):
            result = _mesh_subscribe(dataset_name="ds", publisher="uuid-123")
        assert result.get("status") == "subscribed"

    def test_unsubscribe_with_mock_node(self):
        """Simulate successful unsubscribe."""
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.text = '{"status":"unsubscribed"}'
        mock_resp.json.return_value = {"status": "unsubscribed"}
        with patch("app.tools.mesh.requests.delete", return_value=mock_resp):
            result = _mesh_unsubscribe(dataset_name="ds", publisher="uuid-123")
        assert result.get("status") == "unsubscribed"

    def test_peers_node_returns_error(self):
        """Simulate a node returning an error status."""
        mock_resp = MagicMock()
        mock_resp.status_code = 500
        mock_resp.text = "internal error"
        with patch("app.tools.mesh.requests.get", return_value=mock_resp):
            result = _mesh_peers()
        assert "error" in result
        assert "500" in result["error"]


# ── Config tests ─────────────────────────────────────────────────────


class TestMeshConfig:
    """Verify node URL resolution from env vars and config."""

    def test_node_url_default(self):
        os.environ.pop("PRISM_NODE_URL", None)
        os.environ.pop("PRISM_NODE_PORT", None)
        os.environ.pop("PRISM_NODE_HOST", None)
        url = _node_url()
        assert url == "http://127.0.0.1:7327"

    def test_node_url_full_override(self):
        os.environ["PRISM_NODE_URL"] = "http://10.0.0.5:9999/"
        url = _node_url()
        assert url == "http://10.0.0.5:9999"
        os.environ.pop("PRISM_NODE_URL", None)

    def test_node_port_override(self):
        os.environ.pop("PRISM_NODE_URL", None)
        os.environ["PRISM_NODE_PORT"] = "8080"
        url = _node_url()
        assert url == "http://127.0.0.1:8080"
        os.environ.pop("PRISM_NODE_PORT", None)

    def test_node_host_override(self):
        os.environ.pop("PRISM_NODE_URL", None)
        os.environ.pop("PRISM_NODE_PORT", None)
        os.environ["PRISM_NODE_HOST"] = "10.0.0.5"
        url = _node_url()
        assert url == "http://10.0.0.5:7327"
        os.environ.pop("PRISM_NODE_HOST", None)

    def test_node_url_overrides_port_and_host(self):
        """PRISM_NODE_URL takes full precedence over PORT/HOST."""
        os.environ["PRISM_NODE_URL"] = "http://override:1234"
        os.environ["PRISM_NODE_PORT"] = "9999"
        os.environ["PRISM_NODE_HOST"] = "other-host"
        url = _node_url()
        assert url == "http://override:1234"
        os.environ.pop("PRISM_NODE_URL", None)
        os.environ.pop("PRISM_NODE_PORT", None)
        os.environ.pop("PRISM_NODE_HOST", None)

    def test_invalid_port_falls_back_to_default(self):
        os.environ.pop("PRISM_NODE_URL", None)
        os.environ["PRISM_NODE_PORT"] = "not-a-number"
        url = _node_url()
        assert url == "http://127.0.0.1:7327"
        os.environ.pop("PRISM_NODE_PORT", None)

    def test_session_token_missing(self):
        """When no cli-state.json exists, session token is empty."""
        with patch.dict(os.environ, {"HOME": "/tmp/nonexistent-home-xyz"}):
            token = _session_token()
        assert token == ""