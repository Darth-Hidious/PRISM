# prism serve — MCP Server Mode

Start PRISM as an MCP (Model Context Protocol) server, exposing all 26 tools
and resources to external LLM hosts like Claude Desktop, custom agents, or
any MCP-compatible client.

## Usage

```bash
# stdio transport (Claude Desktop, terminal clients)
prism serve

# HTTP transport (web clients, remote access)
prism serve --transport http --port 8000

# Bind to all interfaces (for remote/container access)
prism serve --transport http --host 0.0.0.0 --port 8000

# Print Claude Desktop config JSON
prism serve --install

# Print nginx reverse-proxy config
prism serve --generate-nginx
prism serve --generate-nginx --port 9000   # custom port
```

## Options

| Flag | Default | Description |
|------|---------|-------------|
| `--transport` | `stdio` | Transport protocol: `stdio` or `http` |
| `--host` | `127.0.0.1` | Bind address (`0.0.0.0` for all interfaces) |
| `--port` | `8000` | HTTP port (only used with `--transport http`) |
| `--install` | — | Print Claude Desktop configuration JSON and exit |
| `--generate-nginx` | — | Print nginx reverse-proxy configuration and exit |

---

## Transports

### stdio (default)

For local use with Claude Desktop or any MCP client that spawns PRISM as a
subprocess. Communication happens over stdin/stdout.

```bash
prism serve
```

### HTTP (streamable-http)

For remote access, web clients, or multi-client setups. Uses FastMCP's
streamable-http transport (SSE-based streaming over HTTP).

```bash
prism serve --transport http --port 8000
```

The MCP endpoint is at `http://<host>:<port>/mcp`.

---

## Claude Desktop Integration

Generate the config entry for Claude Desktop:

```bash
prism serve --install
```

Output:
```json
{
  "mcpServers": {
    "prism": {
      "command": "/path/to/python",
      "args": ["-m", "app.cli", "serve"]
    }
  }
}
```

Add this to `~/.claude/claude_desktop_config.json` (or merge into existing).

---

## Deploying with nginx

For production or shared-server deployments, run PRISM behind nginx as a
reverse proxy. This gives you:

- Standard port 80/443 access
- SSL termination (add your certs)
- Rate limiting, access control
- Works anywhere: cloud VMs, HPC login nodes, on-prem servers, containers

### Quick Setup

```bash
# 1. Generate nginx config
prism serve --generate-nginx > /etc/nginx/sites-available/prism-mcp

# 2. Enable the site
ln -s /etc/nginx/sites-available/prism-mcp /etc/nginx/sites-enabled/

# 3. Test and reload nginx
nginx -t && systemctl reload nginx

# 4. Start the PRISM server
prism serve --transport http --host 127.0.0.1 --port 8000
```

Clients connect to `http://your-server/mcp` and nginx proxies to PRISM.

### Custom Port

```bash
prism serve --generate-nginx --port 9000 > /etc/nginx/sites-available/prism-mcp
prism serve --transport http --port 9000
```

### What the nginx Config Does

- Proxies `/mcp` to the PRISM upstream
- Disables buffering for SSE/streaming support
- Sets 300s read/send timeouts (tool calls can be slow)
- Passes standard proxy headers (X-Real-IP, X-Forwarded-For)
- Provides a `/health` endpoint for monitoring
- Returns 404 for all other paths

### Adding SSL

Edit the generated config to add:

```nginx
server {
    listen 443 ssl;
    ssl_certificate     /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;
    # ... rest of config ...
}
```

### Running as a Service

Create a systemd unit to keep PRISM running:

```ini
# /etc/systemd/system/prism-mcp.service
[Unit]
Description=PRISM MCP Server
After=network.target

[Service]
Type=simple
User=prism
WorkingDirectory=/opt/prism
ExecStart=/opt/prism/.venv/bin/prism serve --transport http --host 127.0.0.1 --port 8000
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
systemctl enable --now prism-mcp
```

---

## Exposed Tools (26)

All tools from the PRISM plugin registry are exposed via MCP:

| Category | Tools |
|----------|-------|
| Materials data | `search_materials`, `query_materials_project`, `export_results_csv` |
| Literature | `literature_search`, `patent_search` |
| ML prediction | `predict_property`, `predict_properties`, `list_predictable_properties`, `list_models` |
| CALPHAD | `calculate_phase_diagram`, `calculate_equilibrium`, `analyze_phases` |
| Visualization | `plot_materials_comparison`, `plot_correlation_matrix`, `plot_property_distribution` |
| Data quality | `validate_dataset`, `review_dataset` |
| Skills | `materials_discovery`, `acquire_materials`, `select_materials`, `plan_simulations`, `visualize_dataset`, `generate_report` |
| Data import | `import_dataset` |
| System | `read_file`, `write_file`, `web_search`, `show_scratchpad` |

## Exposed Resources

| URI | Description |
|-----|-------------|
| `prism://tools` | List all available tools |
| `prism://sessions` | Saved PRISM sessions |
| `prism://datasets` | Collected materials datasets |
| `prism://datasets/{name}` | Specific dataset metadata + preview |
| `prism://models` | Trained ML models and metrics |
| `prism://skills` | Available skills with step details |
| `prism://calphad/databases` | Thermodynamic TDB databases (if pycalphad installed) |
| `prism://simulations/structures` | Atomistic structures (if pyiron installed) |
| `prism://simulations/jobs` | Simulation jobs |
| `prism://simulations/jobs/{id}` | Specific job details |

---

## Managing External MCP Servers

PRISM can also act as an MCP **client**, importing tools from other MCP servers:

```bash
# Create config template
prism mcp init

# Check server status
prism mcp status
```

Configure external servers in `~/.prism/mcp_servers.json`:

```json
{
  "servers": {
    "my-server": {
      "command": "npx",
      "args": ["-y", "@my/mcp-server"]
    }
  }
}
```

Tools from external MCP servers are available in the REPL and `prism run`.
