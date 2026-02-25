# `prism data` — Data Pipeline Commands

Manage materials datasets: collect from federated OPTIMADE providers, import local files, and inspect stored datasets.

## Subcommands

| Command | Description |
|---------|-------------|
| `prism data collect` | Search federated OPTIMADE providers and save results as a dataset |
| `prism data import` | Import a local CSV, JSON, or Parquet file as a PRISM dataset |
| `prism data status` | List all stored datasets with row/column counts |

## `prism data collect`

Runs a federated search across OPTIMADE providers using the SearchEngine (same engine as `prism search`), then converts results to a pandas DataFrame and saves to the DataStore.

```bash
# Collect silicon-oxygen materials
prism data collect --elements Si,O --limit 50

# Collect by formula
prism data collect --formula SiO2 --name silica_dataset

# Target specific providers
prism data collect --elements Fe,Ni --providers mp,aflow --limit 200
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--elements` | — | Comma-separated elements to search (e.g. `Si,O`) |
| `--formula` | — | Chemical formula (e.g. `SiO2`) |
| `--providers` | all | Comma-separated provider IDs |
| `--limit` | 100 | Maximum results |
| `--name` | auto | Dataset name (auto-generated from query if omitted) |

At least one of `--elements` or `--formula` is required.

### Output

Saves a Parquet dataset to `~/.prism/cache/datasets/` containing:

- `id`, `formula`, `elements`, `n_elements`, `sources`
- Property columns (when available): `space_group`, `crystal_system`, `band_gap`, `formation_energy`, `energy_above_hull`, `bulk_modulus`, `debye_temperature`
- Source attribution columns: `{property}_source` for each property

## `prism data import`

Import an existing file into the PRISM DataStore.

```bash
prism data import my_materials.csv
prism data import results.json --name experiment_1
prism data import output.parquet --format parquet
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--name` | filename stem | Dataset name |
| `--format` | auto-detect | File format override (`csv`, `json`, `parquet`) |

## `prism data status`

List all stored datasets.

```bash
prism data status
```

```
┌─────────────────────────────────────┐
│        Available Datasets           │
├──────────────┬──────┬───────┬───────┤
│ Name         │ Rows │ Cols  │ Saved │
├──────────────┼──────┼───────┼───────┤
│ collect_Si_O │  142 │    12 │ 2026… │
│ experiment_1 │   50 │     8 │ 2026… │
└──────────────┴──────┴───────┴───────┘
```

## Code Execution (`execute_python` tool)

The agent can write and execute Python code for data manipulation via the `execute_python` tool. This is available in both `prism run` (autonomous mode) and the REPL.

### How it works

- Code runs in a **subprocess** (isolated from the agent process)
- The user's full Python environment is available: pandas, numpy, matplotlib, pymatgen, ASE, scikit-learn, pycalphad, etc.
- **Requires user approval** before execution (unless `--dangerously-accept-all` is set)
- Default timeout: 60 seconds
- Use `print()` to return output to the agent
- Use `plt.savefig("filename.png")` to save plots

### Example agent interaction

```
User: Find iron oxides and plot their band gaps

Agent: <plan>
1. Use search_materials to find iron oxides
2. Use execute_python to analyze and plot band gaps
</plan>

[Tool: search_materials] → 47 results
[Tool: execute_python] ⚠ Requires approval

  Code:
    import pandas as pd
    import matplotlib.pyplot as plt
    data = pd.read_parquet("~/.prism/cache/datasets/collect_Fe_O.parquet")
    data['band_gap'].dropna().hist(bins=20)
    plt.xlabel('Band Gap (eV)')
    plt.savefig('iron_oxide_bandgaps.png')
    print(f"Plotted {len(data)} materials")

  [y/N]: y

  → Plotted 47 materials
  → Saved: iron_oxide_bandgaps.png
```

### Security model

| Layer | Protection |
|-------|-----------|
| Subprocess isolation | Code runs in a child process, not in the agent |
| Approval gate | User must approve each execution (tool has `requires_approval=True`) |
| Timeout | Default 60s, configurable per call |
| No network restriction | Full access to installed packages and filesystem |

## Architecture

```
prism data collect
  └─ SearchEngine (federated async search)
       └─ ProviderRegistry → OPTIMADE providers
  └─ _materials_to_dataframe() → pandas DataFrame
  └─ DataStore.save() → ~/.prism/cache/datasets/

prism run / REPL
  └─ AgentCore (TAOR loop)
       └─ execute_python tool
            └─ subprocess.run([python, "-c", code])
```

## Related

- [`prism search`](search.md) — Interactive federated search
- [`prism run`](run.md) — Autonomous agent mode (uses execute_python)
- [`prism serve`](serve.md) — MCP server mode
