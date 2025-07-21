# Installation Guide

This guide will walk you through setting up the PRISM development environment.

## Prerequisites

- Python 3.9+
- `git`

## Installation

Due to a temporary issue with a sub-dependency, the installation process is currently more complex than usual. Please follow these steps carefully.

### 1. Clone the Repository

```bash
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
```

### 2. Create and Activate a Virtual Environment

It is highly recommended to use a virtual environment to avoid conflicts with other projects.

**On macOS and Linux:**

```bash
python3 -m venv .venv
source .venv/bin/activate
```

**On Windows:**

```bash
python -m venv .venv
.\.venv\Scripts\activate
```

### 3. Install Dependencies

First, install the core dependencies from the `pyproject.toml` file:

```bash
pip install -e .
```

Next, manually install the `optimade-client` and its required dependencies, *excluding* the problematic notebook dependencies:

```bash
pip install requests pandas cachecontrol filelock ipywidgets ipywidgets-extended widget-periodictable optimade
pip install --no-deps optimade-client
```

### 4. Verify the Installation

You can verify the installation by running the following command, which should display the help message for the PRISM CLI:

```bash
prism --help
```

You are now ready to use PRISM!
