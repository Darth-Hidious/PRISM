# PRISM Platform - Installation Guide

This guide provides simple instructions to get the PRISM command-line interface (CLI) up and running.

## Requirements

- **Python**: 3.9 or higher
- **pip**: The Python package installer

## Installation

The recommended way to install PRISM is using `pip` in a virtual environment.

### 1. Clone the Repository

First, clone the project from GitHub:

```bash
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
```

### 2. Create and Activate a Virtual Environment (Recommended)

Using a virtual environment prevents conflicts with other Python projects on your system.

**On macOS / Linux:**
```bash
python3 -m venv .venv
source .venv/bin/activate
```

**On Windows:**
```bash
python -m venv .venv
.venv\Scripts\activate
```

### 3. Install PRISM

Now, install the PRISM package in "editable" mode. This allows you to use the `prism` command directly and also make changes to the code if you wish.

```bash
python -m pip install -e .
```
Alternatively, you can run the `quick_install.py` script, which does the same thing:
```bash
python quick_install.py
```

## Verify Installation

After the installation is complete, you can verify that the CLI is working by running:

```bash
prism --help
```

This should display the main help message for the PRISM CLI, showing all available commands and options.

## Usage

You can now use the `prism` command to interact with the platform. For example, to search for materials:

```bash
# Search for structures containing Silicon and Oxygen
prism search --elements Si O

# Search for structures with a specific formula
prism search --formula "SiO2"
```

## Troubleshooting

### `command not found: prism`

If your shell cannot find the `prism` command after installation, it might be because the Python scripts directory is not in your system's `PATH`.

- **Solution 1 (Activate Virtual Environment):** Ensure your virtual environment is activated. The `prism` command will be available automatically when the venv is active.
- **Solution 2 (Use `python -m`):** If you are not using a virtual environment, you can run the CLI as a Python module:
  ```bash
  python -m app.cli --help
  ```

### Dependency Installation Issues

If you encounter errors during the installation of dependencies (e.g., related to `numpy` or `pandas`), you may be missing system-level development libraries.

- **On Debian/Ubuntu:**
  ```bash
  sudo apt-get update
  sudo apt-get install python3-dev
  ```
- **On Fedora/CentOS:**
  ```bash
  sudo dnf install python3-devel
  ```

After installing the required system packages, try the installation command again.
