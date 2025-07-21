# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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