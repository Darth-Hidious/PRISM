"""
Enhanced JARVIS Connector with Distributed Rate Limiting.

This example shows how to integrate the distributed rate limiter
with the existing JARVIS connector for production-ready API access.
"""

import asyncio
import logging
from typing import List, Dict, Any, Optional
import httpx

from app.services.connectors.base_connector import DatabaseConnector, StandardizedMaterial
from app.services.rate_limiter import rate_limit, RateLimitConfig, get_rate_limiter


logger = logging.getLogger(__name__)


class EnhancedJarvisConnector(DatabaseConnector):
    """
    Enhanced JARVIS connector with distributed rate limiting.
    
    This connector demonstrates best practices for integrating
    the distributed rate limiter with external API calls.
    """
    
    def __init__(
        self,
        api_key: Optional[str] = None,
        rate_limiter_enabled: bool = True,
        custom_rate_config: Optional[RateLimitConfig] = None
    ):
        """
        Initialize enhanced JARVIS connector.
        
        Args:
            api_key: Optional JARVIS API key
            rate_limiter_enabled: Whether to use rate limiting
            custom_rate_config: Custom rate limiting configuration
        """
        super().__init__(
            source_name="jarvis",
            base_url="https://jarvis.nist.gov/api/v1"
        )
        
        self.api_key = api_key
        self.rate_limiter_enabled = rate_limiter_enabled
        
        # Configure rate limiting if enabled
        if rate_limiter_enabled and custom_rate_config:
            rate_limiter = get_rate_limiter()
            if rate_limiter:
                rate_limiter.configure_source("jarvis", custom_rate_config)
                logger.info(f"Configured custom rate limiting for JARVIS: {custom_rate_config.requests_per_minute} RPM")
    
    async def connect(self) -> bool:
        """Establish connection to JARVIS API."""
        try:
            headers = {"Content-Type": "application/json"}
            if self.api_key:
                headers["Authorization"] = f"Bearer {self.api_key}"
            
            self.client = httpx.AsyncClient(
                base_url=self.base_url,
                headers=headers,
                timeout=30.0,
                limits=httpx.Limits(max_connections=10, max_keepalive_connections=5)
            )
            
            # Test connection with rate limiting
            if self.rate_limiter_enabled:
                await self._test_connection_with_rate_limit()
            else:
                await self._test_connection_basic()
            
            self.connected = True
            logger.info("Connected to JARVIS API")
            return True
            
        except Exception as e:
            logger.error(f"Failed to connect to JARVIS API: {e}")
            return False
    
    @rate_limit(source="jarvis", endpoint="health_check", tokens_requested=1)
    async def _test_connection_with_rate_limit(self) -> None:
        """Test connection with rate limiting."""
        response = await self.client.get("/info")
        response.raise_for_status()
        logger.debug("JARVIS API connection test successful")
    
    async def _test_connection_basic(self) -> None:
        """Test connection without rate limiting."""
        response = await self.client.get("/info")
        response.raise_for_status()
        logger.debug("JARVIS API connection test successful")
    
    @rate_limit(source="jarvis", endpoint="dft_3d", requests_per_minute=100, burst_capacity=50)
    async def fetch_dft_3d_materials(
        self,
        limit: int = 100,
        offset: int = 0,
        filters: Optional[Dict[str, Any]] = None
    ) -> List[Dict[str, Any]]:
        """
        Fetch 3D DFT materials with rate limiting.
        
        Args:
            limit: Maximum number of materials to fetch
            offset: Offset for pagination
            filters: Optional filters for the query
            
        Returns:
            List of material data
        """
        try:
            params = {
                "limit": limit,
                "offset": offset
            }
            
            if filters:
                params.update(filters)
            
            response = await self.client.get("/dft_3d", params=params)
            response.raise_for_status()
            
            data = response.json()
            logger.info(f"Fetched {len(data)} 3D DFT materials from JARVIS")
            
            return data
            
        except httpx.HTTPStatusError as e:
            if e.response.status_code == 429:
                logger.warning("JARVIS API rate limit hit for DFT 3D endpoint")
                # The rate limiter will automatically handle this via adaptive limiting
            raise
        except Exception as e:
            logger.error(f"Error fetching DFT 3D materials: {e}")
            raise
    
    @rate_limit(source="jarvis", endpoint="search", requests_per_minute=60, burst_capacity=30)
    async def search_materials_by_formula(
        self,
        formula: str,
        dataset: str = "dft_3d"
    ) -> List[Dict[str, Any]]:
        """
        Search materials by chemical formula with rate limiting.
        
        Args:
            formula: Chemical formula (e.g., "Al2O3")
            dataset: JARVIS dataset to search
            
        Returns:
            List of matching materials
        """
        try:
            params = {
                "formula": formula,
                "dataset": dataset
            }
            
            response = await self.client.get("/search", params=params)
            response.raise_for_status()
            
            data = response.json()
            logger.info(f"Found {len(data)} materials for formula {formula}")
            
            return data
            
        except Exception as e:
            logger.error(f"Error searching for formula {formula}: {e}")
            raise
    
    @rate_limit(source="jarvis", endpoint="bulk", requests_per_minute=30, burst_capacity=15)
    async def fetch_bulk_materials(
        self,
        material_ids: List[str],
        dataset: str = "dft_3d",
        batch_size: int = 50
    ) -> List[StandardizedMaterial]:
        """
        Fetch multiple materials in bulk with rate limiting and batching.
        
        Args:
            material_ids: List of JARVIS material IDs
            dataset: JARVIS dataset
            batch_size: Number of materials per batch request
            
        Returns:
            List of standardized materials
        """
        all_materials = []
        
        # Process in batches to respect rate limits
        for i in range(0, len(material_ids), batch_size):
            batch_ids = material_ids[i:i + batch_size]
            
            try:
                params = {
                    "ids": ",".join(batch_ids),
                    "dataset": dataset
                }
                
                response = await self.client.get("/bulk", params=params)
                response.raise_for_status()
                
                batch_data = response.json()
                
                # Standardize each material
                for material_data in batch_data:
                    try:
                        standardized = await self.standardize_data(material_data)
                        all_materials.append(standardized)
                    except Exception as e:
                        logger.warning(f"Failed to standardize material {material_data.get('id', 'unknown')}: {e}")
                
                logger.info(f"Processed batch {i//batch_size + 1}/{(len(material_ids) + batch_size - 1)//batch_size}")
                
            except Exception as e:
                logger.error(f"Error fetching batch {batch_ids}: {e}")
                # Continue with next batch rather than failing completely
                continue
        
        logger.info(f"Successfully fetched and standardized {len(all_materials)} materials")
        return all_materials
    
    async def get_material_by_id(self, material_id: str) -> Optional[StandardizedMaterial]:
        """Get a specific material by ID with automatic rate limiting."""
        # This method will automatically use the source-level rate limiting
        # since it doesn't specify an endpoint
        
        @rate_limit(source="jarvis", tokens_requested=1)
        async def _fetch_material():
            response = await self.client.get(f"/materials/{material_id}")
            response.raise_for_status()
            return response.json()
        
        try:
            material_data = await _fetch_material()
            return await self.standardize_data(material_data)
        except Exception as e:
            logger.error(f"Error fetching material {material_id}: {e}")
            return None
    
    async def search_materials(
        self,
        filters: Dict[str, Any],
        limit: int = 100
    ) -> List[StandardizedMaterial]:
        """Search materials with automatic rate limiting."""
        
        @rate_limit(source="jarvis", endpoint="search")
        async def _search():
            params = dict(filters)
            params["limit"] = limit
            
            response = await self.client.get("/search", params=params)
            response.raise_for_status()
            return response.json()
        
        try:
            search_results = await _search()
            materials = []
            
            for result in search_results:
                try:
                    standardized = await self.standardize_data(result)
                    materials.append(standardized)
                except Exception as e:
                    logger.warning(f"Failed to standardize search result: {e}")
            
            return materials
            
        except Exception as e:
            logger.error(f"Error searching materials: {e}")
            return []
    
    async def validate_response(self, response_data: Any) -> bool:
        """Validate JARVIS API response."""
        if not isinstance(response_data, dict):
            return False
        
        # Check for required JARVIS fields
        required_fields = ["jid", "formula"]
        return all(field in response_data for field in required_fields)
    
    async def standardize_data(self, raw_data: Dict[str, Any]) -> StandardizedMaterial:
        """Convert JARVIS data to standardized format."""
        # This is a simplified version - the full implementation would be more comprehensive
        from app.services.connectors.base_connector import MaterialStructure, MaterialProperties, MaterialMetadata
        
        # Extract basic information
        source_id = raw_data.get("jid", "")
        formula = raw_data.get("formula", "")
        
        # Extract structural information
        structure = MaterialStructure(
            lattice_parameters=raw_data.get("lattice", {}),
            atomic_positions=raw_data.get("atoms", []),
            space_group=raw_data.get("spg_symbol", ""),
            crystal_system=raw_data.get("crystal_system", "")
        )
        
        # Extract properties
        properties = MaterialProperties(
            formation_energy=raw_data.get("formation_energy_peratom"),
            band_gap=raw_data.get("optb88vdw_bandgap"),
            total_energy=raw_data.get("total_energy"),
            volume=raw_data.get("volume")
        )
        
        # Extract metadata
        metadata = MaterialMetadata(
            source="jarvis",
            source_id=source_id,
            last_updated=raw_data.get("last_updated"),
            version="1.0",
            confidence_score=0.9  # JARVIS data is generally high quality
        )
        
        return StandardizedMaterial(
            source_db="jarvis",
            source_id=source_id,
            formula=formula,
            structure=structure,
            properties=properties,
            metadata=metadata
        )
    
    async def disconnect(self) -> None:
        """Clean up connection resources."""
        if self.client:
            await self.client.aclose()
        self.connected = False
        logger.info("Disconnected from JARVIS API")


