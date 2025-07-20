# MARC27's PRISM Platform - Installation & Setup Guide

## 🚀 Quick Installation

### One-Line Installation (Recommended)
```bash
# Install from GitHub
pip install git+https://github.com/Darth-Hidious/PRISM.git

# Or clone and install
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
python quick_install.py
```

## 🖥️ Platform-Specific Installation

### 🐧 Linux/macOS
```bash
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
chmod +x install.sh
./install.sh
```

### 🪟 Windows

#### Command Prompt
```batch
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
install_windows.bat
```

#### PowerShell (Recommended)
```powershell
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
.\install_windows.ps1
```

## 📦 Package Manager Installation

### With uv (Fastest)
```bash
# Install uv first
curl -LsSf https://astral.sh/uv/install.sh | sh  # Linux/macOS
# or
powershell -c "irm https://astral.sh/uv/install.ps1 | iex"  # Windows

# Install PRISM
uv pip install -e .
```

### With pip
```bash
pip install -e .
```

### With pipx (Isolated)
```bash
pipx install git+https://github.com/Darth-Hidious/PRISM.git
```

## 🔧 Development Installation

```bash
# Clone repository
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM

# Install with development dependencies
pip install -e ".[dev,export,monitoring]"

# Run tests
pytest
```

## ✅ Verify Installation

```bash
# Check installation
prism --version
prism --help

# Launch MARC27's PRISM
prism

# Test database connection
prism list-databases

# Interactive tutorial
prism getting-started
```

## 🛠 Troubleshooting

### Common Issues

#### Python not found
```bash
# Make sure Python 3.8+ is installed
python --version  # or python3 --version

# On Windows, you might need:
py --version
```

#### Command not found after installation
```bash
# Try module syntax
python -m app.cli --help

# Or check PATH (restart terminal)
echo $PATH  # Linux/macOS
echo $env:PATH  # Windows PowerShell
```

#### Permission errors
```bash
# Use user installation
pip install --user -e .

# Or use virtual environment
python -m venv venv
source venv/bin/activate  # Linux/macOS
venv\Scripts\activate     # Windows
pip install -e .
```

## 📋 Requirements

- **Python**: 3.8 or higher
- **OS**: Windows 10+, macOS 10.15+, Linux (Ubuntu 18.04+)
- **Memory**: 4GB RAM minimum
- **Storage**: 2GB free space

## 🆘 Support

- **Issues**: https://github.com/Darth-Hidious/PRISM/issues
- **Email**: team@marc27.com
- **Documentation**: Built-in via `prism getting-started`
