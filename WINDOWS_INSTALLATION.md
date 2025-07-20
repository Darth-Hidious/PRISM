# Windows Installation Guide for MARC27's PRISM Platform

## 🪟 **Windows Compatibility**

MARC27's PRISM Platform is **fully compatible** with Windows 10 and Windows 11. The platform has been designed with cross-platform compatibility in mind.

### ✅ **What Works on Windows**
- 🎨 **ASCII Art Logo**: Displays perfectly in Windows Terminal, PowerShell, and Command Prompt
- 🖥️ **Rich Interface**: Full color and formatting support
- 📦 **Package Management**: Compatible with pip, uv, and pipx
- 🔍 **Database Search**: All NOMAD, JARVIS, OQMD, COD connectors work
- 📊 **Data Export**: CSV, JSON, and visualization features
- 🚀 **Interactive CLI**: All interactive features function properly

## 🚀 **Installation Methods**

### **Method 1: Automated Windows Installation (Recommended)**

#### **Option A: Batch Script (Classic)**
```batch
# Download and run the Windows installer
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
install_windows.bat
```

#### **Option B: PowerShell Script (Modern)**
```powershell
# Download and run the PowerShell installer
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM
.\install_windows.ps1
```

### **Method 2: Manual Installation**

#### **With pip**
```bash
# Clone repository
git clone https://github.com/Darth-Hidious/PRISM.git
cd PRISM

# Install with pip
pip install -e .

# Verify installation
prism --help
```

#### **With uv (Fastest)**
```bash
# Install uv first (if not installed)
powershell -c "irm https://astral.sh/uv/install.ps1 | iex"

# Install PRISM
uv pip install -e .

# Verify installation
prism --help
```

#### **Development Installation**
```bash
# Install with development dependencies
pip install -e ".[dev,export,monitoring]"

# Run tests
pytest
```

### **Method 3: One-Line Installation**
```bash
# Direct from GitHub
pip install git+https://github.com/Darth-Hidious/PRISM.git
```

## 🔧 **Windows Requirements**

### **System Requirements**
- **OS**: Windows 10 (version 1903+) or Windows 11
- **Python**: 3.8 or higher
- **Terminal**: Windows Terminal (recommended) or PowerShell/Command Prompt
- **Memory**: 4GB RAM minimum, 8GB recommended
- **Storage**: 2GB free space

### **Python Installation**
1. Download Python from [python.org](https://python.org)
2. **Important**: Check "Add Python to PATH" during installation
3. Verify installation: `python --version`

### **Recommended Terminal Setup**
For the best experience with MARC27's PRISM logo and formatting:

#### **Windows Terminal (Best Experience)**
1. Install from Microsoft Store or [GitHub](https://github.com/microsoft/terminal)
2. Supports full Unicode and color formatting
3. Best ASCII art rendering

#### **PowerShell 7+ (Good)**
1. Install from [GitHub](https://github.com/PowerShell/PowerShell)
2. Modern features and better Unicode support

#### **Command Prompt (Basic)**
- Works but limited formatting support
- ASCII art may not display perfectly

## 🎨 **Windows-Specific Features**

### **Terminal Enhancements**
```bash
# Enable color output in older terminals
set FORCE_COLOR=1

# Use Windows Terminal for best experience
wt python -m app.cli
```

### **Path Configuration**
If `prism` command is not found after installation:
```bash
# Add Python Scripts to PATH
# Usually located at: C:\Users\YourName\AppData\Local\Programs\Python\Python3x\Scripts

# Or use module syntax
python -m app.cli --help
```

### **PowerShell Execution Policy**
If PowerShell scripts are blocked:
```powershell
# Allow script execution (run as Administrator)
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
```

## 📋 **Windows Installation Verification**

After installation, test these commands:

```bash
# Basic functionality
prism --version
prism --help

# Launch screen with logo
prism

# Database listing
prism list-databases

# Quick search test
prism search --database oqmd --elements Li --limit 3

# Interactive tutorial
prism getting-started
```

## 🛠 **Troubleshooting Windows Issues**

### **Common Issues and Solutions**

#### **1. Python Not Found**
```bash
# Error: 'python' is not recognized
# Solution: Reinstall Python with "Add to PATH" checked
# Or use: py instead of python
py -m app.cli --help
```

#### **2. ASCII Art Not Displaying**
```bash
# Issue: Logo appears as squares or strange characters
# Solution: Use Windows Terminal or update font
# Install a Nerd Font for better Unicode support
```

#### **3. Permission Errors**
```bash
# Error: Permission denied during installation
# Solution: Run Command Prompt as Administrator
# Or use --user flag
pip install --user -e .
```

#### **4. Module Import Errors**
```bash
# Error: ModuleNotFoundError
# Solution: Ensure you're in the right directory and Python path is correct
cd C:\path\to\PRISM
pip install -e .
```

#### **5. uv Installation Issues**
```powershell
# If uv installation fails
# Solution: Use pip instead or install manually
pip install -e .
```

## 🚀 **Windows Performance Tips**

### **Optimize Performance**
1. **Use Windows Terminal** for best rendering performance
2. **Install uv** for faster package management
3. **Use SSD storage** for better I/O performance
4. **Increase terminal buffer** for large search results

### **Memory Management**
```bash
# For large datasets, use limits
prism search --database oqmd --elements Si,O --limit 100

# Monitor memory usage
prism search --database nomad --elements Li --limit 50 --debug
```

## 📦 **Distribution on Windows**

### **Create Windows Executable**
```bash
# Install PyInstaller
pip install pyinstaller

# Create standalone executable
pyinstaller --onefile --name="marc27-prism" app/cli.py
```

### **Windows Package**
```bash
# Create wheel for distribution
python -m build

# Install from wheel
pip install dist/prism_platform-1.0.0-py3-none-any.whl
```

## ✅ **Verified Windows Versions**
- ✅ Windows 11 (22H2)
- ✅ Windows 10 (21H2, 22H2)
- ✅ Windows Server 2019/2022
- ✅ PowerShell 5.1, 7.x
- ✅ Command Prompt
- ✅ Windows Terminal
- ✅ Git Bash
- ✅ WSL (Windows Subsystem for Linux)

## 🆘 **Support**

For Windows-specific issues:
- **GitHub Issues**: [https://github.com/Darth-Hidious/PRISM/issues](https://github.com/Darth-Hidious/PRISM/issues)
- **Email**: team@marc27.com
- **Tag issues**: `windows` `installation` `compatibility`

---

**MARC27's PRISM Platform works seamlessly on Windows! 🪟✨**
