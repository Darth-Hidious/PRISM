# Installation Guide

Follow these steps to get PRISM up and running on your system.

## Prerequisites

- **Python**: 3.10 or newer.
- **OS**: macOS or Linux (Windows works but is not officially tested).

## Quick Install

### Option 1: pipx (recommended)

```bash
pipx install prism-platform
```

### Option 2: uv

```bash
uv tool install prism-platform
```

### Option 3: curl one-liner

```bash
curl -fsSL https://raw.githubusercontent.com/Darth-Hidious/PRISM/main/install.sh | sh
```

This detects your OS, finds Python, installs pipx/uv if needed, and installs PRISM.

### Option 4: pip (in a virtualenv)

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install prism-platform
```

## Optional Extras

Install additional capabilities as needed:

```bash
# ML pipeline (scikit-learn, xgboost, lightgbm, optuna, matminer)
pip install "prism-platform[ml]"

# Atomistic simulation (pyiron)
pip install "prism-platform[simulation]"

# CALPHAD thermodynamics (pycalphad)
pip install "prism-platform[calphad]"

# Extra data sources (HuggingFace datasets for OMAT24)
pip install "prism-platform[data]"

# PDF/HTML reports (markdown, weasyprint)
pip install "prism-platform[reports]"

# Everything
pip install "prism-platform[all]"
```

## Development Install

```bash
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
pip install -e ".[dev]"
```

## Configuration

Before using the agent commands, configure at least one LLM provider.
Copy the example environment file and fill in your keys:

```bash
cp .env.example .env
# Edit .env with your API keys
```

Or configure interactively:

```bash
prism advanced configure
```

## Verification

```bash
# Show version
prism --version

# List available commands
prism --help

# Check for updates
prism update
```

## Updating

```bash
# Check for updates
prism update

# Upgrade via pipx
pipx upgrade prism-platform

# Or via pip
pip install --upgrade prism-platform
```
