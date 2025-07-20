#!/usr/bin/env python3
"""
Simplified CLI demo that works without full database setup.

This version demonstrates the CLI functionality with mock data,
allowing testing without database or external API dependencies.
"""

import sys
from pathlib import Path

# Add the current directory to Python path for imports
sys.path.insert(0, str(Path(__file__).parent))

import asyncio
import json
from datetime import datetime
from typing import Dict, List, Optional, Any

import click
from rich.console import Console
from rich.table import Table
from rich.progress import Progress
from rich.panel import Panel
from rich.text import Text
from rich import print as rprint

console = Console()

# Mock data for demonstration
MOCK_MATERIALS = {
    'jarvis': [
        {
            'jid': 'JVASP-1000',
            'formula': 'Si2',
            'formation_energy_peratom': -5.4,
            'elements': ['Si'],
            'structure': {'lattice_type': 'cubic'},
            'band_gap': 1.1
        },
        {
            'jid': 'JVASP-1001', 
            'formula': 'GaAs',
            'formation_energy_peratom': -0.8,
            'elements': ['Ga', 'As'],
            'structure': {'lattice_type': 'cubic'},
            'band_gap': 1.4
        }
    ],
    'nomad': [
        {
            'entry_id': 'test-entry-001',
            'chemical_formula': 'TiO2',
            'elements': ['Ti', 'O'],
            'energy_total': -234.5,
            'band_gap': 3.2,
            'space_group': 'P42/mnm'
        },
        {
            'entry_id': 'test-entry-002',
            'chemical_formula': 'MgO',
            'elements': ['Mg', 'O'],
            'energy_total': -89.1,
            'band_gap': 7.8,
            'space_group': 'Fm-3m'
        }
    ]
}

MOCK_JOBS = [
    {'id': 1, 'source': 'jarvis', 'status': 'completed', 'created_at': '2024-01-15 10:30:00'},
    {'id': 2, 'source': 'nomad', 'status': 'pending', 'created_at': '2024-01-15 11:15:00'},
    {'id': 3, 'source': 'jarvis', 'status': 'failed', 'created_at': '2024-01-15 12:00:00'},
    {'id': 4, 'source': 'nomad', 'status': 'running', 'created_at': '2024-01-15 12:30:00'},
]

class MockConnector:
    """Mock connector for demonstration purposes"""
    
    def __init__(self, source_type: str):
        self.source_type = source_type
        
    async def health_check(self) -> bool:
        """Simulate health check"""
        # Simulate some delay
        await asyncio.sleep(0.1)
        return True
    
    async def search_materials(self, **kwargs) -> List[Dict[str, Any]]:
        """Mock material search"""
        await asyncio.sleep(0.2)  # Simulate API delay
        materials = MOCK_MATERIALS.get(self.source_type, [])
        
        # Apply basic filtering
        if 'elements' in kwargs:
            requested_elements = kwargs['elements']
            materials = [
                m for m in materials 
                if any(elem in m.get('elements', []) for elem in requested_elements)
            ]
        
        if 'formula' in kwargs:
            formula = kwargs['formula']
            materials = [
                m for m in materials
                if m.get('formula', m.get('chemical_formula', '')).startswith(formula)
            ]
            
        return materials
    
    async def get_material_by_id(self, material_id: str) -> Dict[str, Any]:
        """Mock get material by ID"""
        await asyncio.sleep(0.1)
        materials = MOCK_MATERIALS.get(self.source_type, [])
        for material in materials:
            if (material.get('jid') == material_id or 
                material.get('entry_id') == material_id):
                return material
        return {}
    
    async def fetch_bulk(self, limit: int = 100, offset: int = 0) -> List[Dict[str, Any]]:
        """Mock bulk fetch"""
        await asyncio.sleep(0.3)
        materials = MOCK_MATERIALS.get(self.source_type, [])
        return materials[offset:offset+limit]


@click.group()
@click.option('--debug', is_flag=True, help='Enable debug mode')
def cli(debug):
    """PRISM Platform CLI Demo - Simplified version for testing"""
    if debug:
        console.print("[yellow]Demo mode - using mock data[/yellow]")


@cli.command()
@click.option('--source', '-s', required=True, 
              type=click.Choice(['jarvis', 'nomad']),
              help='Data source to fetch from')
@click.option('--material-id', '-m', help='Specific material ID to fetch')
@click.option('--formula', '-f', help='Chemical formula to search for')
@click.option('--elements', '-e', help='Comma-separated list of elements')
@click.option('--output', '-o', help='Output file path')
@click.option('--format', 'output_format', default='json',
              type=click.Choice(['json', 'csv', 'yaml']),
              help='Output format')
