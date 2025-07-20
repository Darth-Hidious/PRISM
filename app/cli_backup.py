#!/usr/bin/env python3
"""
PRISM Platform CLI Tool

A comprehensive command-line interface for managing the PRISM data ingestion platform.
Provides commands for material fetching, bulk operations, queue management, and more.

Usage:
    python -m app.cli [COMMAND] [OPTIONS]
    
Available Commands:
    fetch-material      Fetch material from a specific source
    bulk-fetch          Perform bulk material fetching
    list-sources        List all available data sources
    test-connection     Test connection to data sources
    queue-status        Show job queue status
    retry-failed-jobs   Retry failed jobs
    export-data         Export data to various formats
    monitor             Monitor system performance
    config              Manage configuration settings
"""

import asyncio
import json
import sys
from datetime import datetime, timedelta
from pathlib import Path
from typing import Dict, List, Optional, Any

import click
from rich.console import Console
from rich.table import Table
from rich.progress import Progress, TaskID
from rich.panel import Panel
from rich.text import Text
from rich.prompt import Confirm, Prompt
from rich.tree import Tree
from rich import print as rprint
from app.services.connectors.base_connector import StandardizedMaterial, MaterialStructure, MaterialProperties, MaterialMetadata

# Import our application modules
try:
    from app.core.config import get_settings
    from app.services.connectors.jarvis_connector import JarvisConnector
    from app.services.connectors.nomad_connector import NOMADConnector
    from app.services.connectors.oqmd_connector import OQMDConnector
    from app.services.connectors.cod_connector import CODConnector
    from app.services.enhanced_nomad_connector import EnhancedNOMADConnector, create_progress_printer
    from app.services.job_processor import JobProcessor
    from app.services.job_scheduler import JobScheduler
    from app.services.data_viewer import MaterialsDataViewer
    from app.db.database import get_db_session, init_db_sync
    from app.db.models import Job, DataSource
    from app.schemas import JobStatus
    
    # Check if database needs initialization
    try:
        init_db_sync()
    except Exception as db_error:
        console = Console()
        console.print("[yellow]Database initialization issue detected.[/yellow]")
        console.print(f"[red]Error: {db_error}[/red]")
        console.print("[blue]Run 'python init_database.py' to set up PostgreSQL properly.[/blue]")
except ImportError as e:
    print(f"Import error: {e}")
    print("Please ensure you're running from the project root directory")
    sys.exit(1)

console = Console()
settings = get_settings()


def get_nomad_config() -> Dict[str, Any]:
    """Get NOMAD configuration from settings."""
    return {
        "base_url": settings.nomad_base_url,
        "timeout": settings.nomad_timeout,
        "max_retries": settings.max_retries,
        "requests_per_second": settings.nomad_rate_limit / 60.0,  # Convert per minute to per second
        "burst_capacity": settings.nomad_burst_size,
        "cache_ttl": 3600,
        "api_key": getattr(settings, 'nomad_api_key', None)
    }


class CLIError(Exception):
    """Custom exception for CLI errors"""
    pass






@click.group()
@click.option('--debug', is_flag=True, help='Enable debug mode')
@click.option('--config-file', help='Path to configuration file')
@click.pass_context
def cli(ctx, debug, config_file):
    """PRISM Platform CLI - Manage data ingestion and processing"""
    ctx.ensure_object(dict)
    ctx.obj['debug'] = debug
    ctx.obj['config_file'] = config_file
    
    if debug:
        console.print("[yellow]Debug mode enabled[/yellow]")


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
@click.option('--dataset', default='jarvis_dft_3d', help='JARVIS dataset to use')
@click.pass_context
def fetch_material(ctx, source, material_id, formula, elements, output, output_format, dataset):
    """Fetch material data from a specific source"""
    
    with console.status(f"[bold green]Fetching material from {source.upper()}..."):
        if source == 'jarvis':
            connector = JarvisConnector()
        elif source == 'nomad':
            connector = NOMADConnector(config=get_nomad_config())
        else:
            raise CLIError(f"Unknown source: {source}")
        
        # Build query parameters
        query_params = {}
        if material_id:
            query_params['material_id'] = material_id
        if formula:
            query_params['formula'] = formula
        if elements:
            query_params['elements'] = [e.strip() for e in elements.split(',')]
        if dataset:
            query_params['dataset'] = dataset
        
        if not query_params:
            raise CLIError("Must specify at least one search parameter")
        
        # Fetch the material
        try:
            # Use the correct method names from the connectors
            if material_id:
                result = asyncio.run(connector.get_material_by_id(material_id))
            else:
                asyncio.run(connector.connect())
                result = asyncio.run(connector.search_materials(**query_params))
            
            if not result:
                console.print("[yellow]No materials found matching criteria[/yellow]")
                return
            
            # Format output
            if output_format == 'json':

                class StandardizedMaterialEncoder(json.JSONEncoder):
                    def default(self, o):
                        if isinstance(o, (StandardizedMaterial, MaterialStructure, MaterialProperties, MaterialMetadata)):
                            return o.__dict__
                        return super().default(o)

                formatted_data = json.dumps(result, indent=2, cls=StandardizedMaterialEncoder)
            elif output_format == 'csv':
                # Convert to CSV format (simplified)
                import csv
                import io
                output_buffer = io.StringIO()
                if isinstance(result, list) and result:
                    fieldnames = result[0].keys()
                    writer = csv.DictWriter(output_buffer, fieldnames=fieldnames)
                    writer.writeheader()
                    writer.writerows(result)
                formatted_data = output_buffer.getvalue()
            elif output_format == 'yaml':
                import yaml
                formatted_data = yaml.dump(result, default_flow_style=False)
            
            # Output results
            if output:
                Path(output).write_text(formatted_data)
                console.print(f"[green]Results saved to {output}[/green]")
            else:
                console.print(formatted_data)
                
        except Exception as e:
            raise CLIError(f"Failed to fetch material: {e}")


@cli.command()
@click.option('--source', '-s', required=True,
              type=click.Choice(['jarvis', 'nomad', 'all']),
              help='Data source(s) for bulk fetch')
@click.option('--elements', '-e', help='Comma-separated list of elements')
@click.option('--limit', '-l', default=100, type=int,
              help='Maximum number of materials to fetch')
@click.option('--batch-size', '-b', default=10, type=int,
              help='Batch size for processing')
@click.option('--output-dir', '-o', help='Output directory for results')
@click.option('--dry-run', is_flag=True, help='Show what would be done without executing')
@click.pass_context

