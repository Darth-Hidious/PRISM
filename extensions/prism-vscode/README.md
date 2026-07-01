# PRISM VS Code Extension

Status: first-party scaffold for the PRISM/MARC27 developer surface.

This extension is the VS Code client for PRISM. It keeps VS Code where it is
strongest: files, notebooks, terminals, Git, language servers, and review loops.
The Mac app can be the polished control room; this extension is the working
scientist/developer surface.

## What It Wires Today

- Activity-bar container: `PRISM`
- Agent webview: local `prism backend` JSON-RPC over stdio
- Context tree: backend state, workspace root, MARC27 auth state
- MARC27 capability discovery: `GET /api/v1/agent/capabilities`
- Service trees: models, workflows, jobs, billing
- Commands:
  - `PRISM: Open Agent`
  - `PRISM: Start Backend`
  - `PRISM: Stop Backend`
  - `PRISM: Send Selection to Agent`
  - `PRISM: Run Research Query`
  - `PRISM: Query Knowledge`
  - `PRISM: Show Models`
  - `PRISM: Show Workflows`
  - `PRISM: Refresh MARC27 Capabilities`
  - `PRISM: Set MARC27 API Key`

## Local Development

```bash
cd extensions/prism-vscode
npm install
npm run compile
```

Then open this folder in VS Code and run the extension host.

The local backend launch defaults to:

```bash
prism backend --project-root <workspace> --python python3
```

Override the command in VS Code settings when testing local binaries:

```json
{
  "prism.backendCommand": "/Users/siddharthakovid/Downloads/PRISM/target/debug/prism",
  "prism.backendArgs": ["backend"],
  "prism.pythonPath": "python3",
  "prism.marc27ApiBaseUrl": "https://api.marc27.com/api/v1"
}
```

## Credential Handling

MARC27 API keys are stored in VS Code `SecretStorage`. They are never written to
workspace files, settings JSON, logs, or handoff documents. The extension can
read public capabilities without a key.

## Design Map

See `docs/api-revamp-and-extension-map.md` for the API boundary, feature map,
and MARC27 revamp contract this extension is intended to stabilize.
