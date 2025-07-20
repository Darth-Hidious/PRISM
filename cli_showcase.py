#!/usr/bin/env python3
"""
PRISM CLI Demonstration Script

This script demonstrates the full capabilities of the PRISM CLI tool
by running various commands and showing their outputs.

Usage:
    python cli_showcase.py
"""

import subprocess
import sys
import time
from rich.console import Console
from rich.panel import Panel
from rich.text import Text

console = Console()

def run_demo_command(description: str, command: str, sleep_after: float = 2.0):
    """Run a demo command and display its output"""
    
    # Create a panel for the command description
    console.print(Panel(
        Text(description, style="bold cyan"),
        title="Demo Command",
        border_style="blue"
    ))
    
    # Show the command being executed
    console.print(f"[bold green]$ {command}[/bold green]")
    console.print()
    
    # Execute the command
    try:
        result = subprocess.run(
            command.split(),
            capture_output=True,
            text=True,
            cwd='.'
        )
        
        if result.stdout:
            console.print(result.stdout)
        
        if result.stderr:
            console.print(f"[red]{result.stderr}[/red]")
            
    except Exception as e:
        console.print(f"[red]Error running command: {e}[/red]")
    
    console.print("-" * 80)
    time.sleep(sleep_after)


def main():
    """Run the CLI demonstration"""
    
    console.print(Panel(
        Text("PRISM Platform CLI Tool Demonstration", style="bold magenta"),
        title="Welcome to PRISM CLI Demo",
        border_style="magenta"
    ))
    
    console.print("[yellow]This demonstration showcases the PRISM CLI capabilities.[/yellow]")
    console.print("[yellow]All commands use mock data for demonstration purposes.[/yellow]")
    console.print()
    
    # Demo commands to run
    demos = [
        {
            "description": "Display CLI help and available commands",
            "command": "python cli_demo.py --help"
        },
        {
            "description": "List all available data sources",
            "command": "python cli_demo.py list-sources"
        },
        {
            "description": "Test connections to data sources",
            "command": "python cli_demo.py test-connection"
        },
        {
            "description": "Fetch silicon-containing materials from JARVIS",
            "command": "python cli_demo.py fetch-material -s jarvis -e Si"
        },
        {
            "description": "Search for materials by formula in NOMAD",
            "command": "python cli_demo.py fetch-material -s nomad -f TiO2"
        },
        {
            "description": "Perform bulk fetch from all sources (limited to 5 materials)",
            "command": "python cli_demo.py bulk-fetch -s all -l 5"
        },
        {
            "description": "Show job queue status and statistics",
            "command": "python cli_demo.py queue-status"
        },
        {
            "description": "Display system monitoring information",
            "command": "python cli_demo.py monitor"
        },
        {
            "description": "Demonstrate dry-run mode for bulk operations",
            "command": "python cli_demo.py bulk-fetch -s jarvis -l 10 --dry-run"
        },
        {
            "description": "Show sources in JSON format",
            "command": "python cli_demo.py list-sources --format json"
        }
    ]
    
    # Run each demonstration
    for i, demo in enumerate(demos, 1):
        console.print(f"\n[bold blue]Demo {i}/{len(demos)}[/bold blue]")
        run_demo_command(demo["description"], demo["command"])
    
    # Final summary
    console.print(Panel(
        Text("CLI Demonstration Complete!\n\n"
             "The PRISM CLI tool provides comprehensive functionality for:\n"
             "• Material data fetching and searching\n"
             "• Bulk operations with progress tracking\n"
             "• System monitoring and health checks\n"
             "• Queue management and job processing\n"
             "• Data export and configuration management\n\n"
             "For full documentation, see CLI_DOCUMENTATION.md",
             style="bold green"),
        title="Demonstration Summary",
        border_style="green"
    ))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        console.print("\n[yellow]Demonstration interrupted by user[/yellow]")
        sys.exit(0)
    except Exception as e:
        console.print(f"\n[red]Demonstration error: {e}[/red]")
        sys.exit(1)
