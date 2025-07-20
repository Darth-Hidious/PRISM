"""
Example integration of JARVIS connector with the data ingestion microservice.

This example shows how to create a data ingestion job that fetches materials
data from JARVIS and processes it through the microservice pipeline.
"""

import asyncio
import json
import logging
from datetime import datetime
from typing import Dict, List, Any

# Import the standalone connector to avoid config issues
from jarvis_demo_standalone import JarvisConnector


# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


class DataIngestionJob:
    """
    Simulated data ingestion job for demonstration.
    In the real implementation, this would integrate with the full microservice.
    """
    
    def __init__(self, job_id: str, source_config: Dict[str, Any]):
        self.job_id = job_id
        self.source_config = source_config
        self.created_at = datetime.now()
        self.status = "pending"
        self.results = []
        self.error_message = None
    
    async def execute(self) -> bool:
        """Execute the data ingestion job."""
        try:
            self.status = "running"
            logger.info(f"Starting job {self.job_id}")
            
            # Initialize JARVIS connector
            connector = JarvisConnector(
                timeout=30,
                requests_per_second=1.0,
                burst_capacity=5
            )
            
            await connector.connect()
            
            # Extract search parameters from source config
            search_params = self.source_config.get("search_params", {})
            dataset = self.source_config.get("dataset", "dft_3d")
            limit = self.source_config.get("limit", 10)
            
            # Search for materials
            logger.info(f"Searching for materials with params: {search_params}")
            
            # For demo purposes, use mock data since API endpoints are not available
            mock_materials = await self._get_mock_materials(connector, search_params, limit)
            
            # Process each material
            for material in mock_materials:
                processed_data = self._process_material(material)
                self.results.append(processed_data)
                logger.info(f"Processed material: {material['jid']} - {material['formula']}")
            
            await connector.disconnect()
            
            self.status = "completed"
            logger.info(f"Job {self.job_id} completed successfully. Processed {len(self.results)} materials.")
            return True
            
        except Exception as e:
            self.status = "failed"
            self.error_message = str(e)
            logger.error(f"Job {self.job_id} failed: {e}")
            return False
    
    async def _get_mock_materials(
        self, 
        connector: JarvisConnector, 
        search_params: Dict[str, Any], 
        limit: int
    ) -> List[Dict[str, Any]]:
        """Get mock materials data for demonstration."""
        
        # Create comprehensive mock data
        mock_materials = [
            {
                "jid": "JVASP-1001",
                "formula": "Si2",
                "formation_energy_peratom": -5.425,
                "ehull": 0.0,
                "bulk_modulus_kv": 97.8,
                "shear_modulus_gv": 51.5,
                "nelements": 1,
                "atoms": {
                    "lattice_mat": [[5.43, 0.0, 0.0], [0.0, 5.43, 0.0], [0.0, 0.0, 5.43]],
                    "elements": ["Si", "Si"],
                    "coords": [[0.0, 0.0, 0.0], [0.25, 0.25, 0.25]]
                }
            },
            {
                "jid": "JVASP-1002",
                "formula": "GaN",
                "formation_energy_peratom": -1.23,
                "ehull": 0.01,
                "bulk_modulus_kv": 207.0,
                "nelements": 2,
                "atoms": {
                    "lattice_mat": [[3.19, 0.0, 0.0], [0.0, 3.19, 0.0], [0.0, 0.0, 5.18]],
                    "elements": ["Ga", "N"],
                    "coords": [[0.0, 0.0, 0.0], [0.33, 0.33, 0.5]]
                }
            },
            {
                "jid": "JVASP-1003", 
                "formula": "AlN",
                "formation_energy_peratom": -2.85,
                "ehull": 0.0,
                "bulk_modulus_kv": 202.0,
                "nelements": 2,
                "atoms": {
                    "lattice_mat": [[3.11, 0.0, 0.0], [0.0, 3.11, 0.0], [0.0, 0.0, 4.98]],
                    "elements": ["Al", "N"],
                    "coords": [[0.0, 0.0, 0.0], [0.33, 0.33, 0.5]]
                }
            },
            {
                "jid": "JVASP-1004",
                "formula": "SiC",
                "formation_energy_peratom": -1.34,
                "ehull": 0.0,
                "bulk_modulus_kv": 220.0,
                "nelements": 2,
                "atoms": {
                    "lattice_mat": [[4.36, 0.0, 0.0], [0.0, 4.36, 0.0], [0.0, 0.0, 4.36]],
                    "elements": ["Si", "C"],
                    "coords": [[0.0, 0.0, 0.0], [0.25, 0.25, 0.25]]
                }
            }
        ]
        
        # Apply search filters
        filtered_materials = []
        
        for material in mock_materials:
            if len(filtered_materials) >= limit:
                break
            
            # Filter by formula if specified
            if "formula" in search_params:
                target_formula = search_params["formula"]
                if not connector._matches_formula(material, target_formula):
                    continue
            
            # Filter by number of elements if specified
            if "n_elements" in search_params:
                if material.get("nelements") != search_params["n_elements"]:
                    continue
            
            # Filter by formation energy range if specified
            if "formation_energy_range" in search_params:
                min_energy, max_energy = search_params["formation_energy_range"]
                formation_energy = material.get("formation_energy_peratom")
                if formation_energy is None or not (min_energy <= formation_energy <= max_energy):
                    continue
            
            # Extract material data
            extracted = connector._extract_material_data(
                material, 
                search_params.get("properties")
            )
            filtered_materials.append(extracted)
        
        logger.info(f"Filtered {len(filtered_materials)} materials from {len(mock_materials)} total")
        return filtered_materials
    
    def _process_material(self, material: Dict[str, Any]) -> Dict[str, Any]:
        """Process a material data record."""
        
        # Simulate data processing and enrichment
        processed = {
            "ingestion_job_id": self.job_id,
            "source": "JARVIS-DFT",
            "ingested_at": datetime.now().isoformat(),
            "raw_data": material,
            "processed_data": {
                "material_id": material["jid"],
                "chemical_formula": material["formula"],
                "thermodynamic_stability": {
                    "formation_energy_per_atom": material.get("formation_energy_peratom"),
                    "energy_above_hull": material.get("ehull"),
                    "is_stable": material.get("ehull", float('inf')) <= 0.1
                },
                "mechanical_properties": material.get("elastic_constants", {}),
                "structural_data": material.get("structure", {}),
                "data_quality": self._assess_data_quality(material)
            }
        }
        
        return processed
    
    def _assess_data_quality(self, material: Dict[str, Any]) -> Dict[str, Any]:
        """Assess the quality of material data."""
        
        quality_score = 0
        max_score = 5
        
        # Check for essential properties
        if material.get("jid"):
            quality_score += 1
        if material.get("formula"):
            quality_score += 1
        if material.get("formation_energy_peratom") is not None:
            quality_score += 1
        if material.get("structure"):
            quality_score += 1
        if material.get("elastic_constants"):
            quality_score += 1
        
        return {
            "score": quality_score / max_score,
            "completeness": quality_score / max_score,
            "has_structure": material.get("structure") is not None,
            "has_thermodynamics": material.get("formation_energy_peratom") is not None,
            "has_mechanics": material.get("elastic_constants") is not None
        }
    
    def get_summary(self) -> Dict[str, Any]:
        """Get job summary."""
        return {
            "job_id": self.job_id,
            "status": self.status,
            "created_at": self.created_at.isoformat(),
            "source_config": self.source_config,
            "results_count": len(self.results),
            "error_message": self.error_message
        }


