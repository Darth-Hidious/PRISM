# PRISM CLI Command Reference

## Entry Points

| Command | File | Description |
|---------|------|-------------|
| `prism` | `app/cli/main.py` | Main CLI group — launches REPL if no subcommand |
| `prism run <goal>` | `app/commands/run.py` | Autonomous agent (TAOR loop) |
| `prism search` | `app/commands/search.py` | Federated materials search (no LLM) |
| `prism serve` | `app/commands/serve.py` | MCP server mode |
| `prism data` | `app/commands/data.py` | Data collect/import/status |
| `prism predict <formula>` | `app/commands/predict.py` | ML property prediction |
| `prism model` | `app/commands/model.py` | Model train/status |
| `prism optimade` | `app/commands/optimade.py` | Direct OPTIMADE queries |
| `prism sim` | `app/commands/sim.py` | DFT/MD simulation (pyiron) |
| `prism calphad` | `app/commands/calphad.py` | CALPHAD thermodynamics |
| `prism plugin` | `app/commands/plugin.py` | Plugin list/init |
| `prism configure` | `app/commands/configure.py` | Settings wizard |
| `prism setup` | `app/commands/setup.py` | First-run onboarding |
| `prism update` | `app/commands/update.py` | Version check + upgrade |
| `prism mcp` | `app/commands/mcp.py` | MCP server management |
| `prism advanced` | `app/commands/advanced.py` | Dev/debug commands |
| `prism docs` | `app/commands/docs.py` | Documentation generation |
| `prism ask` | _(deprecated alias)_ | Redirects to `prism run` |

## Flags by Command

### `prism` (global)
| Flag | Type | Description |
|------|------|-------------|
| `--version` | flag | Show version |
| `--verbose` / `-v` | flag | Enable verbose output |
| `--quiet` / `-q` | flag | Suppress non-essential output |
| `--mp-api-key` | string | Set Materials Project API key |
| `--resume` | string | Resume a saved session by SESSION_ID |
| `--no-mcp` | flag | Disable MCP server discovery |
| `--dangerously-accept-all` | flag | Auto-approve all tool calls |
| `--help` | flag | Show help |

### `prism run <goal>`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `<goal>` | argument | required | Research goal to execute |
| `--agent` | string | None | Use a named agent config from the registry |
| `--provider` | string | None | LLM provider (anthropic/openai/openrouter) |
| `--model` | string | None | Model name override |
| `--confirm` | flag | — | Require confirmation for expensive tools |
| `--dangerously-accept-all` | flag | — | Auto-approve all tool calls |

> **Note:** `--dangerously-accept-all` exists on both global and `run` command. Consider removing from `run` in future.

### `prism search`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--elements` | string | None | Comma-separated elements (e.g., Fe,O) |
| `--formula` | string | None | Chemical formula |
| `--nelements` | int | None | Number of elements |
| `--band-gap-min` | float | None | Minimum band gap (eV) |
| `--band-gap-max` | float | None | Maximum band gap (eV) |
| `--space-group` | string | None | Space group symbol |
| `--crystal-system` | choice | None | cubic\|hexagonal\|tetragonal\|orthorhombic\|monoclinic\|triclinic\|trigonal |
| `--providers` | string | None | Comma-separated provider IDs |
| `--limit` | int | 100 | Max results (1-10000) |
| `--refresh` | flag | — | Force re-discovery of OPTIMADE providers |

### `prism serve`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--transport` | choice | stdio | MCP transport (stdio\|http) |
| `--port` | int | 8000 | HTTP port (http transport only) |
| `--install` | flag | — | Print Claude Desktop config JSON |

### `prism data`

#### `prism data collect`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--elements` | string | None | Elements to search (e.g., Si,O) |
| `--formula` | string | None | Chemical formula |
| `--providers` | string | None | Comma-separated provider IDs |
| `--max-results` | int | 100 | Max results per provider |
| `--name` | string | None | Dataset name (auto-generated if omitted) |

#### `prism data import <file_path>`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `<file_path>` | argument | required | Path to file to import |
| `--name` | string | None | Dataset name (defaults to filename) |
| `--format` | string | None | File format override (csv, json, parquet) |

#### `prism data status`
No flags.

### `prism predict <formula>`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `<formula>` | argument | required | Chemical formula |
| `--property` | string | band_gap | Property to predict |
| `--algorithm` | string | random_forest | ML algorithm |
| `--all-properties` | flag | — | Predict all available properties |

### `prism model`

#### `prism model train`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--property` | string | None | Property to train on |
| `--algorithm` | string | random_forest | Algorithm (rf/xgboost/lightgbm/gbr/linear) |
| `--dataset` | string | None | Dataset name |

#### `prism model status`
No flags.

### `prism optimade`

#### `prism optimade list-dbs`
No flags.

### `prism sim`

#### `prism sim status`
No flags.

#### `prism sim jobs`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--status` | string | None | Filter by job status |

#### `prism sim init`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--name` | string | prism_default | Project name |

### `prism calphad`

#### `prism calphad status`
No flags.

#### `prism calphad databases`
No flags.

#### `prism calphad import <tdb_path>`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `<tdb_path>` | argument | required | Path to TDB file |
| `--name` | string | None | Database name (defaults to filename) |

### `prism plugin`

#### `prism plugin list`
No flags.

#### `prism plugin init <name>`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `<name>` | argument | required | Plugin name |

### `prism configure`
| Flag | Type | Description |
|------|------|-------------|
| `--mp-api-key` | string | Set Materials Project API key |
| `--list-config` | flag | List current configuration |
| `--reset` | flag | Reset to defaults |

### `prism setup`
Interactive wizard, no flags.

### `prism update`
No flags.

### `prism mcp`

#### `prism mcp init`
No flags.

#### `prism mcp status`
No flags.

### `prism advanced`

#### `prism advanced init`
No flags.

#### `prism advanced configure`
Interactive wizard, no flags.

### `prism docs`

#### `prism docs save-readme`
No flags.

#### `prism docs save-install`
No flags.

## Agent Configs (from catalog)

Named agent configs available via `prism run --agent NAME`:

| ID | Name | Runtime | Status |
|----|------|---------|--------|
| `phase_stability_agent` | Phase Stability Agent | local | coming_soon |
| _(more from app/plugins/catalog.json)_ | | | |

## Plugin Types (catalog.json)

| Type | Description | Registry |
|------|-------------|----------|
| `provider` | Search database | ProviderRegistry |
| `tool` | Atomic action | ToolRegistry |
| `skill` | Multi-step workflow | SkillRegistry |
| `agent` | Agent configuration | AgentRegistry |
| `algorithm` | ML model | AlgorithmRegistry |
| `collector` | Data source | CollectorRegistry |
| `bundle` | Multiple of above | Multiple |

## Known Issues

1. `--dangerously-accept-all` is duplicated on both the global `prism` group and `prism run` — consider removing from `run`
2. `search` requires at least one filter criterion at runtime but Click doesn't enforce this
3. `--formation-energy-min/max` flags mentioned in the plan are NOT implemented in `search` (only `--band-gap-min/max`)
