# PRISM Notebook Orchestration — Design

> Status: research-complete, ready to build.
> Date: 2026-07-02
> Owner: PRISM TUI/CLI team

## Goal

Let users launch, manage, and connect to Jupyter notebooks from PRISM —
locally or on remote compute (MARC27 nodes) — with automatic port
forwarding and IDE connectivity (VS Code, JupyterLab), using their PRISM
account for auth and compute routing. Like Google Colab, but self-hosted
and orchestrated through the PRISM CLI/TUI.

## Research findings (informed the design)

### Google Colab architecture
- Kernel runs on a GCE instance; notebook UI is browser-based.
- Port forwarding via `google.colab.kernel.proxyPort(port)` — a per-port
  subdomain proxied through the Colab frontend.
- Local runtime: connect via SSH local forwarding
  (`ssh -N -L 8888:localhost:8888 user@host`).
- Drive/storage mounted into the kernel container.

### Jupyter Server API
- Full REST API for kernel lifecycle: `POST /api/kernels` (start),
  `DELETE /api/kernels/<id>` (stop), `POST /api/kernels/<id>/restart`.
- `MultiKernelManager` / `MappingKernelManager` manage multiple kernels.
- Each kernel uses 5 ZMQ channels (shell, iopub, stdin, control, hb) on
  separate ports — tunneling must forward all 5.
- Headless launch: `jupyter lab --no-browser --port=0 --ServerApp.token=<T>`.

### Port forwarding / IDE
- **SSH local forward**: `ssh -L local:remote` — standard, reliable.
- **VS Code Remote-SSH**: installs VS Code Server on remote; all
  communication through SSH tunnel; native port forwarding in the UI.
- **jupyter-remote-kernel** (PyPI): registers a kernel spec so a remote
  kernel appears in JupyterLab/VS Code kernel selector automatically.
  Uses paramiko/sshtunnel for the 5 ZMQ channels.
- **Pattern**: Launch headless on remote → tunnel port 8888 → open
  `localhost:8888` in browser or connect IDE.

### What PRISM already has
- `crates/mesh/`: cross-site networking, federation, burst routing.
- `crates/compute/`: local, MARC27, BYOC backends. Docker-based.
- `crates/node/`: node lifecycle (`prism node up`), model detection.
- `~/.prism/venv/`: Python venv with Jupyter pre-installed.
- MARC27 auth (device flow) + compute API (26 GPU types).

## Architecture

```
┌─ User ─────────────────────────────────────────────────────────┐
│  Browser (JupyterLab)  ·  VS Code (Remote-SSH + Jupyter ext)   │
└──────────┬──────────────────────────────────┬──────────────────┘
           │ localhost:PORT                    │ SSH tunnel
           │                                  │
┌──────────▼──────────────────────────────────▼──────────────────┐
│  PRISM CLI / TUI                                               │
│  ┌─ notebook manager ─────────────────────────────────────┐    │
│  │  start / stop / list / tunnel / connect                │    │
│  │  port allocation · PID tracking · token generation     │    │
│  └────────────────────────────────────────────────────────┘    │
│  ┌─ mesh tunnel ──────────────────────────────────────────┐    │
│  │  local ↔ MARC27 node (SSH or mesh relay)               │    │
│  └────────────────────────────────────────────────────────┘    │
└──────────┬─────────────────────────────────────────────────────┘
           │
   ┌───────┴───────┐
   ▼               ▼
┌──────────┐  ┌──────────────────┐
│ Local    │  │ MARC27 Compute   │
│ venv     │  │ Docker container │
│ Jupyter  │  │ Jupyter + GPU    │
│ port:N   │  │ port:N           │
└──────────┘  └──────────────────┘
```

## CLI surface

```bash
# Local notebook (default)
prism notebook start [--port N] [--notebook PATH] [--no-browser]
# → launches jupyter lab in ~/.prism/venv, auto-port, prints URL + token.

# On a remote MARC27 compute node
prism notebook start --remote [--gpu A100] [--image prism-jupyter:latest]
# → launches Jupyter in a container on a MARC27 node via the mesh,
#   tunnels port back to localhost, prints URL.

# List active notebook sessions
prism notebook list
# → PID · port · URL · kernel count · uptime · local/remote.

# Stop a notebook
prism notebook stop [<pid|port|all>]

# Forward a port from the notebook's kernel (like Colab's proxyPort)
prism notebook forward <remote-port> [--local-port N]
# → SSH-tunnels a kernel port to localhost (for mlflow, tensorboard, etc.).

# Register kernel spec for IDE
prism notebook connect [--vscode] [--jupyter]
# → registers a kernel spec so VS Code / JupyterLab see the PRISM notebook
#   as a selectable kernel. Uses SSH tunnel for the 5 ZMQ channels.
```

