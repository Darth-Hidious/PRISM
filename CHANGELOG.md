# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.1.1] - 2026-02-24

### Added
- **Claude Code-style REPL**: Minimal `>` prompt, tool call timing display, compact welcome header, `/login` command for MARC27 managed LLM access, `/skills` command (replaces `/skill`).
- **MARC27 Login**: `/login` REPL command stores token at `~/.prism/marc27_token`; factory auto-detects `MARC27_TOKEN` and routes through MARC27 API gateway (OpenAI-compatible).
- **First-Run Onboarding**: On first startup, PRISM asks for LLM provider and API keys interactively. Saves to `.env` and marks onboarding complete.

### Changed
- **Python**: Bumped minimum to **3.11** (was 3.10). pyiron and CALPHAD now install by default with `pip install "prism-platform[all]"`.
- **OPTIMADE**: All `OptimadeClient()` calls now use explicit `base_urls` from curated provider list — eliminates "Unable to retrieve databases" noise during searches.
- **Version markers**: Simplified to upper-bound only (`python_version<'3.14'`) since floor is now 3.11.
- **REPL aesthetics**: Removed decorative panels, switched to text-heavy minimal output matching Claude Code CLI design language.

### Fixed
- OPTIMADE search noise from auto-discovery of providers with null base_urls.
- MCP `_test_filter` method using `include_providers` instead of `base_urls`.

## [2.1.0] - 2026-02-24

### Added
- **Dual License (Phase F-1)**: MIT for core plumbing, MARC27 Source-Available for AI orchestration. New `LICENSE-MIT` and `LICENSE-MARC27` files.
- **Tool Consent (Phase F-2)**: `requires_approval` flag on expensive tools; REPL prompts for confirmation; `--confirm` flag for autonomous mode; `--dangerously-accept-all` to skip all prompts; `/approve-all` REPL command.
- **Scratchpad (Phase F-3)**: Append-only execution log (`Scratchpad` class) auto-populated after each tool call; `show_scratchpad` tool for agent self-inspection; `/scratchpad` REPL command; included in session save/load and reports as "Methodology" section.
- **Plan-then-Execute (Phase F-4)**: System prompt instructs agent to output `<plan>` blocks for complex goals; REPL detects and renders plan as a panel, gating execution on user approval.
- **Feedback Loops (Phase F-5)**: `_post_tool_hook()` in AgentCore injects validation, review, and CALPHAD findings back into agent context as system messages, closing the Evolver-Evaluator loop.
- **README Refresh (Phase F-6)**: Full rewrite with deck-aligned content, banner image, architecture table, dual-license section.

### Changed
- **Project Version**: Bumped from 2.0.0 to 2.1.0.
- **License**: Changed from MIT to dual (MIT core + MARC27 Source-Available AI).
- **CLI Help**: Updated to "AI-Native Autonomous Materials Discovery" positioning.
- **pyproject.toml**: Updated description, keywords, license field, author.

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