# Example usage and demonstration functions

async def demonstrate_rate_limited_jarvis_usage():
    """Demonstrate usage of the enhanced JARVIS connector with rate limiting."""
    
    # Custom rate limiting configuration for demonstration
    custom_config = RateLimitConfig(
        requests_per_minute=120,
        burst_capacity=60,
        queue_size=200,
        queue_timeout=60.0,
        adaptive_enabled=True,
        adaptive_backoff_factor=0.5,
        adaptive_recovery_factor=1.05
    )
    
    # Initialize connector
    connector = EnhancedJarvisConnector(
        rate_limiter_enabled=True,
        custom_rate_config=custom_config
    )
    
    try:
        # Connect to JARVIS
        await connector.connect()
        
        # Demonstrate different types of rate-limited operations
        
        # 1. Search for materials (uses endpoint-specific rate limiting)
        print("Searching for Al2O3 materials...")
        al2o3_materials = await connector.search_materials_by_formula("Al2O3")
        print(f"Found {len(al2o3_materials)} Al2O3 materials")
        
        # 2. Fetch DFT 3D materials (uses endpoint-specific rate limiting)
        print("Fetching DFT 3D materials...")
        dft_materials = await connector.fetch_dft_3d_materials(limit=50)
        print(f"Fetched {len(dft_materials)} DFT 3D materials")
        
        # 3. Bulk fetch with automatic batching and rate limiting
        if dft_materials:
            material_ids = [mat.get("jid", "") for mat in dft_materials[:10]]
            print(f"Bulk fetching {len(material_ids)} materials...")
            bulk_materials = await connector.fetch_bulk_materials(material_ids)
            print(f"Successfully fetched {len(bulk_materials)} materials in bulk")
        
        # 4. Get individual material (uses source-level rate limiting)
        if dft_materials:
            first_id = dft_materials[0].get("jid")
            if first_id:
                print(f"Fetching individual material {first_id}...")
                material = await connector.get_material_by_id(first_id)
                if material:
                    print(f"Fetched material: {material.formula}")
        
    except Exception as e:
        print(f"Error during demonstration: {e}")
    
    finally:
        # Clean up
        await connector.disconnect()


