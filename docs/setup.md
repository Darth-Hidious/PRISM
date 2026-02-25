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

Check for newer versions and show the correct upgrade command.

```bash
prism update                       # Check and show upgrade instructions
prism update --check-only          # Just check, no instructions
```

Auto-detects your installation method:

| Method | Detection | Upgrade Command |
|--------|-----------|-----------------|
| uv | `uv tool list` | `uv tool upgrade prism-platform` |
| pipx | `pipx list --short` | `pipx upgrade prism-platform` |
| pip | `importlib.metadata` | `pip install --upgrade prism-platform` |
| curl/unknown | fallback | `curl -fsSL .../install.sh \| bash -s -- --upgrade` |

Version checks are cached for 24 hours (`~/.prism/.update_check`).
Sources: PyPI first, GitHub releases as fallback.

## Configuration Files

| File | Purpose | Managed By |
|------|---------|-----------|
| `.env` | API keys, secrets, model default | `prism configure` / onboarding |
| `~/.prism/preferences.json` | Workflow preferences | `prism setup` / onboarding |
| `~/.prism/.update_check` | Version check cache | `prism update` (auto) |
| `~/.prism/cache/` | Search cache, provider health | Search engine (auto) |
| `~/.prism/databases/` | TDB thermodynamic databases | `prism model calphad import` |
| `~/.prism/labs_subscriptions.json` | Lab service subscriptions | `prism labs subscribe` |

## Related

- [`prism search`](search.md) -- Uses default providers and max results
- [`prism model`](predict.md) -- Uses default algorithm and compute budget
- [`prism labs`](labs.md) -- Uses labs API key for marketplace
- [Plugins](plugins.md) -- Extend configuration with custom settings