def bulk_fetch(ctx, source, elements, limit, batch_size, output_dir, dry_run):
    """Perform bulk material fetching with progress tracking"""
    
    sources = ['jarvis', 'nomad'] if source == 'all' else [source]
    
    if dry_run:
        console.print("[yellow]DRY RUN MODE - No actual fetching will be performed[/yellow]")
    
    # Setup output directory
    if output_dir:
        output_path = Path(output_dir)
        output_path.mkdir(parents=True, exist_ok=True)
    
    with Progress() as progress:
        for source_name in sources:
            task = progress.add_task(f"[green]Fetching from {source_name.upper()}", total=limit)
            
            if dry_run:
                # Simulate progress for dry run
                for i in range(limit):
                    progress.update(task, advance=1)
                    if i % 10 == 0:  # Update every 10 items
                        progress.refresh()
                continue
            
            # Get connector
            if source_name == 'jarvis':
                connector = JarvisConnector()
            else:
                connector = NOMADConnector(config=get_nomad_config())
            
            # Process in batches
            fetched_count = 0
            for batch_start in range(0, limit, batch_size):
                batch_end = min(batch_start + batch_size, limit)
                batch_params = {
                    'limit': batch_end - batch_start,
                    'offset': batch_start
                }
                
                if elements:
                    batch_params['elements'] = [e.strip() for e in elements.split(',')]
                
                try:
                    # Use the correct method for bulk fetching
                    batch_results = asyncio.run(connector.fetch_bulk(limit=batch_end-batch_start, offset=batch_start))
                    fetched_count += len(batch_results)
                    
                    # Save batch results if output directory specified
                    if output_dir and batch_results:
                        batch_file = output_path / f"{source_name}_batch_{batch_start}_{batch_end}.json"
                        batch_file.write_text(json.dumps(batch_results, indent=2))
                    
                    progress.update(task, advance=len(batch_results))
                    
                except Exception as e:
                    console.print(f"[red]Error in batch {batch_start}-{batch_end}: {e}[/red]")
                    progress.update(task, advance=batch_size)
            
            console.print(f"[green]Completed {source_name.upper()}: {fetched_count} materials fetched[/green]")


@cli.command()
@click.option('--format', 'output_format', default='table',
              type=click.Choice(['table', 'json', 'list']),
              help='Output format')
@click.option('--status', help='Filter by source status')

def list_sources(output_format, status):
    """List all available data sources and their status"""
    
    # Get database session
    db = next(get_db_session())
    
    try:
        sources = db.query(DataSource).all()
        
        if output_format == 'table':
            table = Table(title="Available Data Sources")
            table.add_column("ID", style="cyan")
            table.add_column("Name", style="magenta")
            table.add_column("Type", style="green")
            table.add_column("Status", style="yellow")
            table.add_column("Last Updated", style="blue")
            table.add_column("Description")
            
            for source in sources:
                if status and source.status != status:
                    continue
                    
                table.add_row(
                    str(source.id),
                    source.name,
                    source.source_type,
                    source.status,
                    source.last_updated.strftime("%Y-%m-%d %H:%M") if source.last_updated else "Never",
                    source.description or "No description"
                )
            
            console.print(table)
            
        elif output_format == 'json':
            sources_data = []
            for source in sources:
                if status and source.status != status:
                    continue
                sources_data.append({
                    'id': source.id,
                    'name': source.name,
                    'type': source.source_type,
                    'status': source.status,
                    'last_updated': source.last_updated.isoformat() if source.last_updated else None,
                    'description': source.description
                })
            console.print(json.dumps(sources_data, indent=2))
            
        elif output_format == 'list':
            for source in sources:
                if status and source.status != status:
                    continue
                console.print(f"• {source.name} ({source.source_type}) - {source.status}")
                
    finally:
        db.close()


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
                if source_name == 'jarvis':
                    connector = JarvisConnector()
                else:
                    connector = NOMADConnector(config=get_nomad_config())
                
                start_time = datetime.now()
                # Test basic connectivity
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
                    'message': 'Connection successful' if test_result else 'Connection failed'
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
        
        response_time = f"{result['response_time']:.2f}s" if result['response_time'] else "N/A"
        
        table.add_row(
            source_name.upper(),
            f"[{status_color}]{result['status'].upper()}[/{status_color}]",
            response_time,
            result['message']
        )
    
    console.print(table)


@cli.command()
@click.option('--refresh', '-r', is_flag=True, help='Refresh queue status')
@click.option('--watch', '-w', is_flag=True, help='Watch mode (continuous updates)')

def queue_status(refresh, watch):
    """Show job queue status and statistics"""
    
    def display_queue_status():
        db = next(get_db_session())
        try:
            # Get job statistics
            total_jobs = db.query(Job).count()
            pending_jobs = db.query(Job).filter(Job.status == JobStatus.PENDING).count()
            running_jobs = db.query(Job).filter(Job.status == JobStatus.RUNNING).count()
            completed_jobs = db.query(Job).filter(Job.status == JobStatus.COMPLETED).count()
            failed_jobs = db.query(Job).filter(Job.status == JobStatus.FAILED).count()
            
            # Recent jobs (last 24 hours)
            yesterday = datetime.now() - timedelta(days=1)
            recent_jobs = db.query(Job).filter(Job.created_at >= yesterday).count()
            
            # Create status table
            console.clear()
            
            # Summary panel
            summary_text = Text()
            summary_text.append(f"Total Jobs: {total_jobs}\n", style="bold")
            summary_text.append(f"Recent (24h): {recent_jobs}\n", style="cyan")
            summary_text.append(f"Queue Health: ", style="bold")
            
            if failed_jobs / max(total_jobs, 1) > 0.1:
                summary_text.append("WARNING", style="red bold")
            else:
                summary_text.append("GOOD", style="green bold")
            
            console.print(Panel(summary_text, title="Queue Overview", border_style="blue"))
            
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
            
            # Recent failures
            if failed_jobs > 0:
                recent_failures = db.query(Job).filter(
                    Job.status == JobStatus.FAILED,
                    Job.updated_at >= yesterday
                ).limit(5).all()
                
                if recent_failures:
                    failure_table = Table(title="Recent Failures (Last 24h)")
                    failure_table.add_column("Job ID", style="cyan")
                    failure_table.add_column("Source", style="magenta")
                    failure_table.add_column("Failed At", style="red")
                    failure_table.add_column("Error")
                    
                    for job in recent_failures:
                        failure_table.add_row(
                            str(job.id),
                            job.source_type,
                            job.updated_at.strftime("%H:%M:%S"),
                            job.error_message[:50] + "..." if job.error_message and len(job.error_message) > 50 else job.error_message or "Unknown error"
                        )
                    
                    console.print(failure_table)
            
        finally:
            db.close()
    
    if watch:
        try:
            while True:
                display_queue_status()
                console.print("\n[dim]Press Ctrl+C to exit watch mode[/dim]")
                import time
                time.sleep(5)
        except KeyboardInterrupt:
            console.print("\n[yellow]Exiting watch mode[/yellow]")
    else:
        display_queue_status()