def fetch_material(source, material_id, formula, elements, output, output_format):
    """Fetch material data from a specific source"""
    
    with console.status(f"[bold green]Fetching material from {source.upper()}..."):
        connector = MockConnector(source)
        
        # Build query parameters
        query_params = {}
        if material_id:
            query_params['material_id'] = material_id
        if formula:
            query_params['formula'] = formula
        if elements:
            query_params['elements'] = [e.strip() for e in elements.split(',')]
        
        if not query_params and not material_id:
            console.print("[red]Must specify at least one search parameter[/red]")
            return
        
        # Fetch the material
        try:
            if material_id:
                result = asyncio.run(connector.get_material_by_id(material_id))
                if not result:
                    result = []
                else:
                    result = [result]
            else:
                result = asyncio.run(connector.search_materials(**query_params))
            
            if not result:
                console.print("[yellow]No materials found matching criteria[/yellow]")
                return
            
            # Format output
            if output_format == 'json':
                formatted_data = json.dumps(result, indent=2)
            elif output_format == 'csv':
                # Simple CSV conversion
                if result:
                    import io
                    import csv
                    output_buffer = io.StringIO()
                    fieldnames = result[0].keys()
                    writer = csv.DictWriter(output_buffer, fieldnames=fieldnames)
                    writer.writeheader()
                    for row in result:
                        # Convert complex fields to strings
                        simple_row = {k: str(v) for k, v in row.items()}
                        writer.writerow(simple_row)
                    formatted_data = output_buffer.getvalue()
                else:
                    formatted_data = ""
            elif output_format == 'yaml':
                try:
                    import yaml
                    formatted_data = yaml.dump(result, default_flow_style=False)
                except ImportError:
                    console.print("[red]PyYAML not installed. Using JSON format instead.[/red]")
                    formatted_data = json.dumps(result, indent=2)
            
            # Output results
            if output:
                Path(output).write_text(formatted_data)
                console.print(f"[green]Results saved to {output}[/green]")
            else:
                console.print(formatted_data)
                
        except Exception as e:
            console.print(f"[red]Failed to fetch material: {e}[/red]")


@cli.command()
@click.option('--source', '-s', required=True,
              type=click.Choice(['jarvis', 'nomad', 'all']),
              help='Data source(s) for bulk fetch')
@click.option('--elements', '-e', help='Comma-separated list of elements')
@click.option('--limit', '-l', default=10, type=int,
              help='Maximum number of materials to fetch')
@click.option('--batch-size', '-b', default=5, type=int,
              help='Batch size for processing')
@click.option('--dry-run', is_flag=True, help='Show what would be done without executing')
def bulk_fetch(source, elements, limit, batch_size, dry_run):
    """Perform bulk material fetching with progress tracking"""
    
    sources = ['jarvis', 'nomad'] if source == 'all' else [source]
    
    if dry_run:
        console.print("[yellow]DRY RUN MODE - No actual fetching will be performed[/yellow]")
        for source_name in sources:
            console.print(f"Would fetch {limit} materials from {source_name.upper()}")
        return
    
    with Progress() as progress:
        for source_name in sources:
            task = progress.add_task(f"[green]Fetching from {source_name.upper()}", total=limit)
            connector = MockConnector(source_name)
            
            # Process in batches
            fetched_count = 0
            for batch_start in range(0, limit, batch_size):
                batch_end = min(batch_start + batch_size, limit)
                
                try:
                    batch_results = asyncio.run(
                        connector.fetch_bulk(limit=batch_end-batch_start, offset=batch_start)
                    )
                    fetched_count += len(batch_results)
                    progress.update(task, advance=len(batch_results))
                    
                except Exception as e:
                    console.print(f"[red]Error in batch {batch_start}-{batch_end}: {e}[/red]")
                    progress.update(task, advance=batch_size)
            
            console.print(f"[green]Completed {source_name.upper()}: {fetched_count} materials fetched[/green]")


@cli.command()
@click.option('--format', 'output_format', default='table',
              type=click.Choice(['table', 'json', 'list']),
              help='Output format')
def list_sources(output_format):
    """List all available data sources and their status"""
    
    sources_data = [
        {'id': 1, 'name': 'JARVIS-DFT', 'type': 'jarvis', 'status': 'active', 'description': 'JARVIS materials database'},
        {'id': 2, 'name': 'NOMAD Lab', 'type': 'nomad', 'status': 'active', 'description': 'NOMAD repository'},
    ]
    
    if output_format == 'table':
        table = Table(title="Available Data Sources")
        table.add_column("ID", style="cyan")
        table.add_column("Name", style="magenta")
        table.add_column("Type", style="green")
        table.add_column("Status", style="yellow")
        table.add_column("Description")
        
        for source in sources_data:
            table.add_row(
                str(source['id']),
                source['name'],
                source['type'],
                source['status'],
                source['description']
            )
        
        console.print(table)
        
    elif output_format == 'json':
        console.print(json.dumps(sources_data, indent=2))
        
    elif output_format == 'list':
        for source in sources_data:
            console.print(f"• {source['name']} ({source['type']}) - {source['status']}")


@cli.command()
@click.option('--source', '-s', 
              type=click.Choice(['jarvis', 'nomad', 'all']),
              default='all', help='Source to test')
