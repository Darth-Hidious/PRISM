# Installation Guide

Follow these steps to get PRISM up and running on your system.

## Prerequisites

- **Python**: 3.11 or newer.
- **OS**: macOS or Linux (Windows works but is not officially tested).

## Quick Install

### Option 1: curl one-liner (recommended)

```bash
curl -fsSL https://prism.marc27.com/install.sh | bash
```

This detects your OS, finds Python, installs pipx/uv if needed, installs PRISM,
and downloads the compiled Ink frontend binary for your platform.

### Option 2: pipx

```bash
pipx install prism-platform
prism update    # downloads the Ink frontend binary
```

### Option 3: uv

```bash
uv tool install prism-platform
prism update    # downloads the Ink frontend binary
```

### Option 4: pip (in a virtualenv)

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install prism-platform
prism update    # downloads the Ink frontend binary
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

# Launch (Ink frontend)
prism

# Launch with classic Rich UI
prism --classic
```

## Updating

```bash
# Check for updates, upgrade, and download latest Ink binary
prism update

# Or with auto-confirm
prism update -y
```

`prism update` auto-detects how PRISM was installed (uv, pipx, pip) and runs
the correct upgrade command. It also downloads the latest Ink frontend binary
for your platform to `~/.prism/bin/prism-tui`.

## Ink Frontend Binary

The installer and `prism update` automatically download a pre-compiled Ink
frontend binary for your platform:

| Platform | Binary |
|----------|--------|
| macOS Apple Silicon | `prism-tui-darwin-arm64` |
| macOS Intel | `prism-tui-darwin-x64` |
| Linux x86_64 | `prism-tui-linux-x64` |
| Linux ARM64 | `prism-tui-linux-arm64` |

The binary is stored at `~/.prism/bin/prism-tui`. If no binary is available
for your platform, PRISM falls back to the classic Rich UI automatically.

To force the classic UI: `prism --classic`