@cli.command()
@click.option('--max-age', '-a', default=24, type=int,
              help='Maximum age of failed jobs to retry (hours)')
@click.option('--source', '-s', 
              type=click.Choice(['jarvis', 'nomad', 'all']),
              help='Source type to filter')
@click.option('--dry-run', is_flag=True, help='Show what would be retried without executing')
@click.option('--batch-size', '-b', default=10, type=int,
              help='Number of jobs to retry in each batch')

def retry_failed_jobs(max_age, source, dry_run, batch_size):
    """Retry failed jobs with filtering options"""
    
    db = next(get_db_session())
    try:
        # Build query for failed jobs
        cutoff_time = datetime.now() - timedelta(hours=max_age)
        query = db.query(Job).filter(
            Job.status == JobStatus.FAILED,
            Job.updated_at >= cutoff_time
        )
        
        if source and source != 'all':
            query = query.filter(Job.source_type == source)
        
        failed_jobs = query.all()
        
        if not failed_jobs:
            console.print("[yellow]No failed jobs found matching criteria[/yellow]")
            return
        
        console.print(f"Found {len(failed_jobs)} failed jobs to retry")
        
        if dry_run:
            table = Table(title="Jobs to Retry (Dry Run)")
            table.add_column("Job ID", style="cyan")
            table.add_column("Source", style="magenta")
            table.add_column("Failed At", style="red")
            table.add_column("Error")
            
            for job in failed_jobs[:20]:  # Show first 20
                table.add_row(
                    str(job.id),
                    job.source_type,
                    job.updated_at.strftime("%Y-%m-%d %H:%M"),
                    job.error_message[:50] + "..." if job.error_message and len(job.error_message) > 50 else job.error_message or "Unknown"
                )
            
            console.print(table)
            if len(failed_jobs) > 20:
                console.print(f"... and {len(failed_jobs) - 20} more jobs")
            return
        
        # Confirm retry
        if not Confirm.ask(f"Retry {len(failed_jobs)} failed jobs?"):
            console.print("[yellow]Operation cancelled[/yellow]")
            return
        
        # Process retries in batches
        processor = JobProcessor()
        retry_count = 0
        
        with Progress() as progress:
            task = progress.add_task("[green]Retrying jobs...", total=len(failed_jobs))
            
            for i in range(0, len(failed_jobs), batch_size):
                batch = failed_jobs[i:i+batch_size]
                
                for job in batch:
                    try:
                        # Reset job status and retry
                        job.status = JobStatus.PENDING
                        job.error_message = None
                        job.retry_count = (job.retry_count or 0) + 1
                        job.updated_at = datetime.now()
                        
                        db.commit()
                        retry_count += 1
                        
                    except Exception as e:
                        console.print(f"[red]Failed to retry job {job.id}: {e}[/red]")
                    
                    progress.update(task, advance=1)
        
        console.print(f"[green]Successfully queued {retry_count} jobs for retry[/green]")
        
    finally:
        db.close()


@cli.command()
@click.option('--format', 'output_format', default='json',
              type=click.Choice(['json', 'csv', 'xlsx', 'parquet']),
              help='Export format')
@click.option('--output', '-o', required=True, help='Output file path')
@click.option('--source', '-s',
              type=click.Choice(['jarvis', 'nomad', 'all']),
              help='Filter by data source')
@click.option('--date-from', help='Start date (YYYY-MM-DD)')
@click.option('--date-to', help='End date (YYYY-MM-DD)')
@click.option('--status', 
              type=click.Choice(['pending', 'running', 'completed', 'failed']),
              help='Filter by job status')
@click.option('--limit', '-l', type=int, help='Maximum number of records')

def export_data(output_format, output, source, date_from, date_to, status, limit):
    """Export data to various formats"""
    
    db = next(get_db_session())
    try:
        # Build query
        query = db.query(Job)
        
        if source and source != 'all':
            query = query.filter(Job.source_type == source)
        
        if status:
            status_enum = getattr(JobStatus, status.upper())
            query = query.filter(Job.status == status_enum)
        
        if date_from:
            from_date = datetime.strptime(date_from, '%Y-%m-%d')
            query = query.filter(Job.created_at >= from_date)
        
        if date_to:
            to_date = datetime.strptime(date_to, '%Y-%m-%d')
            query = query.filter(Job.created_at <= to_date)
        
        if limit:
            query = query.limit(limit)
        
        jobs = query.all()
        
        if not jobs:
            console.print("[yellow]No data found matching criteria[/yellow]")
            return
        
        console.print(f"Exporting {len(jobs)} records...")
        
        # Convert to exportable format
        data = []
        for job in jobs:
            data.append({
                'id': job.id,
                'source_type': job.source_type,
                'status': job.status.value,
                'created_at': job.created_at.isoformat(),
                'updated_at': job.updated_at.isoformat() if job.updated_at else None,
                'completed_at': job.completed_at.isoformat() if job.completed_at else None,
                'retry_count': job.retry_count or 0,
                'error_message': job.error_message,
                'job_metadata': job.job_metadata
            })
        
        # Export based on format
        output_path = Path(output)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        
        if output_format == 'json':
            output_path.write_text(json.dumps(data, indent=2))
        
        elif output_format == 'csv':
            import csv
            with output_path.open('w', newline='') as csvfile:
                if data:
                    fieldnames = data[0].keys()
                    writer = csv.DictWriter(csvfile, fieldnames=fieldnames)
                    writer.writeheader()
                    writer.writerows(data)
        
        elif output_format == 'xlsx':
            try:
                import pandas as pd
                df = pd.DataFrame(data)
                df.to_excel(output_path, index=False)
            except ImportError:
                raise CLIError("pandas and openpyxl required for Excel export. Install with: pip install pandas openpyxl")
        
        elif output_format == 'parquet':
            try:
                import pandas as pd
                df = pd.DataFrame(data)
                df.to_parquet(output_path, index=False)
            except ImportError:
                raise CLIError("pandas and pyarrow required for Parquet export. Install with: pip install pandas pyarrow")
        
        console.print(f"[green]Data exported to {output_path}[/green]")
        console.print(f"[dim]Format: {output_format.upper()}, Records: {len(data)}[/dim]")
        
    finally:
        db.close()


