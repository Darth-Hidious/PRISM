#!/usr/bin/env python3
"""
Configuration Testing and Validation Script

This script validates the PRISM platform configuration system and shows
how environment variables are loaded and processed.
"""

import os
import sys
from pathlib import Path

# Add the project root to the path
sys.path.insert(0, str(Path(__file__).parent))

from app.core.config import get_settings, Settings
from rich.console import Console
from rich.table import Table
from rich.panel import Panel
from rich.text import Text

console = Console()

def test_configuration():
    """Test and display the current configuration."""
    
    console.print(Panel(
        "PRISM Platform Configuration Test",
        title="🔧 Configuration Validation",
        border_style="blue"
    ))
    
    try:
        settings = get_settings()
        console.print("✅ [green]Configuration loaded successfully![/green]\n")
        
        # Database connector settings
        connector_table = Table(title="Database Connector Settings")
        connector_table.add_column("Connector", style="cyan")
        connector_table.add_column("Base URL", style="magenta")
        connector_table.add_column("Rate Limit", justify="right", style="green")
        connector_table.add_column("Burst Size", justify="right", style="yellow")
        connector_table.add_column("Timeout", justify="right", style="red")
        
        connector_table.add_row(
            "JARVIS",
            settings.jarvis_base_url,
            str(settings.jarvis_rate_limit),
            str(settings.jarvis_burst_size),
            f"{settings.jarvis_timeout}s"
        )
        
        connector_table.add_row(
            "NOMAD",
            settings.nomad_base_url,
            str(settings.nomad_rate_limit),
            str(settings.nomad_burst_size),
            f"{settings.nomad_timeout}s"
        )
        
        connector_table.add_row(
            "OQMD",
            settings.oqmd_base_url,
            str(settings.oqmd_rate_limit),
            str(settings.oqmd_burst_size),
            f"{settings.oqmd_timeout}s"
        )
        
        console.print(connector_table)
        console.print()
        
        # Job processing settings
        job_table = Table(title="Job Processing Settings")
        job_table.add_column("Setting", style="cyan")
        job_table.add_column("Value", style="green")
        job_table.add_column("Description", style="dim")
        
        job_settings = [
            ("Batch Size", str(settings.batch_size), "Default batch size for processing"),
            ("Max Retries", str(settings.max_retries), "Maximum retry attempts"),
            ("Retry Delay", f"{settings.retry_delay}s", "Delay between retries"),
            ("Job Timeout", f"{settings.job_timeout}s", "Maximum job execution time"),
            ("Max Concurrent Jobs", str(settings.max_concurrent_jobs), "Maximum parallel jobs"),
            ("Cleanup Interval", f"{settings.job_cleanup_interval}s", "Job cleanup frequency")
        ]
        
        for setting, value, desc in job_settings:
            job_table.add_row(setting, value, desc)
        
        console.print(job_table)
        console.print()
        
        # Rate limiting settings
        rate_table = Table(title="Rate Limiting Configuration")
        rate_table.add_column("Setting", style="cyan")
        rate_table.add_column("Value", style="green")
        
        rate_settings = [
            ("Enabled", "✅ Yes" if settings.rate_limiter_enabled else "❌ No"),
            ("Backend", settings.rate_limiter_backend),
            ("Default Limit", f"{settings.rate_limiter_default_limit} req/min"),
            ("Default Period", f"{settings.rate_limiter_default_period}s"),
            ("Adaptive", "✅ Yes" if settings.rate_limiter_adaptive else "❌ No")
        ]
        
        for setting, value in rate_settings:
            rate_table.add_row(setting, value)
        
        console.print(rate_table)
        console.print()
        
        # Environment variable validation
        env_table = Table(title="Environment Variable Status")
        env_table.add_column("Variable", style="cyan")
        env_table.add_column("Status", style="bold")
        env_table.add_column("Current Value", style="green")
        
        env_vars = [
            ("JARVIS_BASE_URL", settings.jarvis_base_url),
            ("JARVIS_RATE_LIMIT", settings.jarvis_rate_limit),
            ("NOMAD_BASE_URL", settings.nomad_base_url),
            ("NOMAD_RATE_LIMIT", settings.nomad_rate_limit),
            ("OQMD_BASE_URL", settings.oqmd_base_url),
            ("BATCH_SIZE", settings.batch_size),
            ("MAX_RETRIES", settings.max_retries),
            ("RETRY_DELAY", settings.retry_delay),
        ]
        
        for var_name, current_value in env_vars:
            env_value = os.getenv(var_name)
            if env_value:
                status = "🌍 From ENV"
                display_value = str(env_value)
            else:
                status = "⚙️ Default"
                display_value = str(current_value)
            
            env_table.add_row(var_name, status, display_value)
        
        console.print(env_table)
        console.print()
        
        # CLI configuration
        cli_table = Table(title="CLI Configuration")
        cli_table.add_column("Setting", style="cyan")
        cli_table.add_column("Value", style="green")
        
        cli_settings = [
            ("Default Output Format", settings.cli_default_output_format),
            ("Default Batch Size", str(settings.cli_default_batch_size)),
            ("Progress Bar", "✅ Enabled" if settings.cli_progress_bar else "❌ Disabled"),
            ("Color Output", "✅ Enabled" if settings.cli_color_output else "❌ Disabled")
        ]
        
        for setting, value in cli_settings:
            cli_table.add_row(setting, value)
        
        console.print(cli_table)
        console.print()
        
        # Summary
        summary_text = Text()
        summary_text.append("Configuration Status: ", style="bold")
        summary_text.append("✅ All settings loaded successfully\n", style="green bold")
        summary_text.append(f"Environment: ", style="bold")
        summary_text.append(f"{settings.environment}\n", style="yellow")
        summary_text.append(f"Debug Mode: ", style="bold")
        summary_text.append("✅ Enabled" if settings.debug else "❌ Disabled", style="green" if settings.debug else "red")
        
        console.print(Panel(summary_text, title="📊 Configuration Summary", border_style="green"))
        
        return True
        
    except Exception as e:
        console.print(f"❌ [red]Configuration error: {e}[/red]")
        return False

