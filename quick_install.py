#!/usr/bin/env python3
"""
Quick PRISM Platform Installer
Handles all dependencies and setup automatically
"""

import subprocess
import sys
import os
from pathlib import Path

def print_banner():
    """Display MARC27 PRISM banner"""
    print("""
    ███╗   ███╗ █████╗ ██████╗  ██████╗██████╗ ███████╗
    ████╗ ████║██╔══██╗██╔══██╗██╔════╝╚════██╗╚════██║
    ██╔████╔██║███████║██████╔╝██║      █████╔╝    ██╔╝
    ██║╚██╔╝██║██╔══██║██╔══██╗██║     ██╔═══╝    ██╔╝ 
    ██║ ╚═╝ ██║██║  ██║██║  ██║╚██████╗███████╗   ██║  
    ╚═╝     ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝╚══════╝   ╚═╝  
                                                        
         ██████╗ ██████╗ ██╗███████╗███╗   ███╗
         ██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
         ██████╔╝██████╔╝██║███████╗██╔████╔██║
         ██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
         ██║     ██║  ██║██║███████║██║ ╚═╝ ██║
         ╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝

Platform for Research in Intelligent Synthesis of Materials
One-Command Installation Script
""")

def run_command(cmd, description, allow_failure=False):
    """Run a shell command with error handling"""
    print(f"🔄 {description}...")
    try:
        result = subprocess.run(cmd, shell=True, check=True, capture_output=True, text=True)
        print(f"✅ {description} completed")
        return True, result.stdout
    except subprocess.CalledProcessError as e:
        if allow_failure:
            print(f"⚠️  {description} failed (continuing): {e.stderr.strip()}")
            return False, e.stderr
        else:
            print(f"❌ {description} failed: {e.stderr.strip()}")
            return False, e.stderr

def install_minimal_deps():
    """Install minimal dependencies for CLI"""
    print("📦 Installing minimal CLI dependencies...")
    
    # Try pip install for basic packages that should work anywhere
    basic_packages = ["click", "rich"]
    
    for package in basic_packages:
        success, output = run_command(f"pip install {package}", f"Installing {package}", allow_failure=True)
        if not success:
            print(f"⚠️  Failed to install {package}, trying alternative...")
    
    return True

def install_full_deps():
    """Try to install full dependencies, with fallback"""
    print("🚀 Attempting full dependency installation...")
    
    # Try to install from requirements.txt with fallback
    success, output = run_command("pip install -e .", "Installing PRISM package", allow_failure=True)
    
    if not success:
        print("⚠️  Full installation failed, trying minimal installation...")
        return install_minimal_deps()
    
    return True

def verify_cli():
    """Verify CLI functionality"""
    print("🔍 Verifying CLI installation...")
    
    # Test CLI import
    success, output = run_command(
        'python -c "from app.cli import cli; print(\'CLI import successful\')"',
        "Testing CLI import"
    )
    
    if success:
        print("✅ PRISM CLI is ready!")
        print("\n🎉 Installation complete! Try these commands:")
        print("  python -m app.cli --help          # Show all commands")
        print("  python -m app.cli                 # Interactive interface")
        print("  python -m app.cli getting-started # Tutorial")
        print("  python -m app.cli test-database   # Test connections")
        return True
    else:
        print("❌ CLI verification failed")
        return False

def main():
    """Main installation process"""
    print_banner()
    
    # Check if we're in the right directory
    if not (Path("setup.py").exists() or Path("pyproject.toml").exists()):
        print("❌ Please run this script from the PRISM project directory")
        print("   Make sure you're in the folder containing setup.py or pyproject.toml")
        sys.exit(1)
    
    print("🚀 Starting PRISM Platform installation...")
    print("📍 Installation mode: Internal testing (development)")
    
    # Step 1: Try full installation, fall back to minimal
    print("\n🔧 Installing dependencies...")
    if not install_full_deps():
        print("❌ Dependency installation failed")
        sys.exit(1)
    
    # Step 2: Verify CLI works
    print("\n🔍 Verifying installation...")
    if verify_cli():
        print("\n🎊 SUCCESS! PRISM Platform is ready for internal testing!")
        print("\n📖 Next steps:")
        print("   1. Run: python -m app.cli")
        print("   2. Try: python -m app.cli getting-started")
        print("   3. Test: python -m app.cli test-database")
    else:
        print("\n❌ Installation verification failed")
        print("💡 Try manual installation:")
        print("   pip install click rich")
        print("   python -m app.cli --help")
        sys.exit(1)

if __name__ == "__main__":
    main()