@cli.command()
@click.option('--interval', '-i', default=5, type=int,
              help='Update interval in seconds')
@click.option('--duration', '-d', type=int,
              help='Monitoring duration in seconds (default: infinite)')

def monitor(interval, duration):
    """Monitor system performance and metrics"""
    
    start_time = datetime.now()
    
    try:
        while True:
            console.clear()
            
            # System info
            current_time = datetime.now()
            uptime = current_time - start_time
            
            console.print(Panel(
                f"PRISM System Monitor\n"
                f"Started: {start_time.strftime('%Y-%m-%d %H:%M:%S')}\n"
                f"Uptime: {uptime}\n"
                f"Update Interval: {interval}s",
                title="System Status",
                border_style="green"
            ))
            
            # Get metrics
            db = next(get_db_session())
            try:
                # Job metrics
                total_jobs = db.query(Job).count()
                recent_jobs = db.query(Job).filter(
                    Job.created_at >= datetime.now() - timedelta(minutes=5)
                ).count()
                
                active_jobs = db.query(Job).filter(
                    Job.status.in_([JobStatus.PENDING, JobStatus.RUNNING])
                ).count()
                
                # Performance metrics
                metrics_table = Table(title="Performance Metrics")
                metrics_table.add_column("Metric", style="cyan")
                metrics_table.add_column("Value", style="green")
                metrics_table.add_column("Status", style="yellow")
                
                metrics_table.add_row(
                    "Total Jobs",
                    str(total_jobs),
                    "Normal" if total_jobs < 10000 else "High"
                )
                
                metrics_table.add_row(
                    "Recent Jobs (5m)",
                    str(recent_jobs),
                    "Normal" if recent_jobs < 100 else "High"
                )
                
                metrics_table.add_row(
                    "Active Jobs",
                    str(active_jobs),
                    "Normal" if active_jobs < 50 else "High"
                )
                
                console.print(metrics_table)
                
            finally:
                db.close()
            
            # Check duration
            if duration and (current_time - start_time).total_seconds() >= duration:
                break
            
            console.print(f"\n[dim]Press Ctrl+C to exit monitoring[/dim]")
            
            import time
            time.sleep(interval)
            
    except KeyboardInterrupt:
        console.print("\n[yellow]Monitoring stopped[/yellow]")


@cli.command()
@click.option('--source', '-s', default='nomad',
              type=click.Choice(['nomad']),
              help='Data source for material fetching')
@click.option('--elements', '-e', help='Comma-separated list of elements (e.g., "Si,O")')
@click.option('--formula', '-f', help='Chemical formula to search for')
@click.option('--max-results', '-m', type=int, help='Maximum number of materials to fetch')
@click.option('--batch-size', '-b', default=50, type=int,
              help='Batch size for processing (default: 50)')
@click.option('--show-progress', is_flag=True, default=True,
              help='Show progress during fetching')
@click.option('--database-only', is_flag=True,
              help='Search only in local database, don\'t fetch from remote')
@click.option('--stats', is_flag=True,
              help='Show database statistics')
def fetch_and_store(source, elements, formula, max_results, batch_size, 
                   show_progress, database_only, stats):
    """
    Fetch materials from remote sources and store in local database with progress tracking.
    
    This command provides a controlled way to fetch materials from external databases
    like NOMAD and store them locally with proper progress tracking and batch processing.
    
    Examples:
        prism fetch-and-store --elements Si --max-results 100
        prism fetch-and-store --formula SiO2 --max-results 50
        prism fetch-and-store --database-only --elements Si  # Search local only
        prism fetch-and-store --stats  # Show database statistics
    """
    console = Console()
    
    try:
        # Initialize database
        init_db_sync()
        
        if stats:
            # Show database statistics
            from app.services.materials_service import MaterialsService
            materials_service = MaterialsService()
            db_stats = materials_service.get_statistics()
            
            console.print(Panel("[bold blue]Database Statistics[/bold blue]"))
            
            stats_table = Table(title="Materials Database Stats")
            stats_table.add_column("Metric", style="cyan")
            stats_table.add_column("Count", style="green")
            
            stats_table.add_row("Total Materials", str(db_stats.get("total_materials", 0)))
            
            # Origins
            for origin, count in db_stats.get("by_origin", {}).items():
                stats_table.add_row(f"  └─ From {origin}", str(count))
            
            # Crystal systems
            console.print("\n[bold]By Crystal System:[/bold]")
            for crystal_system, count in db_stats.get("by_crystal_system", {}).items():
                if crystal_system:
                    stats_table.add_row(f"  └─ {crystal_system}", str(count))
            
            console.print(stats_table)
            return
        
        if database_only:
            # Search local database only
            from app.services.materials_service import MaterialsService
            materials_service = MaterialsService()
            
            search_params = {}
            if elements:
                search_params["elements"] = [e.strip() for e in elements.split(",")]
            if formula:
                search_params["formula"] = formula
            
            materials, total = materials_service.search_materials(**search_params)
            
            console.print(f"[green]Found {len(materials)} materials in local database[/green]")
            
            if materials:
                table = Table(title="Local Materials")
                table.add_column("ID", style="cyan")
                table.add_column("Formula", style="green")
                table.add_column("Origin", style="yellow")
                table.add_column("Elements", style="blue")
                
                for material in materials[:10]:  # Show first 10
                    elements_str = ", ".join(material.elements) if material.elements else "N/A"
                    table.add_row(
                        material.material_id[:12] + "...",
                        material.reduced_formula,
                        material.origin,
                        elements_str
                    )
                
                if len(materials) > 10:
                    table.add_row("...", "...", "...", f"... and {len(materials) - 10} more")
                
                console.print(table)
            return
        
        # Build query parameters
        query_params = {}
        if elements:
            query_params["elements"] = elements.replace(",", " ").strip()
        if formula:
            query_params["formula"] = formula
        
        if not query_params:
            console.print("[red]Error: Please specify --elements or --formula[/red]")
            return
        
        # Create progress callback if requested
        progress_callback = None
        if show_progress:
            progress_callback = create_progress_printer()
        
        # Initialize enhanced connector
        config = get_nomad_config()
        config["batch_size"] = batch_size
        
        enhanced_connector = EnhancedNOMADConnector(config, auto_store=True)
        
        with console.status("[bold green]Connecting to NOMAD API..."):
            success = asyncio.run(enhanced_connector.connect())
            if not success:
                console.print("[red]Failed to connect to NOMAD API[/red]")
                return
        
        console.print("[green]✅ Connected to NOMAD API[/green]")
        
        # Start the search and store operation
        console.print(f"[blue]Searching for materials with: {query_params}[/blue]")
        if max_results:
            console.print(f"[blue]Maximum results: {max_results}[/blue]")
        
        # Run the enhanced search with database storage
        stats = asyncio.run(enhanced_connector.search_and_store_materials(
            query_params=query_params,
            max_results=max_results,
            progress_callback=progress_callback
        ))
        
        # Display final results
        console.print(Panel("[bold green]Operation Complete![/bold green]"))
        
        results_table = Table(title="Final Results")
        results_table.add_column("Metric", style="cyan")
        results_table.add_column("Count", style="green")
        
        results_table.add_row("Total Available", str(stats["total_available"]))
        results_table.add_row("Total Fetched", str(stats["total_fetched"]))
        results_table.add_row("Total Stored", str(stats["total_stored"]))
        results_table.add_row("Total Updated", str(stats["total_updated"]))
        results_table.add_row("Total Errors", str(stats["total_errors"]))
        results_table.add_row("Batches Processed", str(stats["batches_processed"]))
        
        console.print(results_table)
        
        # Show updated database stats
        db_stats = enhanced_connector.get_database_statistics()
        console.print(f"[blue]Local database now contains {db_stats['total_materials']} total materials[/blue]")
        
        # Disconnect
        asyncio.run(enhanced_connector.disconnect())
        
    except KeyboardInterrupt:
        console.print("\n[yellow]Operation cancelled by user[/yellow]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/red]")
        import traceback
        if console.is_terminal:
            traceback.print_exc()


