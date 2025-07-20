#!/usr/bin/env python3
"""
PRISM CLI Entry Point

This script provides a convenient way to run the PRISM CLI tool.
Can be used as:
    python cli_runner.py [COMMAND] [OPTIONS]
    
Or make it executable and run directly:
    chmod +x cli_runner.py
    ./cli_runner.py [COMMAND] [OPTIONS]
"""

import sys
from pathlib import Path

# Add the app directory to Python path
sys.path.insert(0, str(Path(__file__).parent))

if __name__ == '__main__':
    from app.cli import cli
    cli()
