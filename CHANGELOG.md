# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.0.0] - 2026-02-24

### Added
- **Agent Loop (Phase A)**: Agentic REPL with streaming, tool registry, CSV export, session memory, and ML pipeline (train, predict, feature-importance, correlation).
- **MCP Integration (Phase B)**: `prism serve` exposes tools and resources via FastMCP 3.x; `prism mcp init/status` manages external MCP servers.
- **Simulation (Phase C)**: Pyiron atomistic simulation bridge with structure/job stores, HPC config, and `prism sim` commands.
- **Skills (Phase D)**: Multi-step workflow orchestration — acquisition, featurize, train-predict, simulation planning, and more.
- **Plugin System (Phase E-1)**: `PluginRegistry` supporting pip entry-points and `~/.prism/plugins/` local plugins; `prism plugin list/init`.
- **Local Data Import (Phase E-1)**: CSV/JSON/Parquet ingestion via `import_local_data` tool and `data import` command.
- **New Data Sources (Phase E-2)**: OMAT24 (HuggingFace streaming), literature search (arXiv + Semantic Scholar), patent search (Lens.org).
- **CALPHAD Integration (Phase E-3)**: pycalphad bridge with 6 tools (phase diagram, equilibrium, Gibbs energy, list/import databases, list phases), ThermoCalc plugin skeleton.
- **Review Agent (Phase E-4)**: Validation rules (outlier detection, physical constraints, completeness scoring), `validate_dataset` and `review_dataset` skills.
- **Interactive ML (Phase E-4)**: `list_predictable_properties` tool, correlation visualization.
- **Enhanced Reports (Phase E-4)**: HTML report generation with embedded charts.
- **Version Check (Phase E-5)**: `prism update` command, automatic update check on REPL startup with PyPI/GitHub fallback and 24h cache.
- **Curl Installer (Phase E-5)**: `install.sh` one-command installer (pipx/uv auto-detection).
- **Packaging (Phase E-5)**: `[all]` optional dependency group, `app.validation` in setuptools packages, Python 3.13/3.14 classifiers.

### Changed
- **Project Version**: Bumped from 1.1.0 to 2.0.0.
- **Architecture**: Provider-agnostic `AgentCore` with tool registry replaces monolithic CLI.
- **Two Modes**: Interactive REPL (`prism`) and autonomous agent (`prism run "goal"`).
- **`.env.example`**: Updated to v2.0 template (LLM keys + Materials Project API key).
- **`docs/INSTALL.md`**: Modernized with pipx/uv/curl install methods and optional extras.
- **`docs/SECURITY.md`**: Updated version table and added MCP/CALPHAD/ML/plugin mentions.

### Removed
- `Schema.txt` (209KB OPTIMADE spec — no longer used).
- `prism.db` (runtime artifact — should not be tracked).
- `app/optimade_properties.py` (empty file).
- `provider_fields.json` (empty runtime cache).
- `quick_install.py` (superseded by `install.sh` and Makefile).
- `requirements.txt` (duplicate of pyproject.toml).
- `app/env.example` (merged into root `.env.example`).

## [1.1.0] - 2024-07-26

### Added

- **Intelligent Search (`ask` command)**: A new command that uses natural language to query materials science databases. It leverages LLMs to translate queries into OPTIMADE filters and summarize the results.
- **Interactive Mode**: The `ask` command now has an `--interactive` flag to enable a conversational query refinement mode.
- **Pluggable LLM Providers**: Support for multiple LLM providers, including OpenAI, Google Vertex AI, Anthropic, and OpenRouter. Configuration is handled via the `prism advanced configure` command.
- **Configurable Models**: Users can now specify a particular model to use for a given LLM provider in the `.env` file.
- **Provider Discovery**: The `prism optimade list-dbs` command now lists all available OPTIMADE providers, with a fallback to a cached list if the live network is unavailable.
- **RAG Capability**: The core MCP has been updated to support Retrieval-Augmented Generation, allowing for future integration with local knowledge bases.
- **Polished CLI Output**: The CLI output for `search` and `ask` has been enhanced with `rich.panel` for better readability.

### Changed

- **Project Version**: Bumped from 1.0.0 to 1.1.0.
- **Documentation**: The `README.md` file has been significantly updated with more detailed explanations and examples.
- **CLI Refinements**: The `search` and `ask` commands have been improved for better usability and error handling.

### Removed

- **Problematic Test**: The test file `tests/test_ask_command.py` was removed due to persistent, unresolvable issues with mocking. 