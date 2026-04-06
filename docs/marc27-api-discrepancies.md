# MARC27 API Alignment Notes — Node Protocol

**Last updated:** 2026-04-06

This note replaces the older "WS-only node lifecycle" warning.

## Current State

The platform v1 node surface is now live and PRISM is aligned to it:

- `WS /api/v1/nodes/connect?token=...`
- `POST /api/v1/nodes/register`
- `GET /api/v1/nodes`
- `GET /api/v1/nodes/{id}`
- `POST /api/v1/nodes/{id}/heartbeat`
- `DELETE /api/v1/nodes/{id}`
- `GET /api/v1/nodes/{id}/public-key`
- `POST /api/v1/nodes/{id}/exchange-key`

PRISM uses the WebSocket path for the active node daemon and the REST path for:

- discovery
- graceful lifecycle operations
- DB-backed heartbeat freshness
- node-to-node public-key lookup and exchange

## Important API Shapes

`GET /nodes` returns a wrapper object, not a bare array:

```json
{
  "nodes": [{ "id": "...", "name": "...", "status": "online", "profile": {...} }],
  "count": 1
}
```

`GET /nodes/{id}/public-key` returns:

```json
{
  "node_id": "uuid",
  "name": "lab-hpc-01",
  "public_key": "base64-x25519-key",
  "algorithm": "x25519"
}
```

`POST /nodes/{id}/exchange-key` returns:

```json
{
  "target_node_id": "uuid",
  "target_public_key": "base64-x25519-key",
  "algorithm": "x25519",
  "your_public_key_received": "base64-x25519-key"
}
```

## Remaining Caveat

Platform node detail is rich enough for identity, visibility, and E2EE, but direct
federation still depends on nodes advertising usable service endpoints in their
capability profile. If those endpoints are absent, PRISM can discover the peer but
cannot infer a routable `http://host:port` query target automatically.
