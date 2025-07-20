#!/usr/bin/env python3
"""
Enhanced Job System Demo

This script demonstrates the advanced job system features including:
1. Different job types for material data fetching
2. Batch processing with progress tracking
3. Job scheduling and dependencies
4. Error handling with retry logic
5. Distributed rate limiting integration

Run this after setting up PostgreSQL and Redis.
"""

import asyncio
import json
import logging
from datetime import datetime, timedelta
from typing import List, Dict, Any

import httpx

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


class EnhancedJobSystemDemo:
    """Demo for the enhanced job system."""
    
    def __init__(self, base_url: str = "http://localhost:8000"):
        self.base_url = base_url
        self.client = httpx.AsyncClient(timeout=30.0)
    
    async def __aenter__(self):
        return self
    
    async def __aexit__(self, exc_type, exc_val, exc_tb):
        await self.client.aclose()
    
    async def demo_single_material_fetch(self):
        """Demo single material fetch job."""
        logger.info("=== Demo 1: Single Material Fetch ===")
        
        job_data = {
            "job_type": "fetch_single_material",
            "source_type": "jarvis",
            "source_config": {
                "material_id": "JVASP-1002",
                "dataset": "dft_3d"
            },
            "destination_type": "database",
            "priority": 8,
            "batch_size": 1,
            "retry_count": 3,
            "metadata": {
                "description": "Fetch single silicon material from JARVIS",
                "requester": "demo_system"
            }
        }
        
        response = await self.client.post(f"{self.base_url}/api/v1/jobs/", json=job_data)
        response.raise_for_status()
        job = response.json()
        
        logger.info(f"Created single material fetch job: {job['id']}")
        
        # Monitor progress
        await self._monitor_job(job['id'])
        
        return job['id']
    
    async def demo_bulk_formula_fetch(self):
        """Demo bulk fetch by formula job."""
        logger.info("=== Demo 2: Bulk Formula Fetch ===")
        
        job_data = {
            "job_type": "bulk_fetch_by_formula",
            "source_type": "jarvis",
            "source_config": {
                "formulas": ["Si", "GaAs", "AlN", "SiC", "InP"],
                "dataset": "dft_3d"
            },
            "destination_type": "database",
            "priority": 6,
            "batch_size": 2,
            "retry_count": 3,
            "metadata": {
                "description": "Bulk fetch semiconductor materials",
                "category": "semiconductors"
            }
        }
        
        response = await self.client.post(f"{self.base_url}/api/v1/jobs/", json=job_data)
        response.raise_for_status()
        job = response.json()
        
        logger.info(f"Created bulk formula fetch job: {job['id']}")
        
        # Monitor progress
        await self._monitor_job(job['id'])
        
        return job['id']
    
    async def demo_bulk_properties_fetch(self):
        """Demo bulk fetch by properties job."""
        logger.info("=== Demo 3: Bulk Properties Fetch ===")
        
        job_data = {
            "job_type": "bulk_fetch_by_properties",
            "source_type": "jarvis",
            "source_config": {
                "property_filters": {
                    "formation_energy_per_atom": {"min": -2.0, "max": 0.0},
                    "band_gap": {"min": 0.5, "max": 3.0}
                },
                "dataset": "dft_3d",
                "max_results": 50
            },
            "destination_type": "database",
            "priority": 5,
            "batch_size": 10,
            "retry_count": 2,
            "metadata": {
                "description": "Fetch materials with specific electronic properties",
                "property_focus": "semiconductors_with_bandgap"
            }
        }
        
        response = await self.client.post(f"{self.base_url}/api/v1/jobs/", json=job_data)
        response.raise_for_status()
        job = response.json()
        
        logger.info(f"Created bulk properties fetch job: {job['id']}")
        
        # Monitor progress
        await self._monitor_job(job['id'])
        
        return job['id']
    
    async def demo_database_sync(self):
        """Demo database sync job."""
        logger.info("=== Demo 4: Database Sync ===")
        
        job_data = {
            "job_type": "sync_database",
            "source_type": "jarvis",
            "source_config": {
                "dataset": "dft_2d",
                "incremental": True,
                "last_sync": None  # Full sync
            },
            "destination_type": "database",
            "priority": 10,  # High priority
            "batch_size": 20,
            "retry_count": 5,
            "metadata": {
                "description": "Sync JARVIS 2D materials database",
                "sync_type": "full"
            }
        }
        
        response = await self.client.post(f"{self.base_url}/api/v1/jobs/", json=job_data)
        response.raise_for_status()
        job = response.json()
        
        logger.info(f"Created database sync job: {job['id']}")
        
        # Monitor progress (this might take a while)
        await self._monitor_job(job['id'], timeout_minutes=30)
        
        return job['id']
    
    async def demo_scheduled_job(self):
        """Demo scheduled recurring job."""
        logger.info("=== Demo 5: Scheduled Job ===")
        
        job_data = {
            "job_type": "sync_database",
            "source_type": "jarvis",
            "source_config": {
                "dataset": "dft_3d",
                "incremental": True
            },
            "destination_type": "database",
            "priority": 7,
            "batch_size": 50,
            "retry_count": 3,
            "schedule_config": {
                "enabled": True,
                "cron_expression": "0 2 * * *",  # Daily at 2 AM
                "max_runs": 5  # Limit for demo
            },
            "metadata": {
                "description": "Daily incremental sync of JARVIS 3D materials",
                "schedule_type": "daily_sync"
            }
        }
        
        response = await self.client.post(f"{self.base_url}/api/v1/jobs/", json=job_data)
        response.raise_for_status()
        job = response.json()
        
        logger.info(f"Created scheduled job: {job['id']}")
        logger.info("Scheduled jobs will be executed by the job scheduler")
        
        return job['id']
    
    async def demo_job_dependencies(self):
        """Demo job dependencies."""
        logger.info("=== Demo 6: Job Dependencies ===")
        
        # Create parent job
        parent_job_data = {
            "job_type": "fetch_single_material",
            "source_type": "jarvis",
            "source_config": {
                "material_id": "JVASP-1001",
                "dataset": "dft_3d"
            },
            "destination_type": "database",
            "priority": 9,
            "metadata": {
                "description": "Parent job - fetch reference material",
                "role": "dependency_parent"
            }
        }
        
        response = await self.client.post(f"{self.base_url}/api/v1/jobs/", json=parent_job_data)
        response.raise_for_status()
        parent_job = response.json()
        parent_job_id = parent_job['id']
        
        logger.info(f"Created parent job: {parent_job_id}")
        
        # Create dependent job
        dependent_job_data = {
            "job_type": "bulk_fetch_by_formula",
            "source_type": "jarvis",
            "source_config": {
                "formulas": ["Si", "Ge"],
                "dataset": "dft_3d"
            },
            "destination_type": "database",
            "priority": 5,
            "dependencies": [parent_job_id],
            "metadata": {
                "description": "Dependent job - fetch related materials",
                "role": "dependency_child",
                "depends_on": parent_job_id
            }
        }
        
        response = await self.client.post(f"{self.base_url}/api/v1/jobs/", json=dependent_job_data)
        response.raise_for_status()
        dependent_job = response.json()
        dependent_job_id = dependent_job['id']
        
        logger.info(f"Created dependent job: {dependent_job_id}")
        logger.info("Dependent job will only start after parent job completes")
        
        # Monitor both jobs
        await self._monitor_job(parent_job_id)
        await self._monitor_job(dependent_job_id)
        
        return [parent_job_id, dependent_job_id]
    
    async def demo_bulk_operations(self):
        """Demo bulk job operations."""
        logger.info("=== Demo 7: Bulk Operations ===")
        
        # Create multiple jobs
        bulk_jobs_data = []
        formulas = ["TiO2", "Al2O3", "ZnO", "SnO2", "In2O3"]
        
        for formula in formulas:
            job_data = {
                "job_type": "fetch_single_material",
                "source_type": "jarvis",
                "source_config": {
                    "formula": formula,
                    "dataset": "dft_3d"
                },
                "destination_type": "database",
                "priority": 4,
                "metadata": {
                    "description": f"Fetch {formula} material",
                    "batch_group": "oxide_materials"
                }
            }
            bulk_jobs_data.append(job_data)
        
        response = await self.client.post(f"{self.base_url}/api/v1/jobs/bulk/create", json=bulk_jobs_data)
        response.raise_for_status()
        jobs = response.json()
        
        job_ids = [job['id'] for job in jobs]
        logger.info(f"Created {len(job_ids)} jobs in bulk: {job_ids}")
        
        # Cancel some jobs for demo
        cancel_ids = job_ids[:2]
        response = await self.client.post(f"{self.base_url}/api/v1/jobs/bulk/cancel", json=cancel_ids)
        response.raise_for_status()
        cancel_result = response.json()
        
        logger.info(f"Cancelled {cancel_result['cancelled_count']} jobs")
        
        # Monitor remaining jobs
        for job_id in job_ids[2:]:
            await self._monitor_job(job_id)
        
        return job_ids
    
    async def demo_job_statistics(self):
        """Demo job statistics."""
        logger.info("=== Demo 8: Job Statistics ===")
        
        response = await self.client.get(f"{self.base_url}/api/v1/jobs/stats?hours=24")
        response.raise_for_status()
        stats = response.json()
        
        logger.info("Job Statistics (last 24 hours):")
        logger.info(f"  Total jobs: {stats['total_jobs']}")
        logger.info(f"  Queued: {stats['queued_jobs']}")
        logger.info(f"  Processing: {stats['processing_jobs']}")
        logger.info(f"  Completed: {stats['completed_jobs']}")
        logger.info(f"  Failed: {stats['failed_jobs']}")
        logger.info(f"  Cancelled: {stats['cancelled_jobs']}")
        logger.info(f"  Success rate: {stats['success_rate']:.1f}%" if stats['success_rate'] else "  Success rate: N/A")
        logger.info(f"  Avg processing time: {stats['avg_processing_time']:.1f}s" if stats['avg_processing_time'] else "  Avg processing time: N/A")
        
        return stats
    
    async def _monitor_job(self, job_id: str, timeout_minutes: int = 10):
        """Monitor job progress until completion."""
        logger.info(f"Monitoring job {job_id}...")
        
        start_time = datetime.utcnow()
        timeout = timedelta(minutes=timeout_minutes)
        
        while True:
            try:
                # Get job status
                response = await self.client.get(f"{self.base_url}/api/v1/jobs/{job_id}")
                response.raise_for_status()
                job = response.json()
                
                status = job['status']
                progress = job['progress']
                processed = job['processed_records']
                total = job['total_records']
                rate = job.get('processing_rate')
                
                # Log progress
                rate_str = f" ({rate:.1f} items/s)" if rate else ""
                logger.info(f"Job {job_id}: {status} - {progress}% ({processed}/{total}){rate_str}")
                
                # Check if completed
                if status in ['completed', 'failed', 'cancelled']:
                    if status == 'completed':
                        logger.info(f"✓ Job {job_id} completed successfully!")
                        
                        # Get materials data
                        try:
                            materials_response = await self.client.get(
                                f"{self.base_url}/api/v1/jobs/{job_id}/materials?limit=5"
                            )
                            if materials_response.status_code == 200:
                                materials = materials_response.json()
                                logger.info(f"  Fetched {len(materials)} materials (showing first 5)")
                                for material in materials[:3]:
                                    formula = material.get('material_formula', 'Unknown')
                                    source_id = material.get('source_id', 'Unknown')
                                    logger.info(f"    - {formula} (ID: {source_id})")
                        except Exception as e:
                            logger.warning(f"Could not fetch materials data: {e}")
                    
                    elif status == 'failed':
                        logger.error(f"✗ Job {job_id} failed: {job.get('error_message', 'Unknown error')}")
                    
                    elif status == 'cancelled':
                        logger.warning(f"⚠ Job {job_id} was cancelled")
                    
                    break
                
                # Check timeout
                if datetime.utcnow() - start_time > timeout:
                    logger.warning(f"⏰ Monitoring timeout for job {job_id} after {timeout_minutes} minutes")
                    break
                
                # Wait before next check
                await asyncio.sleep(2)
                
            except httpx.HTTPStatusError as e:
                if e.response.status_code == 404:
                    logger.error(f"Job {job_id} not found")
                    break
                else:
                    logger.error(f"Error checking job status: {e}")
                    await asyncio.sleep(5)
            
            except Exception as e:
                logger.error(f"Error monitoring job: {e}")
                await asyncio.sleep(5)
    
    async def run_all_demos(self):
        """Run all demonstrations."""
        logger.info("🚀 Starting Enhanced Job System Demo")
        logger.info("=" * 60)
        
        demos = [
            ("Single Material Fetch", self.demo_single_material_fetch),
            ("Bulk Formula Fetch", self.demo_bulk_formula_fetch),
            ("Bulk Properties Fetch", self.demo_bulk_properties_fetch),
            ("Job Dependencies", self.demo_job_dependencies),
            ("Bulk Operations", self.demo_bulk_operations),
            ("Scheduled Job", self.demo_scheduled_job),
            ("Job Statistics", self.demo_job_statistics),
            # Note: Database sync demo is commented out as it takes a long time
            # ("Database Sync", self.demo_database_sync),
        ]
        
        results = {}
        
        for demo_name, demo_func in demos:
            try:
                logger.info(f"\n{'='*20} {demo_name} {'='*20}")
                result = await demo_func()
                results[demo_name] = result
                logger.info(f"✓ {demo_name} completed successfully")
                
                # Short pause between demos
                await asyncio.sleep(2)
                
            except Exception as e:
                logger.error(f"✗ {demo_name} failed: {e}")
                results[demo_name] = f"Error: {e}"
        
        logger.info("\n" + "="*60)
        logger.info("🎉 Enhanced Job System Demo Complete!")
        logger.info("=" * 60)
        
        # Summary
        logger.info("\nDemo Results Summary:")
        for demo_name, result in results.items():
            if isinstance(result, str) and result.startswith("Error"):
                logger.info(f"  ✗ {demo_name}: {result}")
            else:
                logger.info(f"  ✓ {demo_name}: Success")
        
        return results


async def main():
    """Main demo function."""
    try:
        async with EnhancedJobSystemDemo() as demo:
            # First check if the API is available
            try:
                response = await demo.client.get(f"{demo.base_url}/health")
                response.raise_for_status()
                logger.info("✓ API server is running")
            except Exception as e:
                logger.error(f"✗ Cannot connect to API server: {e}")
                logger.error("Please make sure the FastAPI server is running on http://localhost:8000")
                return
            
            # Run all demos
            await demo.run_all_demos()
            
    except KeyboardInterrupt:
        logger.info("\nDemo interrupted by user")
    except Exception as e:
        logger.error(f"Demo failed: {e}")


if __name__ == "__main__":
    asyncio.run(main())
