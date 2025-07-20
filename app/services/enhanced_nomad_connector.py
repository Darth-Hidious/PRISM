"""
Enhanced NOMAD connector with database integration and progress tracking.
"""

import asyncio
import logging
from typing import List, Optional, Dict, Any, AsyncGenerator
from datetime import datetime, timezone

from app.services.connectors.nomad_connector import NOMADConnector
from app.services.materials_service import MaterialsService
from app.db.database import init_db_sync
from app.db.models import Job, MaterialEntry

logger = logging.getLogger(__name__)


class EnhancedNOMADConnector:
    """NOMAD connector with database integration and progress tracking."""
    
    def __init__(self, config: Dict[str, Any], auto_store: bool = True):
        """
        Initialize enhanced NOMAD connector.
        
        Args:
            config: NOMAD connector configuration
            auto_store: Whether to automatically store materials in database
        """
        self.nomad_connector = NOMADConnector(config)
        self.materials_service = MaterialsService()
        self.auto_store = auto_store
        self.batch_size = config.get("batch_size", 50)  # Smaller batches for better progress tracking
        
        # Initialize database
        try:
            init_db_sync()
            logger.info("Database initialized successfully")
        except Exception as e:
            logger.warning(f"Database initialization failed: {e}")
    
    async def connect(self) -> bool:
        """Connect to NOMAD API."""
        return await self.nomad_connector.connect()
    
    async def disconnect(self) -> bool:
        """Disconnect from NOMAD API."""
        return await self.nomad_connector.disconnect()
    
    async def search_and_store_materials(
        self,
        query_params: Dict[str, Any],
        max_results: Optional[int] = None,
        progress_callback: Optional[callable] = None
    ) -> Dict[str, Any]:
        """
        Search for materials and store them in the database with progress tracking.
        
        Args:
            query_params: Search parameters (elements, formula, etc.)
            max_results: Maximum number of results to fetch (None for all)
            progress_callback: Function to call with progress updates
            
        Returns:
            Dictionary with search and storage statistics
        """
        logger.info(f"Starting material search with params: {query_params}")
        
        # First, get total count to estimate progress
        total_available = await self._get_total_count(query_params)
        
        if max_results:
            total_to_fetch = min(total_available, max_results)
        else:
            total_to_fetch = total_available
            
        logger.info(f"Found {total_available} materials, will fetch {total_to_fetch}")
        
        if progress_callback:
            progress_callback(f"Found {total_available} materials in NOMAD database")
        
        # Initialize counters
        fetched_count = 0
        stored_count = 0
        updated_count = 0
        error_count = 0
        
        # Process in batches
        batch_num = 0
        async for batch in self._fetch_materials_in_batches(query_params, total_to_fetch):
            batch_num += 1
            batch_size = len(batch)
            
            try:
                if progress_callback:
                    progress_callback(
                        f"Processing batch {batch_num}: {batch_size} materials "
                        f"(Total: {fetched_count + batch_size}/{total_to_fetch})"
                    )
                
                if self.auto_store:
                    # Store batch in database
                    batch_stored, batch_updated, batch_errors = self.materials_service.store_materials(batch)
                    stored_count += batch_stored
                    updated_count += batch_updated
                    error_count += len(batch_errors)
                    
                    if progress_callback:
                        progress_callback(
                            f"Batch {batch_num} complete: {batch_stored} stored, "
                            f"{batch_updated} updated, {len(batch_errors)} errors"
                        )
                
                fetched_count += batch_size
                
                # Progress update
                progress_pct = (fetched_count / total_to_fetch) * 100
                logger.info(f"Progress: {fetched_count}/{total_to_fetch} ({progress_pct:.1f}%)")
                
                if progress_callback:
                    progress_callback(f"Progress: {progress_pct:.1f}% complete")
                
            except Exception as e:
                error_count += batch_size
                logger.error(f"Error processing batch {batch_num}: {e}")
                if progress_callback:
                    progress_callback(f"Error in batch {batch_num}: {e}")
        
        # Final statistics
        stats = {
            "total_available": total_available,
            "total_fetched": fetched_count,
            "total_stored": stored_count,
            "total_updated": updated_count,
            "total_errors": error_count,
            "batches_processed": batch_num
        }
        
        logger.info(f"Search complete: {stats}")
        if progress_callback:
            progress_callback(f"Complete! Fetched: {fetched_count}, Stored: {stored_count}, Updated: {updated_count}")
        
        return stats
    
    async def _get_total_count(self, query_params: Dict[str, Any]) -> int:
        """Get total count of materials for the query."""
        try:
            # Use the NOMAD connector's method to get total count
            test_params = query_params.copy()
            test_params["page_size"] = 1
            
            # Use the working GET endpoint approach
            import httpx
            async with httpx.AsyncClient(timeout=30.0) as client:
                response = await client.get(
                    "https://nomad-lab.eu/prod/rae/api/v1/entries",
                    params=test_params
                )
                if response.status_code == 200:
                    data = response.json()
                    return data.get("pagination", {}).get("total", 0)
                else:
                    logger.warning(f"Failed to get total count: {response.status_code}")
                    return 0
        except Exception as e:
            logger.warning(f"Error getting total count: {e}")
            return 0
    
    async def _fetch_materials_in_batches(
        self, 
        query_params: Dict[str, Any], 
        max_results: int
    ) -> AsyncGenerator[List, None]:
        """Fetch materials in batches."""
        offset = 0
        
        while offset < max_results:
            # Calculate batch size for this iteration
            remaining = max_results - offset
            current_batch_size = min(self.batch_size, remaining)
            
            try:
                # Prepare query parameters for this batch
                batch_params = query_params.copy()
                batch_params.update({
                    "limit": current_batch_size,
                    "page_size": current_batch_size,
                    "page_offset": offset
                })
                
                # Fetch materials using the NOMAD connector
                materials = await self.nomad_connector.search_materials(**batch_params)
                
                if not materials:
                    logger.info("No more materials found, stopping iteration")
                    break
                
                yield materials
                offset += len(materials)
                
                # Add a small delay to be respectful to the API
                await asyncio.sleep(0.5)
                
            except Exception as e:
                logger.error(f"Error fetching batch at offset {offset}: {e}")
                break
    
    def get_stored_materials_count(self) -> int:
        """Get count of materials stored in local database."""
        stats = self.materials_service.get_statistics()
        return stats.get("total_materials", 0)
    
    def search_local_materials(self, **kwargs) -> List[MaterialEntry]:
        """Search materials in local database."""
        materials, total = self.materials_service.search_materials(**kwargs)
        return materials
    
    def get_database_statistics(self) -> Dict[str, Any]:
        """Get database statistics."""
        return self.materials_service.get_statistics()


def create_progress_printer():
    """Create a simple progress callback that prints to console."""
    def progress_callback(message: str):
        timestamp = datetime.now().strftime("%H:%M:%S")
        print(f"[{timestamp}] {message}")
    return progress_callback