@click.option('--timeout', '-t', default=30, type=int,
              help='Connection timeout in seconds')
def test_connection(source, timeout):
    """Test connection to data sources"""
    
    sources_to_test = ['jarvis', 'nomad'] if source == 'all' else [source]
    
    with console.status("[bold green]Testing connections..."):
        results = {}
        
        for source_name in sources_to_test:
            try:
                connector = MockConnector(source_name)
                start_time = datetime.now()
                
                test_result = asyncio.run(
                    asyncio.wait_for(
                        connector.health_check(),
                        timeout=timeout
                    )
                )
                end_time = datetime.now()
                
                results[source_name] = {
                    'status': 'success' if test_result else 'failed',
                    'response_time': (end_time - start_time).total_seconds(),
                    'message': 'Connection successful (mock)' if test_result else 'Connection failed'
                }
                
            except asyncio.TimeoutError:
                results[source_name] = {
                    'status': 'timeout',
                    'response_time': timeout,
                    'message': f'Connection timed out after {timeout}s'
                }
            except Exception as e:
                results[source_name] = {
                    'status': 'error',
                    'response_time': None,
                    'message': str(e)
                }
    
    # Display results
    table = Table(title="Connection Test Results")
    table.add_column("Source", style="cyan")
    table.add_column("Status", style="bold")
    table.add_column("Response Time", style="green")
    table.add_column("Message")
    
    for source_name, result in results.items():
        status_color = {
            'success': 'green',
            'failed': 'red',
            'timeout': 'yellow',
            'error': 'red'
        }.get(result['status'], 'white')
        
        response_time = f"{result['response_time']:.3f}s" if result['response_time'] else "N/A"
        
        table.add_row(
            source_name.upper(),
            f"[{status_color}]{result['status'].upper()}[/{status_color}]",
            response_time,
            result['message']
        )
    
    console.print(table)


@cli.command()
def queue_status():
    """Show job queue status and statistics (mock data)"""
    
    # Mock job statistics
    total_jobs = len(MOCK_JOBS)
    pending_jobs = len([j for j in MOCK_JOBS if j['status'] == 'pending'])
    running_jobs = len([j for j in MOCK_JOBS if j['status'] == 'running'])
    completed_jobs = len([j for j in MOCK_JOBS if j['status'] == 'completed'])
    failed_jobs = len([j for j in MOCK_JOBS if j['status'] == 'failed'])
    
    # Summary panel
    summary_text = Text()
    summary_text.append(f"Total Jobs: {total_jobs}\n", style="bold")
    summary_text.append(f"Queue Health: ", style="bold")
    summary_text.append("DEMO MODE", style="yellow bold")
    
    console.print(Panel(summary_text, title="Queue Overview (Demo)", border_style="blue"))
    
    # Status breakdown table
    status_table = Table(title="Job Status Breakdown")
    status_table.add_column("Status", style="bold")
    status_table.add_column("Count", justify="right")
    status_table.add_column("Percentage", justify="right")
    
    for status, count, color in [
        ("Pending", pending_jobs, "yellow"),
        ("Running", running_jobs, "blue"),
        ("Completed", completed_jobs, "green"),
        ("Failed", failed_jobs, "red")
    ]:
        percentage = (count / max(total_jobs, 1)) * 100
        status_table.add_row(
            f"[{color}]{status}[/{color}]",
            f"[{color}]{count}[/{color}]",
            f"[{color}]{percentage:.1f}%[/{color}]"
        )
    
    console.print(status_table)
    
    # Jobs table
    jobs_table = Table(title="Recent Jobs")
    jobs_table.add_column("ID", style="cyan")
    jobs_table.add_column("Source", style="magenta")
    jobs_table.add_column("Status", style="bold")
    jobs_table.add_column("Created At", style="blue")
    
    for job in MOCK_JOBS:
        status_color = {
            'pending': 'yellow',
            'running': 'blue',
            'completed': 'green',
            'failed': 'red'
        }.get(job['status'], 'white')
        
        jobs_table.add_row(
            str(job['id']),
            job['source'].upper(),
            f"[{status_color}]{job['status'].upper()}[/{status_color}]",
            job['created_at']
        )
    
    console.print(jobs_table)


@cli.command()
def monitor():
    """Monitor system performance (demo version)"""
    
    console.print(Panel(
        "System Monitor Demo\n"
        "This would show real-time system metrics\n"
        "in the full version with database connection.",
        title="Monitor (Demo Mode)",
        border_style="green"
    ))
    
    # Mock metrics
    metrics_table = Table(title="Demo Metrics")
    metrics_table.add_column("Metric", style="cyan")
    metrics_table.add_column("Value", style="green")
    
    metrics_table.add_row("Total Materials", "2,500")
    metrics_table.add_row("Active Connections", "2")
    metrics_table.add_row("Queue Length", "4")
    metrics_table.add_row("Success Rate", "92.5%")
    
    console.print(metrics_table)


if __name__ == '__main__':
    cli()