@cli.command()
@click.option('--source', '-s', default='nomad',
              type=click.Choice(['nomad']),
              help='Data source for material fetching')
@click.option('--elements', '-e', help='Comma-separated list of elements (e.g., "Si,O")')
@click.option('--formula', '-f', help='Chemical formula to search for')
@click.option('--max-results', '-m', type=int, help='Maximum number of materials to fetch')
@click.option('--batch-size', '-b', default=50, type=int,
              help='Batch size for processing (default: 50)')
@click.option('--show-progress', is_flag=True, default=True,
              help='Show progress during fetching')
@click.option('--database-only', is_flag=True,
              help='Search only in local database, don\'t fetch from remote')
@click.option('--stats', is_flag=True,
              help='Show database statistics')
def fetch_and_store(source, elements, formula, max_results, batch_size, 
                   show_progress, database_only, stats):
    """
    Fetch materials from remote sources and store in PostgreSQL database with progress tracking.
    
    This command provides a controlled way to fetch materials from external databases
    like NOMAD and store them locally with proper progress tracking and batch processing.
    
    Examples:
        prism fetch-and-store --elements Si --max-results 100
        prism fetch-and-store --formula SiO2 --max-results 50
        prism fetch-and-store --database-only --elements Si  # Search local only
        prism fetch-and-store --stats  # Show database statistics
    """
    console = Console()
    
    try:
        # Initialize database
        init_db_sync()
        
        if stats:
            # Show database statistics
            from app.services.materials_service import MaterialsService
            materials_service = MaterialsService()
            db_stats = materials_service.get_statistics()
            
            console.print(Panel("[bold blue]PostgreSQL Database Statistics[/bold blue]"))
            
            stats_table = Table(title="Materials Database Stats")
            stats_table.add_column("Metric", style="cyan")
            stats_table.add_column("Count", style="green")
            
            stats_table.add_row("Total Materials", str(db_stats.get("total_materials", 0)))
            
            # Origins
            for origin, count in db_stats.get("by_origin", {}).items():
                stats_table.add_row(f"  └─ From {origin}", str(count))
            
            # Crystal systems
            console.print("\n[bold]By Crystal System:[/bold]")
            for crystal_system, count in db_stats.get("by_crystal_system", {}).items():
                if crystal_system:
                    stats_table.add_row(f"  └─ {crystal_system}", str(count))
            
            console.print(stats_table)
            return
        
        if database_only:
            # Search local database only
            from app.services.materials_service import MaterialsService
            materials_service = MaterialsService()
            
            search_params = {}
            if elements:
                search_params["elements"] = [e.strip() for e in elements.split(",")]
            if formula:
                search_params["formula"] = formula
            
            materials, total = materials_service.search_materials(**search_params)
            
            console.print(f"[green]Found {len(materials)} materials in local PostgreSQL database[/green]")
            
            if materials:
                table = Table(title="Local Materials")
                table.add_column("ID", style="cyan")
                table.add_column("Formula", style="green")
                table.add_column("Origin", style="yellow")
                table.add_column("Elements", style="blue")
                
                for material in materials[:10]:  # Show first 10
                    elements_str = ", ".join(material.elements) if material.elements else "N/A"
                    table.add_row(
                        material.material_id[:12] + "...",
                        material.reduced_formula,
                        material.origin,
                        elements_str
                    )
                
                if len(materials) > 10:
                    table.add_row("...", "...", "...", f"... and {len(materials) - 10} more")
                
                console.print(table)
            return
        
        # Build query parameters
        query_params = {}
        if elements:
            query_params["elements"] = elements.replace(",", " ").strip()
        if formula:
            query_params["formula"] = formula
        
        if not query_params:
            console.print("[red]Error: Please specify --elements or --formula[/red]")
            return
        
        # Create progress callback if requested
        progress_callback = None
        if show_progress:
            progress_callback = create_progress_printer()
        
        # Initialize enhanced connector
        config = get_nomad_config()
        config["batch_size"] = batch_size
        
        enhanced_connector = EnhancedNOMADConnector(config, auto_store=True)
        
        with console.status("[bold green]Connecting to NOMAD API..."):
            success = asyncio.run(enhanced_connector.connect())
            if not success:
                console.print("[red]Failed to connect to NOMAD API[/red]")
                return
        
        console.print("[green]✅ Connected to NOMAD API[/green]")
        
        # Start the search and store operation
        console.print(f"[blue]Searching for materials with: {query_params}[/blue]")
        if max_results:
            console.print(f"[blue]Maximum results: {max_results}[/blue]")
        
        # Run the enhanced search with database storage
        stats = asyncio.run(enhanced_connector.search_and_store_materials(
            query_params=query_params,
            max_results=max_results,
            progress_callback=progress_callback
        ))
        
        # Display final results
        console.print(Panel("[bold green]Operation Complete![/bold green]"))
        
        results_table = Table(title="Final Results")
        results_table.add_column("Metric", style="cyan")
        results_table.add_column("Count", style="green")
        
        results_table.add_row("Total Available", str(stats["total_available"]))
        results_table.add_row("Total Fetched", str(stats["total_fetched"]))
        results_table.add_row("Total Stored", str(stats["total_stored"]))
        results_table.add_row("Total Updated", str(stats["total_updated"]))
        results_table.add_row("Total Errors", str(stats["total_errors"]))
        results_table.add_row("Batches Processed", str(stats["batches_processed"]))
        
        console.print(results_table)
        
        # Show updated database stats
        db_stats = enhanced_connector.get_database_statistics()
        console.print(f"[blue]PostgreSQL database now contains {db_stats['total_materials']} total materials[/blue]")
        
        # Disconnect
        asyncio.run(enhanced_connector.disconnect())
        
    except KeyboardInterrupt:
        console.print("\n[yellow]Operation cancelled by user[/yellow]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/red]")
        import traceback
        if console.is_terminal:
            traceback.print_exc()


