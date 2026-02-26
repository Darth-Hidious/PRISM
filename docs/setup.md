# `prism setup` / `prism configure` / `prism update` -- Configuration

Three commands manage PRISM's configuration: **setup** for workflow preferences,
**configure** for API keys and secrets, **update** for version management.

## Quick Start

```bash
prism                              # First run triggers onboarding automatically
prism configure --show             # See all current settings
prism setup                        # Re-run the preferences wizard
prism update                       # Check for new versions
```

## First-Run Onboarding

When you run `prism` for the first time (no LLM key configured), an interactive
onboarding flow starts automatically:

1. **Choose LLM provider** -- Anthropic (Claude), OpenAI, OpenRouter, or skip
2. **Enter API key** -- stored in `.env` (password-masked input)
3. **Materials Project key** -- optional, enriches search results
4. Keys saved to `.env`, preferences to `~/.prism/preferences.json`

To re-run: delete `~/.prism/preferences.json` or set `onboarding_complete: false`.

## `prism configure`

Manage API keys, model defaults, and view current configuration.

```bash
# API keys
prism configure --anthropic-key sk-ant-...
prism configure --openai-key sk-...
prism configure --openrouter-key sk-or-...
prism configure --mp-api-key YOUR_KEY
prism configure --labs-key YOUR_KEY

# Default LLM model
prism configure --model claude-sonnet-4-20250514

# View everything
prism configure --show

# Reset to defaults (creates backup)
prism configure --reset
```

`--show` displays:
- All API keys (masked) with set/unset status
- Default model and feature backend
- Workflow preferences from `prism setup`
- Available resources (from capability discovery)

### Supported API Keys

| Flag | Env Variable | Service |
|------|-------------|---------|
| `--anthropic-key` | `ANTHROPIC_API_KEY` | Claude (recommended) |
| `--openai-key` | `OPENAI_API_KEY` | GPT-4 |
| `--openrouter-key` | `OPENROUTER_API_KEY` | 200+ models |
| `--mp-api-key` | `MATERIALS_PROJECT_API_KEY` | Materials Project |
| `--labs-key` | `PRISM_LABS_API_KEY` | PRISM Labs marketplace |
| `--model` | `PRISM_DEFAULT_MODEL` | Default LLM model |

## `prism setup`

Interactive wizard for workflow defaults. Shows available capabilities first,
then prompts for each setting:

| Setting | Options | Default |
|---------|---------|---------|
| Output format | csv, parquet, both | csv |
| Search providers | comma-separated list | optimade |
| Max results/source | 1-10000 | 100 |
| ML algorithm | random_forest, gradient_boosting, linear, xgboost, lightgbm | random_forest |
| Report format | markdown, pdf | markdown |
| Compute budget | local, hpc | local |
| HPC queue | (if hpc) | default |
| HPC cores | (if hpc) | 4 |
| Update checks | yes, no | yes |

Preferences are used by skills (`acquire_materials`, `predict_properties`, etc.)
and CLI commands (`prism data collect`, `prism model train`).

## `prism update`

Check for newer versions, upgrade the Python package, and download the latest
Ink frontend binary.

```bash
prism update                       # Check, confirm, upgrade + download binary
prism update -y                    # Auto-confirm upgrade
prism update --check-only          # Just check, no upgrade
```

Auto-detects your installation method:

| Method | Detection | Upgrade Command |
|--------|-----------|-----------------|
| uv | `uv tool list` | `uv tool upgrade prism-platform` |
| pipx | `pipx list --short` | `pipx upgrade prism-platform` |
| pip | `importlib.metadata` | `pip install --upgrade prism-platform` |
| curl/unknown | fallback | `curl -fsSL .../install.sh \| bash -s -- --upgrade` |

After upgrading the Python package, `prism update` also downloads the latest
Ink frontend binary for your platform to `~/.prism/bin/prism-tui`. If no
binary is available, the classic Rich UI is used as fallback.

Version checks are cached for 24 hours (`~/.prism/.update_check`).
Sources: PyPI first, GitHub releases as fallback.

## Configuration Files

| File | Purpose | Managed By |
|------|---------|-----------|
| `~/.prism/settings.json` | Global settings (agent, search, ML, etc.) | `prism setup` / manual edit |
| `.prism/settings.json` | Project-level overrides (can be checked into git) | Manual edit |
| `.env` | API keys, secrets | `prism configure` / onboarding |
| `~/.prism/preferences.json` | Legacy workflow preferences | `prism setup` / onboarding |
| `~/.prism/.update_check` | Version check cache | `prism update` (auto) |
| `~/.prism/bin/prism-tui` | Compiled Ink frontend binary | `install.sh` / `prism update` |
| `~/.prism/cache/` | Search cache, provider health | Search engine (auto) |
| `~/.prism/databases/` | TDB thermodynamic databases | `prism model calphad import` |
| `~/.prism/labs_subscriptions.json` | Lab service subscriptions | `prism labs subscribe` |

## `settings.json`

PRISM uses a two-tier settings file, similar to Claude Code:

- **Global**: `~/.prism/settings.json` -- user-level defaults
- **Project**: `.prism/settings.json` -- per-project overrides (check into git)

Merge order: **defaults < global < project < environment variables**.

### Schema

```json
{
  "agent": {
    "model": "claude-sonnet-4-20250514",
    "provider": "anthropic",
    "max_iterations": 30,
    "auto_approve": false,
    "temperature": 0.0,
    "max_tokens": 0
  },
  "search": {
    "default_providers": ["optimade"],
    "max_results_per_source": 100,
    "cache_ttl_hours": 24,
    "timeout_seconds": 30,
    "retry_attempts": 3
  },
  "output": {
    "format": "csv",
    "directory": "output",
    "report_format": "markdown"
  },
  "compute": {
    "budget": "local",
    "hpc_queue": "default",
    "hpc_cores": 4
  },
  "ml": {
    "algorithm": "random_forest",
    "feature_backend": ""
  },
  "updates": {
    "check_on_startup": true,
    "cache_ttl_hours": 24,
    "channel": "stable"
  },
  "permissions": {
    "require_approval": ["execute_python", "write_file", "submit_lab_job"],
    "deny": []
  }
}
```

### Environment Variable Overrides

Settings can be overridden with `PRISM_<SECTION>_<KEY>` environment variables:

```bash
PRISM_AGENT_MODEL=gpt-4o          # overrides agent.model
PRISM_SEARCH_TIMEOUT=60           # overrides search.timeout_seconds (planned)
PRISM_DEFAULT_MODEL=gpt-4o        # legacy, maps to agent.model
```

### Project-Level Example

Create `.prism/settings.json` in your project root to share settings with
collaborators:

```json
{
  "agent": { "model": "claude-sonnet-4-20250514" },
  "search": { "max_results_per_source": 500 },
  "output": { "format": "parquet", "directory": "data/output" }
}
```

## Related

- [`prism search`](search.md) -- Uses default providers and max results
- [`prism model`](predict.md) -- Uses default algorithm and compute budget
- [`prism labs`](labs.md) -- Uses labs API key for marketplace
- [Plugins](plugins.md) -- Extend configuration with custom settings