async def demonstrate_adaptive_rate_limiting():
    """Demonstrate adaptive rate limiting behavior."""
    
    connector = EnhancedJarvisConnector(rate_limiter_enabled=True)
    rate_limiter = get_rate_limiter()
    
    if not rate_limiter:
        print("Rate limiter not initialized")
        return
    
    try:
        await connector.connect()
        
        print("Demonstrating adaptive rate limiting...")
        
        # Simulate hitting rate limits
        for i in range(10):
            try:
                # This might hit rate limits and trigger adaptive backoff
                materials = await connector.fetch_dft_3d_materials(limit=10)
                print(f"Iteration {i+1}: Fetched {len(materials)} materials")
                
                # Get current metrics
                metrics = await rate_limiter.get_metrics("jarvis", "dft_3d")
                if metrics:
                    for key, metric in metrics.items():
                        print(f"  {key}: Adaptive multiplier = {metric.adaptive_multiplier:.2f}")
                
            except Exception as e:
                print(f"Iteration {i+1}: Error - {e}")
                
                # Report the error status for adaptive rate limiting
                if hasattr(e, 'response') and hasattr(e.response, 'status_code'):
                    await rate_limiter.report_response_status(
                        "jarvis", e.response.status_code, "dft_3d"
                    )
            
            # Small delay between iterations
            await asyncio.sleep(1)
    
    finally:
        await connector.disconnect()


if __name__ == "__main__":
    # Run demonstrations
    print("=== Enhanced JARVIS Connector with Rate Limiting ===")
    asyncio.run(demonstrate_rate_limited_jarvis_usage())
    
    print("\n=== Adaptive Rate Limiting Demonstration ===")
    asyncio.run(demonstrate_adaptive_rate_limiting())
