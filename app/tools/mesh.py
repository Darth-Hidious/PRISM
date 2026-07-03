"""Mesh networking tools — discover peers, publish datasets, manage
subscriptions, and inspect mesh health from the agent.

These tools wrap the local PRISM node's mesh API at
``http://<host>:<port>/api/mesh/*``.  The node URL is resolved in this
priority order:

1. ``PRISM_NODE_URL`` env var (full URL, overrides everything).
2. ``PRISM_NODE_PORT`` env var (port only; host defaults to 127.0.0.1).
3. ``~/.prism/prism.toml`` ``[node] port`` (host defaults to 127.0.0.1).
4. ``./.prism/prism.toml`` ``[node] port``.
5. Fallback: ``http://127.0.0.1:7327``.

The node must be running (``prism node up``) for these tools to work —
they return a clear offline error when it's not.

All tools return structured JSON dicts.  Read-only tools (peers,
subscriptions, health) have no approval gate.  State-changing tools
(publish, subscribe, unsubscribe) are approval-gated because they
affect cross-node data sharing.

Auth: the local node uses a session token from
``~/.prism/cli-state.json``.  The tools read this automatically.  In
offline mode or when the node is down, they return
``{"error": "...", "offline": true}`` without raising.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any, Optional

import requests

from app.tools.base import Tool, ToolRegistry

# ── Constants ──────────────────────────────────────────────────────

DEFAULT_NODE_HOST = "127.0.0.1"
DEFAULT_NODE_PORT = 7327

MESH_TIMEOUT = 10  # seconds for read ops
MESH_WRITE_TIMEOUT = 15  # seconds for state-changing ops


# ── Node URL resolution ─────────────────────────────────────────────


def _resolve_node_port() -> int:
    """Resolve the node port from env vars or prism.toml.

    Priority:
    1. PRISM_NODE_PORT env var.
    2. ~/.prism/prism.toml [node] port.
    3. ./.prism/prism.toml [node] port.
    4. Default: 7327.
    """
    # 1. Env var
    env_port = os.environ.get("PRISM_NODE_PORT")
    if env_port:
        try:
            return int(env_port)
        except ValueError:
            pass

    # 2-3. prism.toml (try global then project)
    for config_path in [
        Path.home() / ".prism" / "prism.toml",
        Path.cwd() / ".prism" / "prism.toml",
    ]:
        if config_path.exists():
            try:
                # Python 3.11+ has tomllib; fall back to `toml` if needed.
                try:
                    import tomllib as _toml_mod
                    with open(config_path, "rb") as f:
                        config = _toml_mod.load(f)
                except ImportError:
                    import toml as _toml_fallback
                    with open(config_path) as f:
                        config = _toml_fallback.load(f)
                port = config.get("node", {}).get("port")
                if port:
                    return int(port)
            except Exception:
                pass

    # 4. Default
    return DEFAULT_NODE_PORT


def _node_url() -> str:
    """Return the base URL of the local PRISM node.

    Resolved from PRISM_NODE_URL (full URL override), or
    PRISM_NODE_PORT + host, or prism.toml [node] port, or default.
    """
    # Full URL override
    full_url = os.environ.get("PRISM_NODE_URL")
    if full_url:
        return full_url.rstrip("/")

    # Port from env or config, host from env or default
    port = _resolve_node_port()
    host = os.environ.get("PRISM_NODE_HOST", DEFAULT_NODE_HOST)
    return f"http://{host}:{port}"


def _session_token() -> str:
    """Read the session token from ~/.prism/cli-state.json."""
    try:
        state_path = Path.home() / ".prism" / "cli-state.json"
        if state_path.exists():
            data = json.loads(state_path.read_text())
            creds = data.get("credentials") or {}
            return creds.get("access_token", "")
    except Exception:
        pass
    return ""


def _get(path: str) -> dict:
    """GET helper for the local node mesh API.

    Returns dict with 'error' on failure, parsed JSON otherwise.
    """
    url = f"{_node_url()}{path}"
    token = _session_token()
    headers = {}
    if token:
        headers["X-Session-Token"] = token
    try:
        resp = requests.get(url, headers=headers, timeout=MESH_TIMEOUT)
        if resp.status_code == 404:
            return {
                "error": "PRISM node is not running or mesh API is unavailable.",
                "offline": True,
                "hint": "Start the node with: prism node up",
            }
        if resp.status_code != 200:
            return {
                "error": f"node returned HTTP {resp.status_code}",
                "body": resp.text[:500],
            }
        return resp.json()
    except requests.exceptions.ConnectionError:
        return {
            "error": "Cannot connect to PRISM node. Is it running?",
            "offline": True,
            "hint": "Start the node with: prism node up",
        }
    except requests.exceptions.RequestException as e:
        return {"error": f"network error: {e}"}


def _post(path: str, body: dict) -> dict:
    """POST helper for the local node mesh API."""
    url = f"{_node_url()}{path}"
    token = _session_token()
    headers = {"Content-Type": "application/json"}
    if token:
        headers["X-Session-Token"] = token
    try:
        resp = requests.post(
            url, headers=headers, json=body, timeout=MESH_WRITE_TIMEOUT
        )
        if resp.status_code == 404:
            return {
                "error": "PRISM node is not running or mesh API is unavailable.",
                "offline": True,
                "hint": "Start the node with: prism node up",
            }
        if not (200 <= resp.status_code < 300):
            return {
                "error": f"node returned HTTP {resp.status_code}",
                "body": resp.text[:500],
            }
        # Some endpoints return 200 with empty body
        text = resp.text.strip()
        if text:
            try:
                return resp.json()
            except Exception:
                return {"status": "ok", "raw": text}
        return {"status": "ok"}
    except requests.exceptions.ConnectionError:
        return {
            "error": "Cannot connect to PRISM node. Is it running?",
            "offline": True,
            "hint": "Start the node with: prism node up",
        }
    except requests.exceptions.RequestException as e:
        return {"error": f"network error: {e}"}


def _delete(path: str, body: dict) -> dict:
    """DELETE helper (with JSON body) for the local node mesh API."""
    url = f"{_node_url()}{path}"
    token = _session_token()
    headers = {"Content-Type": "application/json"}
    if token:
        headers["X-Session-Token"] = token
    try:
        resp = requests.delete(
            url, headers=headers, json=body, timeout=MESH_WRITE_TIMEOUT
        )
        if resp.status_code == 404:
            return {
                "error": "PRISM node is not running or mesh API is unavailable.",
                "offline": True,
                "hint": "Start the node with: prism node up",
            }
        if not (200 <= resp.status_code < 300):
            return {
                "error": f"node returned HTTP {resp.status_code}",
                "body": resp.text[:500],
            }
        text = resp.text.strip()
        if text:
            try:
                return resp.json()
            except Exception:
                return {"status": "ok", "raw": text}
        return {"status": "ok"}
    except requests.exceptions.ConnectionError:
        return {
            "error": "Cannot connect to PRISM node. Is it running?",
            "offline": True,
            "hint": "Start the node with: prism node up",
        }
    except requests.exceptions.RequestException as e:
        return {"error": f"network error: {e}"}


# ── Tool functions ──────────────────────────────────────────────────


def _mesh_peers(**_kwargs: Any) -> dict:
    """List known mesh peers from the running node.

    Returns:
        {"online": bool, "node_id": str, "peers": [...]} on success.
        {"error": str, "offline": True} when node is down.
    """
    return _get("/api/mesh/nodes")


_MESH_PEERS_DESCRIPTION = (
    "List all known mesh peers connected to this PRISM node. Shows peer "
    "name, address, port, capabilities, and last-seen timestamp. Use "
    "this to discover which nodes are available for cross-site inference, "
    "dataset sharing, or federated queries. Read-only; no approval gate. "
    "Requires the local node to be running (prism node up)."
)


def _mesh_health(**_kwargs: Any) -> dict:
    """Check mesh subsystem health (online/offline + peer count + uptime).

    Returns:
        {"online": bool, "node_id": str, "peer_count": int, ...}
    """
    data = _get("/api/mesh/nodes")
    if data.get("offline"):
        return data
    return {
        "online": data.get("online", False),
        "node_id": data.get("node_id", "unknown"),
        "peer_count": len(data.get("peers", [])),
        "raw": data,
    }


_MESH_HEALTH_DESCRIPTION = (
    "Quick health check for the mesh subsystem. Returns online status, "
    "this node's ID, and peer count. Cheaper than mesh_peers — use this "
    "first to check if the mesh is alive before listing peers or "
    "publishing datasets. Read-only; no approval gate. Requires the "
    "local node to be running (prism node up)."
)


def _mesh_publish(name: str, schema_version: str = "1.0", **_kwargs: Any) -> dict:
    """Publish a dataset to the mesh so other nodes can subscribe.

    Args:
        name: dataset name (e.g. "cfd-2024-v3").
        schema_version: schema version string (default "1.0").
    """
    if not name:
        return {"error": "Missing required argument `name`."}
    return _post("/api/mesh/publish", {
        "name": name,
        "schema_version": schema_version,
    })


_MESH_PUBLISH_DESCRIPTION = (
    "Publish a local dataset to the PRISM mesh so other nodes can "
    "discover and subscribe to it. The dataset becomes visible to peers "
    "via mDNS and the mesh sync protocol. Subscribers receive automatic "
    "updates when the dataset changes.\n"
    "  - name: required. Dataset name (e.g. 'cfd-2024-v3').\n"
    "  - schema_version: optional, default '1.0'.\n"
    "Approval-gated — publishing makes local data available to other "
    "nodes, which is a cross-node data-sharing operation. Requires the "
    "local node to be running (prism node up)."
)


def _mesh_subscribe(dataset_name: str, publisher: str, **_kwargs: Any) -> dict:
    """Subscribe to a dataset published by a remote node.

    Args:
        dataset_name: name of the dataset to subscribe to.
        publisher: UUID of the publishing node.
    """
    if not dataset_name:
        return {"error": "Missing required argument `dataset_name`."}
    if not publisher:
        return {"error": "Missing required argument `publisher`."}
    return _post("/api/mesh/subscribe", {
        "dataset_name": dataset_name,
        "publisher_node": publisher,
    })


_MESH_SUBSCRIBE_DESCRIPTION = (
    "Subscribe to a dataset published by another PRISM node on the mesh. "
    "Once subscribed, this node automatically receives updates when the "
    "publisher changes the dataset. Use mesh_peers first to find the "
    "publisher's node UUID.\n"
    "  - dataset_name: required. Name of the published dataset.\n"
    "  - publisher: required. UUID of the publishing node.\n"
    "Approval-gated — subscribing creates a cross-node data-sharing "
    "relationship. Requires the local node to be running (prism node up)."
)


def _mesh_unsubscribe(dataset_name: str, publisher: str, **_kwargs: Any) -> dict:
    """Unsubscribe from a remote dataset.

    Args:
        dataset_name: name of the dataset to unsubscribe from.
        publisher: UUID of the publishing node.
    """
    if not dataset_name:
        return {"error": "Missing required argument `dataset_name`."}
    if not publisher:
        return {"error": "Missing required argument `publisher`."}
    return _delete("/api/mesh/subscribe", {
        "dataset_name": dataset_name,
        "publisher_node": publisher,
    })


_MESH_UNSUBSCRIBE_DESCRIPTION = (
    "Unsubscribe from a dataset published by another node. Stops "
    "receiving automatic updates for that dataset. Does NOT delete "
    "already-synced data.\n"
    "  - dataset_name: required. Name of the dataset.\n"
    "  - publisher: required. UUID of the publishing node.\n"
    "Approval-gated — changes cross-node data-sharing relationships. "
    "Requires the local node to be running (prism node up)."
)


def _mesh_subscriptions(**_kwargs: Any) -> dict:
    """Show both published datasets and active subscriptions.

    Returns:
        {"published": [...], "subscribed": [...]}
    """
    return _get("/api/mesh/subscriptions")


_MESH_SUBSCRIPTIONS_DESCRIPTION = (
    "Show all datasets this node has published and all datasets it is "
    "currently subscribed to from other nodes. Returns two lists: "
    "'published' (datasets this node shares) and 'subscribed' (datasets "
    "from remote nodes). Each entry includes name, schema version, and "
    "subscriber/publisher count. Read-only; no approval gate. Requires "
    "the local node to be running (prism node up)."
)


# ── Registration ─────────────────────────────────────────────────────


def create_mesh_tools(registry: ToolRegistry) -> None:
    """Register mesh networking tools (peers, health, publish, subscribe,
    unsubscribe, subscriptions).

    Read-only tools: mesh_peers, mesh_health, mesh_subscriptions.
    Approval-gated tools: mesh_publish, mesh_subscribe, mesh_unsubscribe.
    """
    # Read-only: list peers
    registry.register(Tool(
        name="mesh_peers",
        description=_MESH_PEERS_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        },
        func=_mesh_peers,
    ))

    # Read-only: health check
    registry.register(Tool(
        name="mesh_health",
        description=_MESH_HEALTH_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        },
        func=_mesh_health,
    ))

    # Read-only: subscriptions list
    registry.register(Tool(
        name="mesh_subscriptions",
        description=_MESH_SUBSCRIPTIONS_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        },
        func=_mesh_subscriptions,
    ))

    # Approval-gated: publish dataset
    registry.register(Tool(
        name="mesh_publish",
        description=_MESH_PUBLISH_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Dataset name to publish (e.g. 'cfd-2024-v3').",
                },
                "schema_version": {
                    "type": "string",
                    "description": "Schema version string. Default '1.0'.",
                    "default": "1.0",
                },
            },
            "required": ["name"],
            "additionalProperties": False,
        },
        func=_mesh_publish,
        requires_approval=True,
    ))

    # Approval-gated: subscribe to remote dataset
    registry.register(Tool(
        name="mesh_subscribe",
        description=_MESH_SUBSCRIBE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "dataset_name": {
                    "type": "string",
                    "description": "Name of the dataset to subscribe to.",
                },
                "publisher": {
                    "type": "string",
                    "description": "UUID of the publishing node.",
                },
            },
            "required": ["dataset_name", "publisher"],
            "additionalProperties": False,
        },
        func=_mesh_subscribe,
        requires_approval=True,
    ))

    # Approval-gated: unsubscribe from remote dataset
    registry.register(Tool(
        name="mesh_unsubscribe",
        description=_MESH_UNSUBSCRIBE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "dataset_name": {
                    "type": "string",
                    "description": "Name of the dataset to unsubscribe from.",
                },
                "publisher": {
                    "type": "string",
                    "description": "UUID of the publishing node.",
                },
            },
            "required": ["dataset_name", "publisher"],
            "additionalProperties": False,
        },
        func=_mesh_unsubscribe,
        requires_approval=True,
    ))