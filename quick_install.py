#!/usr/bin/env python3
"""
PRISM Platform Installer
Installs the project in editable mode.
"""

import subprocess
import sys
import os

def print_banner():
    """Display a simplified installation banner."""
    print("""
    ===================================================
    PRISM Platform Installer
    ===================================================
    """)

def run_command(cmd, description):
    """Run a shell command with error handling."""
    print(f"🔄 {description}...")
    try:
        # Using a list of arguments is safer than shell=True
        result = subprocess.run(cmd, check=True, capture_output=True, text=True)
        print(f"✅ {description} completed successfully.")
        if result.stdout:
            print(result.stdout)
        return True
    except subprocess.CalledProcessError as e:
        print(f"❌ {description} failed.")
        print("--- STDERR ---")
        print(e.stderr)
        print("--- STDOUT ---")
        print(e.stdout)
        return False

def main():
    """Main installation process."""
    print_banner()

    # The command to install the package in editable mode
    install_command = [sys.executable, "-m", "pip", "install", "-e", "."]

    if run_command(install_command, "Installing PRISM in editable mode"):
        print("\n🎉 Installation successful!")
        print("You can now run the CLI using the 'prism' command.")
        print("\nTry running: prism --help")
    else:
        print("\n❌ Installation failed.")
        print("Please check the error messages above.")
        print("You may need to manually run the command:")
        print(f"   {sys.executable} -m pip install -e .")
        sys.exit(1)

if __name__ == "__main__":
    main()
