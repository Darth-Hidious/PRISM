#!/usr/bin/env python3
"""
Quick PRISM Platform Installer
Usdef main():
    print("""
    ███╗   ███╗ █████╗ ██████╗  ██████╗██████╗ ███████╗
    ████╗ ████║██╔══██╗██╔══██╗██╔════╝╚════██╗╚════██║
    ██╔████╔██║███████║██████╔╝██║      █████╔╝ █████╔╝
    ██║╚██╔╝██║██╔══██║██╔══██╗██║     ██╔═══╝ ██╔═══╝ 
    ██║ ╚═╝ ██║██║  ██║██║  ██║╚██████╗███████╗███████╗
    ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝╚══════╝╚══════╝
                                                        
         ██████╗ ██████╗ ██╗███████╗███╗   ███╗
         ██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
         ██████╔╝██████╔╝██║███████╗██╔████╔██║
         ██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
         ██║     ██║  ██║██║███████║██║ ╚═╝ ██║
         ╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝

MARC27's Platform for Research in Intelligent Synthesis of Materials
Quick Installation Script
""")"""quick_install.py
"""

import subprocess
import sys
import os
from pathlib import Path

def run_command(cmd, description):
    """Run a shell command with error handling"""
    print(f"🔄 {description}...")
    try:
        result = subprocess.run(cmd, shell=True, check=True, capture_output=True, text=True)
        print(f"✅ {description} completed")
        return result.stdout
    except subprocess.CalledProcessError as e:
        print(f"❌ {description} failed: {e.stderr}")
        return None

def check_uv():
    """Check if uv is installed"""
    result = subprocess.run("uv --version", shell=True, capture_output=True)
    return result.returncode == 0

def install_uv():
    """Install uv package manager"""
    print("📦 Installing uv package manager...")
    if sys.platform.startswith('win'):
        cmd = "powershell -c \"irm https://astral.sh/uv/install.ps1 | iex\""
    else:
        cmd = "curl -LsSf https://astral.sh/uv/install.sh | sh"
    
    return run_command(cmd, "Installing uv")

def install_prism():
    """Install PRISM platform"""
    if check_uv():
        print("🚀 Installing PRISM with uv...")
        return run_command("uv pip install -e .", "Installing PRISM platform")
    else:
        print("🐍 Installing PRISM with pip...")
        return run_command("pip install -e .", "Installing PRISM platform")

def verify_installation():
    """Verify PRISM installation"""
    result = subprocess.run("prism --version", shell=True, capture_output=True)
    if result.returncode == 0:
        print("✅ PRISM installed successfully!")
        print("\n🎉 Quick start commands:")
        print("  prism --help          # Show all commands")
        print("  prism list-databases  # List available databases")
        print("  prism getting-started # Interactive tutorial")
        return True
    else:
        print("❌ PRISM installation verification failed")
        return False

def main():
    print("""
██████╗ ██████╗ ██╗███████╗███╗   ███╗
██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
██████╔╝██████╔╝██║███████╗██╔████╔██║
██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
         ██║     ██║  ██║██║███████║██║ ╚═╝ ██║
         ╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝

Platform for Research in Intelligent Synthesis of Materials
Quick Installation Script
""")
    
    # Check if we're in the right directory
    if not Path("setup.py").exists() and not Path("pyproject.toml").exists():
        print("❌ Please run this script from the PRISM project directory")
        sys.exit(1)
    
    # Install uv if not present
    if not check_uv():
        print("⚠️  uv not found, installing...")
        install_uv()
    
    # Install PRISM
    if install_prism():
        verify_installation()
    else:
        print("❌ Installation failed")
        sys.exit(1)

if __name__ == "__main__":
    main()