def test_environment_file():
    """Test if .env file exists and is readable."""
    
    env_file = Path(".env")
    
    if env_file.exists():
        console.print("✅ [green].env file found[/green]")
        
        # Show some key environment variables
        console.print("\n📋 Key Environment Variables:")
        
        key_vars = [
            "JARVIS_BASE_URL",
            "JARVIS_RATE_LIMIT", 
            "NOMAD_BASE_URL",
            "NOMAD_RATE_LIMIT",
            "OQMD_BASE_URL",
            "BATCH_SIZE",
            "MAX_RETRIES",
            "RETRY_DELAY"
        ]
        
        for var in key_vars:
            value = os.getenv(var)
            if value:
                console.print(f"  • {var}: [green]{value}[/green]")
            else:
                console.print(f"  • {var}: [dim]not set (using default)[/dim]")
    else:
        console.print("⚠️ [yellow].env file not found - using default configuration[/yellow]")

def show_usage_examples():
    """Show examples of how to use the configuration."""
    
    console.print(Panel(
        """
# Example 1: Using configuration in code
from app.core.config import get_settings

settings = get_settings()
jarvis_url = settings.jarvis_base_url
batch_size = settings.batch_size

# Example 2: Override via environment variables
export BATCH_SIZE=100
export JARVIS_RATE_LIMIT=200
export DEVELOPMENT_MODE=true

# Example 3: Using in CLI
python cli_demo.py bulk-fetch -s jarvis -l ${BATCH_SIZE}

# Example 4: Docker environment
docker run -e JARVIS_RATE_LIMIT=50 -e BATCH_SIZE=25 prism:latest
        """.strip(),
        title="💡 Usage Examples",
        border_style="cyan"
    ))

if __name__ == "__main__":
    console.print("🚀 PRISM Configuration Testing\n")
    
    # Test environment file
    test_environment_file()
    console.print()
    
    # Test configuration loading
    if test_configuration():
        console.print("\n🎉 [green bold]All configuration tests passed![/green bold]")
        
        # Show usage examples
        console.print()
        show_usage_examples()
    else:
        console.print("\n💥 [red bold]Configuration test failed![/red bold]")
        sys.exit(1)
