# PRISM Platform Installation Guide

## 🚀 Quick Installation Methods

### Method 1: One-Line Installation (Recommended)
```bash
# Install with uv (fastest)
curl -sSL https://raw.githubusercontent.com/Darth-Hidious/PRISM/main/quick_install.py | python

# Or install with pip
pip install git+https://github.com/Darth-Hidious/PRISM.git
```

### Method 2: Local Development Installation
```bash
# Clone the repository
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM

# Quick install script
python quick_install.py

# Or manual installation
uv pip install -e .
# Or with pip
pip install -e .
```

### Method 3: Using the Enhanced Install Script
```bash
# Run the interactive installer
chmod +x install.sh
./install.sh
```

## 📦 Installation Options

### Production Installation
```bash
# Install stable version
uv pip install prism-platform

# Or with pip
pip install prism-platform
```

### Development Installation
```bash
# Install with dev dependencies
uv pip install -e ".[dev,export,monitoring]"

# Or with pip
pip install -e ".[dev,export,monitoring]"
```

### Isolated Installation with pipx
```bash
# Install in isolated environment
pipx install prism-platform

# Or from source
pipx install git+https://github.com/Darth-Hidious/PRISM.git
```

## 🎨 Company Logo Integration

To add your company logo to PRISM:

### Option 1: Provide Image File
1. Upload your logo image (PNG, JPG, SVG)
2. I'll convert it to ASCII art automatically
3. Best for detailed logos

### Option 2: Company Name
1. Provide your company name
2. I'll create stylized ASCII text
3. Simple and clean approach

### Option 3: Custom ASCII Art
1. If you have existing ASCII art
2. Paste it into `app/config/branding.py`
3. Immediate integration

### Option 4: Logo Description
1. Describe your logo design
2. I'll create custom ASCII art
3. Tailored to your specifications

## 🔧 Making It Work Like "uv install"

To make PRISM installable like `uv install prism-platform`:

### 1. Publish to PyPI
```bash
# Build the package
python -m build

# Upload to PyPI (requires account)
python -m twine upload dist/*
```

### 2. Create a Custom uv Source
```bash
# Add to your pyproject.toml
[tool.uv]
index-url = "https://pypi.org/simple/"

# Then users can install with:
uv add prism-platform
```

### 3. Docker Installation
```bash
# Pull and run PRISM container
docker run -it prism-platform:latest prism --help
```

## ✅ Verify Installation

After installation, test with:
```bash
prism --version
prism --help
prism list-databases
prism getting-started
```

## 🛠 Troubleshooting

### Common Issues:
1. **Command not found**: Restart terminal or run `source ~/.bashrc`
2. **Permission errors**: Use `pip install --user` or virtual environment
3. **uv not found**: Install with `curl -LsSf https://astral.sh/uv/install.sh | sh`

### Support:
- GitHub Issues: https://github.com/Darth-Hidious/PRISM/issues
- Documentation: https://github.com/Darth-Hidious/PRISM/wiki
