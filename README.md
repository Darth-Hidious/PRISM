
# PRISM: Platform for Research in Intelligent Synthesis of Materials

<p align="center">
      ██████╗ ██████╗ ██╗███████╗███╗   ███╗
      ██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
      ██████╔╝██████╔╝██║███████╗██╔████╔██║
      ██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
      ██║     ██║  ██║██║███████║██║ ╚═╝ ██║
      ╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝
</p>

<p align="center">
    <em>A next-generation command-line interface for materials science research, powered by the OPTIMADE API network and Large Language Models.</em>
</p>

---

PRISM is a powerful, intuitive tool designed to streamline the process of materials discovery. It provides a single, unified interface to query dozens of major materials science databases and leverages the latest advances in AI to make your search process more natural and efficient.

## Core Concepts

- **OPTIMADE**: PRISM is built on the [Open Databases Integration for Materials Design (OPTIMADE)](https://www.optimade.org/) API specification. This allows PRISM to communicate with a wide range of materials databases using a single, standardized query language.
- **MCP (Model Context Protocol)**: This is the internal system that allows PRISM to translate between human language and the structured query language of OPTIMADE. When you use the `ask` command, the MCP takes your question, uses an LLM to extract the key scientific concepts, and then constructs a precise OPTIMADE filter to find the data you need.
- **BYOK (Bring Your Own Key)**: PRISM is designed to be used with your own API keys for various LLM providers. This ensures that you have full control over your usage and costs.

## Features

- **Unified Search**: Query dozens of materials databases (including Materials Project, OQMD, COD, and more) with a single `search` command.
- **Intelligent Search (`ask`)**: Use natural language to ask questions about materials (e.g., `"Find me all materials containing cobalt and lithium"`). PRISM uses an LLM to translate your query into a precise OPTIMADE filter, searches the databases, and provides a summarized, easy-to-understand answer.
- **Interactive Mode (`ask --interactive`)**: Refine your queries through a conversation with the built-in LLM research assistant. If your query is ambiguous, PRISM will ask you clarifying questions to help you narrow down your search.
- **Local Database**: Save your search results to a local SQLite database for persistence, analysis, and future reference.
- **Pluggable LLM Providers**: Bring your own API key for a variety of LLM providers, including OpenAI, Google Vertex AI, Anthropic, and OpenRouter.
- **Provider Discovery**: List all available OPTIMADE databases with the `optimade list-dbs` command.

## Command Reference

A detailed look at the available commands and their options.

---
### `prism search`
Performs a structured search of the OPTIMADE network. This command is best for when you know the specific properties of the materials you are looking for.

**Usage:**
```bash
prism search [OPTIONS]
```

**Options:**
- `--elements TEXT`: Comma-separated list of elements the material must contain (e.g., `"Si,O"`).
- `--formula TEXT`: An exact chemical formula (e.g., `"SiO2"`).
- `--nelements INTEGER`: The exact number of elements in the material.
- `--providers TEXT`: A comma-separated list of OPTIMADE provider IDs to search. By default, it searches all providers.

**Examples:**
```bash
# Find all materials containing Iron, Nickel, and Chromium
prism search --elements "Fe,Ni,Cr"

# Find materials with the exact formula for silicon carbide
prism search --formula "SiC"

# Find all binary compounds containing Cobalt from the OQMD and Materials Project databases
prism search --elements "Co" --nelements 2 --providers "oqmd,mp"
```
---
### `prism ask`
Asks a question about materials science using natural language. This command is best for exploratory searches or when you are not sure of the exact chemical properties.

**Usage:**
```bash
prism ask "[QUERY]" [OPTIONS]
```

**Options:**
- `--providers TEXT`: A comma-separated list of provider IDs to search.
- `--interactive`: Enables a conversational mode where PRISM will ask clarifying questions to refine your search.

**Examples:**
```bash
# General query
prism ask "What are the known binary compounds of silicon and carbon?"

# A more complex query targeting specific databases
prism ask "high entropy alloys containing molybdenum" --providers "oqmd"

# Start an interactive session to find a semiconductor
prism ask "I need to find a good semiconductor for a high-power application" --interactive
```
---
### `prism optimade list-dbs`
Lists all available OPTIMADE provider databases that PRISM can search. This is useful for finding the provider IDs to use with the `--providers` option in the `search` and `ask` commands.
---
### `prism advanced`
Advanced commands for database management and application configuration.

- `prism advanced init`: Initializes the local SQLite database. This is required if you want to save search results.
- `prism advanced configure`: Guides you through setting up your database connection and LLM provider. This is required to use the `ask` command.
---
### `prism docs`
Commands for generating the project documentation.

- `prism docs save-readme`: Saves this README file to the project root.
- `prism docs save-install`: Saves the `INSTALL.md` file to the project root.

## Quick Start

1.  **Installation**: See the `INSTALL.md` file for detailed instructions.
2.  **Configuration**: To use the `ask` command, you must first configure your preferred LLM provider. PRISM will guide you through this process.
    ```bash
    prism advanced configure
    ```
    You will be prompted to choose an LLM provider (like OpenAI, OpenRouter, etc.) and enter your API key. For the easiest setup, we recommend the **OpenRouter** option.

3.  **Initialize the Database (Optional)**: If you want to save your search results, you first need to initialize the local database.
    ```bash
    prism advanced init
    ```
4.  **Run a Search**:
    ```bash
    prism search --elements "Ti,O" --nelements 2
    ```
5.  **Ask a Question**:
    ```bash
    prism ask "Find me materials containing titanium and oxygen"
    ```