@cli.command()
@click.option('--list', 'list_config', is_flag=True, help='List current configuration')
@click.option('--set', 'set_value', help='Set configuration value (key=value)')
@click.option('--get', 'get_key', help='Get configuration value')

def config(list_config, set_value, get_key):
    """Manage configuration settings"""
    
    config_data = {
        'database_url': settings.DATABASE_URL,
        'jarvis_base_url': getattr(settings, 'JARVIS_BASE_URL', 'Not set'),
        'nomad_base_url': getattr(settings, 'NOMAD_BASE_URL', 'Not set'),
        'rate_limit_requests': getattr(settings, 'RATE_LIMIT_REQUESTS', 100),
        'rate_limit_period': getattr(settings, 'RATE_LIMIT_PERIOD', 60),
        'job_retry_limit': getattr(settings, 'JOB_RETRY_LIMIT', 3),
        'debug_mode': getattr(settings, 'DEBUG', False)
    }
    
    if list_config:
        table = Table(title="Current Configuration")
        table.add_column("Setting", style="cyan")
        table.add_column("Value", style="green")
        table.add_column("Type", style="yellow")
        
        for key, value in config_data.items():
            table.add_row(
                key,
                str(value)[:50] + "..." if len(str(value)) > 50 else str(value),
                type(value).__name__
            )
        
        console.print(table)
    
    elif get_key:
        if get_key in config_data:
            console.print(f"{get_key}: {config_data[get_key]}")
        else:
            console.print(f"[red]Configuration key '{get_key}' not found[/red]")
    
    elif set_value:
        try:
            key, value = set_value.split('=', 1)
            console.print(f"[yellow]Note: Configuration changes require application restart[/yellow]")
            console.print(f"Would set {key} = {value}")
        except ValueError:
            console.print("[red]Invalid format. Use key=value[/red]")
    
    else:
        console.print("Use --list to show configuration, --get KEY to get a value, or --set KEY=VALUE to set a value")


@cli.command()
@click.option('--database', type=click.Choice(['nomad', 'jarvis', 'oqmd', 'cod', 'all']), default='all',
              help='Database to search')
@click.option('--elements', help='Elements to search for (comma-separated, e.g., Si,O)')
@click.option('--formula', help='Specific chemical formula')
@click.option('--formation-energy-max', type=float, help='Maximum formation energy (eV/atom)')
@click.option('--band-gap-min', type=float, help='Minimum band gap (eV)')
@click.option('--band-gap-max', type=float, help='Maximum band gap (eV)')
@click.option('--stability-max', type=float, help='Maximum hull distance (eV/atom, OQMD only)')
@click.option('--space-group', help='Crystal space group')
@click.option('--min-elements', type=int, help='Minimum number of elements (for HEAs)')
@click.option('--max-elements', type=int, help='Maximum number of elements')
@click.option('--limit', default=20, help='Maximum number of results')
@click.option('--export', type=click.Choice(['csv', 'json', 'both']), help='Export results to file')
@click.option('--plot', is_flag=True, help='Generate visualization plots')
@click.option('--interactive', is_flag=True, help='Interactive search mode')
def search(database, elements, formula, formation_energy_max, band_gap_min, band_gap_max,
           stability_max, space_group, min_elements, max_elements, limit, export, plot, interactive):
    """
    Search for materials across databases with advanced filtering.
    
    Examples:
        # Search for Silicon materials in all databases
        ./prism search --elements Si --limit 10
        
        # Search for stable materials in OQMD
        ./prism search --database oqmd --stability-max 0.1 --limit 5
        
        # Search for High Entropy Alloys (4+ elements)
        ./prism search --database cod --min-elements 4 --limit 10
        
        # Search semiconductors with specific band gap
        ./prism search --band-gap-min 1.0 --band-gap-max 3.0 --export csv
        
        # Interactive search mode
        ./prism search --interactive
    """
    console = Console()
    
    if interactive:
        console.print("🔬 [bold cyan]PRISM Interactive Materials Search[/bold cyan]")
        console.print("=" * 50)
        
        # Interactive prompts
        if not database or database == 'all':
            db_choices = ['nomad', 'jarvis', 'oqmd', 'cod', 'all']
            database = Prompt.ask("Select database", choices=db_choices, default='all')
        
        if not elements:
            elements = Prompt.ask("Enter elements (comma-separated, or Enter to skip)", default="")
            elements = elements if elements else None
        
        if not limit:
            limit = int(Prompt.ask("Maximum results", default="20"))
        
        console.print(f"\n🔍 Searching {database} database(s)...")
    
    try:
        # Create connectors based on selection
        connectors = {}
        
        if database == 'all' or database == 'nomad':
            connectors['nomad'] = NOMADConnector({
                'base_url': 'https://nomad-lab.eu/prod/rae/api/v1',
                'timeout': 30.0
            })
        
        if database == 'all' or database == 'jarvis':
            connectors['jarvis'] = JarvisConnector({
                'base_url': 'https://jarvis-materials.org/api/v1',
                'timeout': 30.0
            })
        
        if database == 'all' or database == 'oqmd':
            connectors['oqmd'] = OQMDConnector({
                'base_url': 'http://oqmd.org/oqmdapi',
                'timeout': 30.0
            })
        
        if database == 'all' or database == 'cod':
            connectors['cod'] = CODConnector({
                'base_url': 'https://www.crystallography.net/cod',
                'timeout': 30.0
            })
        
        # Search parameters
        search_params = {
            'limit': limit
        }
        
        if elements:
            search_params['elements'] = elements
        if formula:
            search_params['formula'] = formula
        if formation_energy_max:
            search_params['formation_energy_max'] = formation_energy_max
        if band_gap_min:
            search_params['band_gap_min'] = band_gap_min
        if band_gap_max:
            search_params['band_gap_max'] = band_gap_max
        if stability_max:
            search_params['stability_max'] = stability_max
        if space_group:
            search_params['space_group'] = space_group
        if min_elements:
            search_params['strictmin'] = min_elements
        if max_elements:
            search_params['strictmax'] = max_elements
        
        # Perform searches
        all_materials = []
        
        async def run_searches():
            for db_name, connector in connectors.items():
                try:
                    console.print(f"🔍 Searching {db_name.upper()}...")
                    
                    await connector.connect()
                    materials = await connector.search_materials(**search_params)
                    
                    console.print(f"✅ Found {len(materials)} materials in {db_name.upper()}")
                    all_materials.extend(materials)
                    
                    await connector.disconnect()
                    
                except Exception as e:
                    console.print(f"❌ Error searching {db_name}: {e}")
        
        # Run the search
        asyncio.run(run_searches())
        
        if not all_materials:
            console.print("❌ No materials found matching the criteria")
            return
        
        console.print(f"\n📊 Total materials found: {len(all_materials)}")
        
        # Display results using data viewer
        viewer = MaterialsDataViewer()
        viewer.display_summary_table(all_materials, max_rows=min(limit, 20))
        
        # Export if requested
        if export:
            timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
            
            if export in ['csv', 'both']:
                csv_file = f"materials_search_{timestamp}.csv"
                viewer.export_to_csv(all_materials, csv_file)
            
            if export in ['json', 'both']:
                json_file = f"materials_search_{timestamp}.json"
                viewer.export_to_json(all_materials, json_file)
        
        # Generate plots if requested
        if plot:
            try:
                import matplotlib
                matplotlib.use('Agg')  # Use non-interactive backend
                
                plot_dir = f"plots_{datetime.now().strftime('%Y%m%d_%H%M%S')}"
                console.print(f"📈 Generating plots in {plot_dir}/...")
                
                viewer.generate_report(all_materials, plot_dir)
                
            except ImportError:
                console.print("⚠️  Plotting requires matplotlib. Install with: pip install matplotlib seaborn")
            except Exception as e:
                console.print(f"❌ Error generating plots: {e}")
        
    except Exception as e:
        console.print(f"❌ Search failed: {e}")
        raise click.ClickException(f"Search failed: {e}")


