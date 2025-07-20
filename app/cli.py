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

# Import our application modules
try:
    from app.core.config import get_settings
    from app.services.connectors.jarvis_connector import JarvisConnector
    from app.services.connectors.nomad_connector import NOMADConnector
    from app.services.job_processor import JobProcessor
    from app.services.job_scheduler import JobScheduler
    from app.db.database import get_database
    from app.db.models import Job, JobStatus, DataSource
except ImportError as e:
    print(f"Import error: {e}")
    print("Please ensure you're running from the project root directory")
    sys.exit(1)

console = Console()
settings = get_settings()


class CLIError(Exception):
    """Custom exception for CLI errors"""
    pass


def error_handler(func):
    """Decorator for handling CLI errors gracefully"""
    def wrapper(*args, **kwargs):
        try:
            return func(*args, **kwargs)
        except CLIError as e:
            console.print(f"[red]Error:[/red] {e}")
            sys.exit(1)
        except KeyboardInterrupt:
            console.print("\n[yellow]Operation cancelled by user[/yellow]")
            sys.exit(0)
        except Exception as e:
            console.print(f"[red]Unexpected error:[/red] {e}")
            if click.get_current_context().obj.get('debug', False):
                raise
            sys.exit(1)
    return wrapper


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
@click.pass_context
@error_handler
def fetch_material(ctx, source, material_id, formula, elements, output, output_format):
    """Fetch material data from a specific source"""
    
    with console.status(f"[bold green]Fetching material from {source.upper()}..."):
        if source == 'jarvis':
            connector = JarvisConnector()
        elif source == 'nomad':
            connector = NOMADConnector()
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
        
        if not query_params:
            raise CLIError("Must specify at least one search parameter")
        
        # Fetch the material
        try:
            # Use the correct method names from the connectors
            if material_id:
                result = asyncio.run(connector.get_material_by_id(material_id))
            else:
                result = asyncio.run(connector.search_materials(**query_params))
            
            if not result:
                console.print("[yellow]No materials found matching criteria[/yellow]")
                return
            
            # Format output
            if output_format == 'json':
                formatted_data = json.dumps(result, indent=2)
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
@error_handler
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
                connector = NOMADConnector()
            
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
@error_handler
def list_sources(output_format, status):
    """List all available data sources and their status"""
    
    # Get database session
    db = next(get_database())
    
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
@error_handler
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
                    connector = NOMADConnector()
                
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
@error_handler
def queue_status(refresh, watch):
    """Show job queue status and statistics"""
    
    def display_queue_status():
        db = next(get_database())
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
@error_handler
def retry_failed_jobs(max_age, source, dry_run, batch_size):
    """Retry failed jobs with filtering options"""
    
    db = next(get_database())
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
@error_handler
def export_data(output_format, output, source, date_from, date_to, status, limit):
    """Export data to various formats"""
    
    db = next(get_database())
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
@error_handler
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
            db = next(get_database())
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
@click.option('--list', 'list_config', is_flag=True, help='List current configuration')
@click.option('--set', 'set_value', help='Set configuration value (key=value)')
@click.option('--get', 'get_key', help='Get configuration value')
@error_handler
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


if __name__ == '__main__':
    cli()
