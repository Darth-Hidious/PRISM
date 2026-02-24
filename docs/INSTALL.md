# Installation Guide

Follow these steps to get PRISM up and running on your system.

## Prerequisites

- **Python**: 3.11 or newer.
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
curl -fsSL https://prism.marc27.com/install.sh | sh
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
# Everything — ML, CALPHAD, pyiron, data sources, reports
pip install "prism-platform[all]"

# Individual extras:
pip install "prism-platform[ml]"          # ML (scikit-learn, xgboost, matminer)
pip install "prism-platform[simulation]"  # Atomistic simulation (pyiron)
pip install "prism-platform[calphad]"     # Phase diagrams (pycalphad)
pip install "prism-platform[data]"        # OMAT24, HuggingFace datasets
pip install "prism-platform[reports]"     # PDF/HTML reports
```

> **Note:** pyiron and pycalphad require Python <3.14. On Python 3.14+,
> those packages are silently skipped and the rest installs normally.

## Development Install

```bash
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
pip install -e ".[dev]"
```

## Configuration

On first launch, PRISM will walk you through API key setup:

```bash
prism   # starts onboarding wizard on first run
```

Or configure manually:

```bash
cp .env.example .env    # edit with your API keys
prism setup             # interactive preferences wizard
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