@cli.command()
@click.option('--database', required=True, type=click.Choice(['nomad', 'jarvis', 'oqmd', 'cod']),
              help='Database to test')
def test_database(database):
    """
    Test connection to specific databases.
    
    Examples:
        # Test OQMD connection
        ./prism test-database --database oqmd
        
        # Test COD connection
        ./prism test-database --database cod
    """
    console = Console()
    console.print(f"🔍 Testing connection to {database.upper()}...")
    
    try:
        # Create connector
        if database == 'nomad':
            connector = NOMADConnector({
                'base_url': 'https://nomad-lab.eu/prod/rae/api/v1',
                'timeout': 30.0
            })
        elif database == 'jarvis':
            connector = JarvisConnector({
                'base_url': 'https://jarvis-materials.org/api/v1',
                'timeout': 30.0
            })
        elif database == 'oqmd':
            connector = OQMDConnector({
                'base_url': 'http://oqmd.org/oqmdapi',
                'timeout': 30.0
            })
        elif database == 'cod':
            connector = CODConnector({
                'base_url': 'https://www.crystallography.net/cod',
                'timeout': 30.0
            })
        
        async def test_connection():
            # Test connection
            connected = await connector.connect()
            if not connected:
                raise Exception("Failed to connect")
            
            # Test health check
            healthy = await connector.health_check()
            if not healthy:
                raise Exception("Health check failed")
            
            # Test small search
            materials = await connector.search_materials(limit=1)
            
            await connector.disconnect()
            
            return len(materials)
        
        # Run test
        result = asyncio.run(test_connection())
        
        console.print(f"✅ {database.upper()} connection successful!")
        console.print(f"📊 Retrieved {result} test material(s)")
        
        # Get database info
        if hasattr(connector, 'get_database_info'):
            info = asyncio.run(connector.get_database_info())
            
            table = Table(title=f"{database.upper()} Database Information")
            table.add_column("Property", style="cyan")
            table.add_column("Value", style="green")
            
            for key, value in info.items():
                if isinstance(value, list):
                    value = ", ".join(str(v) for v in value)
                table.add_row(key, str(value))
            
            console.print(table)
        
    except Exception as e:
        console.print(f"❌ Connection to {database.upper()} failed: {e}")
        raise click.ClickException(f"Database test failed: {e}")


@cli.command()
@click.option('--input-file', required=True, help='CSV file with materials data')
@click.option('--database-column', default='database', help='Column name for database source')
@click.option('--id-column', default='id', help='Column name for material ID')
@click.option('--output-dir', default='materials_export', help='Output directory')
def export_from_csv(input_file, database_column, id_column, output_dir):
    """
    Export materials data from CSV file with detailed information.
    
    Examples:
        # Export from search results
        ./prism export-from-csv --input-file search_results.csv
        
        # Custom column mapping
        ./prism export-from-csv --input-file data.csv --database-column source --id-column material_id
    """
    console = Console()
    
    try:
        import pandas as pd
        
        # Read CSV
        console.print(f"📁 Reading CSV file: {input_file}")
        df = pd.read_csv(input_file)
        
        console.print(f"📊 Found {len(df)} materials in CSV")
        
        # Group by database
        if database_column not in df.columns:
            raise ValueError(f"Column '{database_column}' not found in CSV")
        
        if id_column not in df.columns:
            raise ValueError(f"Column '{id_column}' not found in CSV")
        
        grouped = df.groupby(database_column)
        
        # Create connectors
        connectors = {
            'nomad': NOMADConnector({'base_url': 'https://nomad-lab.eu/prod/rae/api/v1', 'timeout': 30.0}),
            'jarvis': JarvisConnector({'base_url': 'https://jarvis-materials.org/api/v1', 'timeout': 30.0}),
            'oqmd': OQMDConnector({'base_url': 'http://oqmd.org/oqmdapi', 'timeout': 30.0}),
            'cod': CODConnector({'base_url': 'https://www.crystallography.net/cod', 'timeout': 30.0})
        }
        
        async def fetch_detailed_data():
            all_materials = []
            
            for database, group in grouped:
                db_name = database.lower()
                
                if db_name not in connectors:
                    console.print(f"⚠️  Unknown database: {database}")
                    continue
                
                connector = connectors[db_name]
                console.print(f"🔍 Fetching detailed data from {database.upper()}...")
                
                try:
                    await connector.connect()
                    
                    for _, row in group.iterrows():
                        material_id = str(row[id_column])
                        
                        try:
                            material = await connector.get_material_by_id(material_id)
                            if material:
                                all_materials.append(material)
                        except Exception as e:
                            console.print(f"⚠️  Failed to fetch {material_id}: {e}")
                    
                    await connector.disconnect()
                    
                except Exception as e:
                    console.print(f"❌ Error with {database}: {e}")
            
            return all_materials
        
        # Fetch data
        materials = asyncio.run(fetch_detailed_data())
        
        if materials:
            console.print(f"✅ Retrieved {len(materials)} detailed materials")
            
            # Generate comprehensive report
            viewer = MaterialsDataViewer()
            viewer.generate_report(materials, output_dir)
            
            console.print(f"📁 Detailed export saved to: {output_dir}")
        else:
            console.print("❌ No materials could be retrieved")
        
    except ImportError:
        console.print("❌ pandas is required for CSV processing. Install with: pip install pandas")
    except Exception as e:
        console.print(f"❌ Export failed: {e}")
        raise click.ClickException(f"Export failed: {e}")