## TUI surface

A **Notebook panel** (palette `notebook.show`):
- Lists active sessions: port · URL · kernel count · status (local/remote).
- Actions: `[o]` open in browser · `[s]` stop · `[t]` tunnel port · `[c]` copy URL.
- Shows the token for copy-paste into a browser.
- If `--remote`: shows the node name + GPU type + tunnel status.

## Protocol (JSON-RPC over the existing IPC)

New notifications:
- `ui.notebook.list` — `{sessions: [{pid, port, url, token, kernels, remote, node}]}`
- `ui.notebook.started` — `{pid, port, url, token}` (response to `/notebook start`)
- `ui.notebook.stopped` — `{pid}` (response to `/notebook stop`)
- `ui.notebook.forwarded` — `{remote_port, local_port, url}`

New commands (TUI → backend):
- `/notebook start [--port N] [--remote]`
- `/notebook stop <pid|all>`
- `/notebook list`
- `/notebook forward <port>`

## Implementation slices (each verifiable)

### Slice 1: Local launch + list + stop (CLI + TUI)
- `prism notebook start` → spawn `jupyter lab --no-browser --port=0
  --ServerApp.token=<random>` in `~/.prism/venv/bin/python`.
- Parse stdout for the URL + token.
- Track PID + port + URL in `~/.prism/notebooks.json`.
- `prism notebook list` → read notebooks.json, print table.
- `prism notebook stop <pid>` → kill process, remove from json.
- TUI: `notebook.show` panel → list sessions + open/stop/copy.
- Fake backend: simulate `/notebook list` → ui.notebook.list.
- Test: spawn → parse URL → list → stop → verify PID gone.

### Slice 2: Port forwarding (local kernel ports)
- `prism notebook forward <port>` → `ssh -N -L <local>:localhost:<port>
  -o StrictHostKeyChecking=no` for local notebooks (passthrough to kernel).
- For remote notebooks: tunnel via the mesh relay.
- TUI: forward action in the notebook panel.
- Test: forward a port, verify reachable, stop.

### Slice 3: Remote launch (MARC27 compute)
- `prism notebook start --remote` → submit a compute job
  (`prism run --image prism-jupyter:latest --json`).
- Job spawns Jupyter in the container on the MARC27 node.
- Mesh tunnel: relay port 8888 from the node back to localhost.
- Token generated server-side, returned via the job result.
- TUI: notebook panel shows remote sessions with node + GPU info.
- Test: fake the compute job, simulate the tunnel.

### Slice 4: IDE connectivity (kernel spec registration)
- `prism notebook connect --vscode` → register a kernel spec in
  `~/.local/share/jupyter/kernels/prism-<session>/kernel.json` that
  launches a tunnel + connects to the PRISM notebook kernel.
- VS Code Jupyter extension sees the kernel → selectable in the dropdown.
- Test: verify kernel.json is written correctly, spec is valid.

## Security

- Token-based auth (Jupyter `ServerApp.token`) — generated per session,
  never persisted in plaintext beyond notebooks.json (which is mode 0600).
- Remote notebooks: MARC27 auth gates compute access; SSH keys managed
  by the mesh, never exposed to the user.
- Port forwarding: only localhost by default; `--expose` flag required to
  bind 0.0.0.0 (with a warning).
- notebooks.json: mode 0600, contains PID/port/token only (no credentials).

## Dependencies

- Jupyter already installed in `~/.prism/venv/` (verified).
- SSH: system `ssh` for tunneling (available on macOS/Linux).
- Mesh: existing crates/mesh for remote relay.
- Compute: existing crates/compute for MARC27 job submission.

## Open questions

1. Should remote notebooks use Docker (consistent env) or bare venv on
   the node? (Recommend Docker for reproducibility.)
2. Should the notebook manager live in the CLI process or the backend
   subprocess? (Recommend CLI — notebooks are long-lived, independent of
   the agent backend.)
3. Multiple notebooks simultaneously? (Recommend yes — each gets its own
   port + PID, tracked in notebooks.json.)