async def create_and_run_ingestion_job(
    job_config: Dict[str, Any]
) -> DataIngestionJob:
    """Create and execute a data ingestion job."""
    
    job_id = f"jarvis-job-{datetime.now().strftime('%Y%m%d-%H%M%S')}"
    
    job = DataIngestionJob(job_id, job_config)
    success = await job.execute()
    
    return job


async def demo_silicon_materials():
    """Demo: Fetch silicon-based materials."""
    logger.info("\n=== Demo: Silicon Materials Ingestion ===")
    
    config = {
        "dataset": "dft_3d",
        "search_params": {
            "formula": "Si",
            "properties": ["bulk_modulus_kv", "shear_modulus_gv"]
        },
        "limit": 5
    }
    
    job = await create_and_run_ingestion_job(config)
    
    logger.info(f"Job Summary: {json.dumps(job.get_summary(), indent=2)}")
    
    if job.results:
        logger.info(f"\nFirst material result:")
        logger.info(json.dumps(job.results[0], indent=2, default=str))


async def demo_binary_compounds():
    """Demo: Fetch binary compounds with high formation energy."""
    logger.info("\n=== Demo: Binary Compounds Ingestion ===")
    
    config = {
        "dataset": "dft_3d",
        "search_params": {
            "n_elements": 2,
            "formation_energy_range": [-3.0, 0.0],  # Stable compounds
            "properties": ["formation_energy_peratom", "ehull", "bulk_modulus_kv"]
        },
        "limit": 3
    }
    
    job = await create_and_run_ingestion_job(config)
    
    logger.info(f"Job Summary: {json.dumps(job.get_summary(), indent=2)}")
    
    # Show data quality assessment
    if job.results:
        logger.info("\nData Quality Assessment:")
        for result in job.results:
            material_id = result["processed_data"]["material_id"]
            quality = result["processed_data"]["data_quality"]
            logger.info(f"  {material_id}: Quality Score = {quality['score']:.2f}")


async def demo_batch_processing():
    """Demo: Batch processing of multiple materials."""
    logger.info("\n=== Demo: Batch Processing ===")
    
    # Create multiple jobs with different search criteria
    job_configs = [
        {
            "dataset": "dft_3d",
            "search_params": {"formula": "Si"},
            "limit": 2
        },
        {
            "dataset": "dft_3d", 
            "search_params": {"formula": "Al"},
            "limit": 2
        },
        {
            "dataset": "dft_3d",
            "search_params": {"n_elements": 2},
            "limit": 2
        }
    ]
    
    # Run jobs concurrently
    tasks = []
    for config in job_configs:
        task = create_and_run_ingestion_job(config)
        tasks.append(task)
    
    jobs = await asyncio.gather(*tasks)
    
    # Summarize results
    total_materials = sum(len(job.results) for job in jobs)
    successful_jobs = sum(1 for job in jobs if job.status == "completed")
    
    logger.info(f"Batch Processing Summary:")
    logger.info(f"  Total jobs: {len(jobs)}")
    logger.info(f"  Successful jobs: {successful_jobs}")
    logger.info(f"  Total materials processed: {total_materials}")
    
    for i, job in enumerate(jobs, 1):
        logger.info(f"  Job {i}: {job.status} - {len(job.results)} materials")


async def main():
    """Main demonstration function."""
    logger.info("JARVIS Connector - Data Ingestion Integration Demo")
    logger.info("=" * 60)
    
    try:
        # Run different demo scenarios
        await demo_silicon_materials()
        await demo_binary_compounds() 
        await demo_batch_processing()
        
        logger.info("\n" + "=" * 60)
        logger.info("Integration demo completed successfully!")
        
    except Exception as e:
        logger.error(f"Demo failed: {e}")
        raise


if __name__ == "__main__":
    asyncio.run(main())