@cli.command()
@click.argument('config_file', type=click.Path(exists=True))
def add_custom_database(config_file):
    """
    Add a custom database connector from configuration file.
    
    The config file should be a JSON file with the following structure:
    {
        "name": "CustomDB",
        "base_url": "https://api.example.com",
        "api_key": "optional",
        "timeout": 30.0,
        "endpoints": {
            "search": "/search",
            "detail": "/material/{id}"
        },
        "field_mappings": {
            "id": "material_id",
            "formula": "chemical_formula",
            "formation_energy": "formation_energy_per_atom"
        }
    }
    
    Examples:
        # Add custom database
        ./prism add-custom-database my_custom_db.json
    """
    console = Console()
    
    try:
        with open(config_file, 'r') as f:
            config = json.load(f)
        
        required_fields = ['name', 'base_url']
        for field in required_fields:
            if field not in config:
                raise ValueError(f"Required field '{field}' missing from config")
        
        console.print(f"📝 Custom database configuration:")
        console.print(f"   Name: {config['name']}")
        console.print(f"   URL: {config['base_url']}")
        
        # Here you would implement the actual database registration
        # For now, just show what would be done
        console.print("✅ Custom database configuration validated")
        console.print("💡 Custom database integration requires development of connector class")
        console.print("   See app/services/connectors/ for examples")
        
    except Exception as e:
        console.print(f"❌ Failed to add custom database: {e}")
        raise click.ClickException(f"Custom database setup failed: {e}")


@cli.command()
def examples():
    """
    Show comprehensive usage examples for PRISM CLI.
    """
    console = Console()
    
    examples_text = """
[bold cyan]🔬 PRISM CLI Usage Examples[/bold cyan]

[bold yellow]1. Basic Material Search[/bold yellow]
   # Search for Silicon materials
   ./prism search --elements Si --limit 10
   
   # Search specific formula
   ./prism search --formula SiO2 --database nomad
   
   # Search by band gap range
   ./prism search --band-gap-min 1.0 --band-gap-max 3.0 --limit 20

[bold yellow]2. Advanced Filtering[/bold yellow]
   # Stable materials only (OQMD)
   ./prism search --database oqmd --stability-max 0.1 --elements Si,O
   
   # High Entropy Alloys (4+ elements)
   ./prism search --database cod --min-elements 4 --max-elements 8
   
   # Semiconductors with specific properties
   ./prism search --formation-energy-max -1.0 --band-gap-min 0.5 --export csv

[bold yellow]3. Database-Specific Searches[/bold yellow]
   # NOMAD: DFT calculations
   ./prism search --database nomad --elements Ti,Al --limit 15
   
   # JARVIS: NIST materials
   ./prism search --database jarvis --space-group "Fm-3m" --limit 10
   
   # OQMD: Formation energies
   ./prism search --database oqmd --formation-energy-max -2.0 --limit 5
   
   # COD: Crystal structures
   ./prism search --database cod --elements Nb,Mo,Ta,W --min-elements 4

[bold yellow]4. Data Export and Visualization[/bold yellow]
   # Export to CSV
   ./prism search --elements Fe,Ni,Cr --export csv --limit 50
   
   # Export to JSON with metadata
   ./prism search --database nomad --elements Li --export json
   
   # Generate plots and comprehensive report
   ./prism search --elements Si --plot --export both --limit 100

[bold yellow]5. Interactive Mode[/bold yellow]
   # Interactive search with prompts
   ./prism search --interactive
   
   # Test database connections
   ./prism test-database --database oqmd
   ./prism test-database --database cod

[bold yellow]6. Data Management[/bold yellow]
   # Export detailed data from CSV
   ./prism export-from-csv --input-file search_results.csv
   
   # Add custom database
   ./prism add-custom-database my_custom_db.json

[bold yellow]7. Bulk Operations[/bold yellow]
   # Fetch materials with progress tracking
   ./prism bulk-fetch --source nomad --elements Si --limit 1000 --store-db
   
   # Enhanced NOMAD with database storage
   ./prism bulk-fetch --source enhanced-nomad --elements Fe,Ni --limit 500

[bold yellow]8. System Operations[/bold yellow]
   # Check system status
   ./prism queue-status
   
   # List available sources
   ./prism list-sources
   
   # Monitor system performance
   ./prism monitor --duration 300

[bold green]💡 Pro Tips:[/bold green]
   • Use --interactive for guided searches
   • Combine --plot and --export for complete analysis
   • Test database connections before large searches
   • Use --limit to control API usage
   • Export results for further analysis in other tools

[bold green]🔗 Database Information:[/bold green]
   • NOMAD: 19M+ DFT calculations, formation energies, band gaps
   • JARVIS: NIST materials, mechanical properties, 2D materials
   • OQMD: 700K+ materials, stability data, hull distances
   • COD: 500K+ crystal structures, space groups, lattice parameters

[bold green]📊 Output Formats:[/bold green]
   • CSV: Spreadsheet-compatible tabular data
   • JSON: Structured data with metadata
   • Plots: Formation energy distributions, band gap correlations
   • Reports: Comprehensive analysis with summaries
    """
    
    console.print(Panel(examples_text, title="PRISM CLI Examples", border_style="cyan"))


if __name__ == '__main__':
    cli()
