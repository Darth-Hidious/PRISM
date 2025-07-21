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

def run_command(cmd, description, allow_failure=False, venv_path=None):
    """Run a shell command with error handling, optionally in a venv"""
    print(f"🔄 {description}...")

    # Activate venv if provided
    if venv_path:
        if sys.platform == "win32":
            activate_script = venv_path / "Scripts" / "activate.bat"
            cmd = f'"{activate_script}" && {cmd}'
        else:
            activate_script = venv_path / "bin" / "activate"
            cmd = f'source "{activate_script}" && {cmd}'

    try:
        result = subprocess.run(cmd, shell=True, check=True, capture_output=True, text=True, executable="/bin/bash")
        print(f"✅ {description} completed")
        return True, result.stdout
    except subprocess.CalledProcessError as e:
        if allow_failure:
            print(f"⚠️  {description} failed (continuing): {e.stderr.strip()}")
        else:
            print(f"❌ {description} failed: {e.stderr.strip()}")
        return False, e.stderr

def create_venv(venv_path):
    """Create a virtual environment if it doesn't exist"""
    if not venv_path.exists():
        print(f"🌱 Creating virtual environment at {venv_path}...")
        success, output = run_command(f"python3 -m venv {venv_path}", "Creating venv")
        if not success:
            print("❌ Could not create virtual environment. Please check your Python installation.")
            sys.exit(1)
        print("✅ Virtual environment created.")
    else:
        print(f"Found existing virtual environment at {venv_path}")

def install_minimal_deps(venv_path):
    """Install minimal dependencies for CLI"""
    print("📦 Installing minimal CLI dependencies...")
    
    # Try pip install for basic packages that should work anywhere
    basic_packages = ["click", "rich"]
    
    for package in basic_packages:
        success, output = run_command(f"pip install {package}", f"Installing {package}", venv_path=venv_path, allow_failure=True)
        if not success:
            print(f"⚠️  Failed to install {package}, trying alternative...")
    
    return True

def install_full_deps(venv_path):
    """Try to install full dependencies, with fallback"""
    print("🚀 Attempting full dependency installation...")
    
    # Install all dependencies including the commonly missing ones
    additional_packages = ["pandas", "numpy", "matplotlib", "seaborn", "scikit-learn"]
    for package in additional_packages:
        success, output = run_command(f"pip install {package}", f"Installing {package}", venv_path=venv_path, allow_failure=True)
        if success:
            print(f"✅ {package} installed successfully")
        else:
            print(f"⚠️  {package} installation failed, continuing...")
    
    # Try to install from requirements.txt with fallback
    success, output = run_command("pip install -e .", "Installing PRISM package", venv_path=venv_path, allow_failure=True)
    
    if not success:
        print("⚠️  Full installation failed, trying minimal installation...")
        return install_minimal_deps(venv_path)
    
    return True

def verify_cli(venv_path):
    """Verify CLI functionality"""
    print("🔍 Verifying CLI installation...")
    
    # Test CLI import
    py_executable = "python"
    cli_command = f'{py_executable} -c "from app.cli import cli; print(\'CLI import successful\')"'

    success, output = run_command(cli_command, "Testing CLI import", venv_path=venv_path)
    
    if success:
        print("✅ PRISM CLI is ready!")
        print("\n🎉 Installation complete! Try these commands:")
        if sys.platform == "win32":
            print(f"  {venv_path}\\Scripts\\activate")
        else:
            print(f"  source {venv_path}/bin/activate")
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
    
    project_dir = Path(__file__).parent

    # Check if we're in the right directory
    if not (project_dir / "setup.py").exists() and not (project_dir / "pyproject.toml").exists():
        print("❌ Please run this script from the PRISM project directory")
        print("   Make sure you're in the folder containing setup.py or pyproject.toml")
        sys.exit(1)

    venv_path = project_dir / ".venv"
    
    print("🚀 Starting PRISM Platform installation...")
    print("📍 Installation mode: Internal testing (development)")
    
    # Step 1: Create venv
    create_venv(venv_path)

    # Step 2: Try full installation, fall back to minimal
    print("\n🔧 Installing dependencies...")
    if not install_full_deps(venv_path):
        print("❌ Dependency installation failed")
        sys.exit(1)
    
    # Step 3: Verify CLI works
    print("\n🔍 Verifying installation...")
    if verify_cli(venv_path):
        print("\n🎊 SUCCESS! PRISM Platform is ready for internal testing!")
        print("\n📖 Next steps:")
        if sys.platform == "win32":
            print(f"   1. Activate venv: .\\.venv\\Scripts\\activate")
        else:
            print(f"   1. Activate venv: source .venv/bin/activate")
        print("   2. Run: python -m app.cli")
        print("   3. Try: python -m app.cli getting-started")
        print("   4. Test: python -m app.cli test-database")
    else:
        print("\n❌ Installation verification failed")
        print("💡 Try manual installation:")
        if sys.platform == "win32":
            print(f"   1. Activate venv: .\\.venv\\Scripts\\activate")
        else:
            print(f"   1. Activate venv: source .venv/bin/activate")
        print("   2. pip install click rich")
        print("   3. python -m app.cli --help")
        sys.exit(1)

if __name__ == "__main__":
    main()
