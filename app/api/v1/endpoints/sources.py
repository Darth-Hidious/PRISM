import logging
from uuid import UUID, uuid4
from typing import List, Optional

from fastapi import APIRouter, Depends, HTTPException, status, Query
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, update, delete

from ....core.dependencies import get_database_session
from ....db.models import DataSource
from ....schemas import DataSourceCreate, DataSourceResponse

logger = logging.getLogger(__name__)
router = APIRouter()


@router.post("/", response_model=DataSourceResponse, status_code=status.HTTP_201_CREATED)
async def create_data_source(
    source_data: DataSourceCreate,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Create a new data source configuration.
    """
    try:
        source = DataSource(
            id=uuid4(),
            name=source_data.name,
            description=source_data.description,
            source_type=source_data.source_type,
            connection_config=source_data.connection_config,
            tags=source_data.tags
        )
        
        db.add(source)
        await db.commit()
        await db.refresh(source)
        
        logger.info(f"Created data source {source.id}: {source.name}")
        return DataSourceResponse.from_orm(source)
        
    except Exception as e:
        logger.error(f"Failed to create data source: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to create data source: {str(e)}"
        )


@router.get("/", response_model=List[DataSourceResponse])
async def list_data_sources(
    skip: int = Query(0, ge=0),
    limit: int = Query(100, ge=1, le=1000),
    source_type: Optional[str] = Query(None, description="Filter by source type"),
    active_only: bool = Query(True, description="Show only active sources"),
    db: AsyncSession = Depends(get_database_session)
):
    """
    List data source configurations.
    """
    try:
        query = select(DataSource)
        
        if active_only:
            query = query.where(DataSource.is_active == True)
        
        if source_type:
            query = query.where(DataSource.source_type == source_type)
        
        query = query.offset(skip).limit(limit).order_by(DataSource.created_at.desc())
        
        result = await db.execute(query)
        sources = result.scalars().all()
        
        return [DataSourceResponse.from_orm(source) for source in sources]
        
    except Exception as e:
        logger.error(f"Failed to list data sources: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to list data sources: {str(e)}"
        )


@router.get("/{source_id}", response_model=DataSourceResponse)
async def get_data_source(
    source_id: UUID,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Get a specific data source by ID.
    """
    try:
        result = await db.execute(
            select(DataSource).where(DataSource.id == source_id)
        )
        source = result.scalar_one_or_none()
        
        if not source:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data source {source_id} not found"
            )
        
        return DataSourceResponse.from_orm(source)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to get data source {source_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to get data source: {str(e)}"
        )


@router.put("/{source_id}", response_model=DataSourceResponse)
async def update_data_source(
    source_id: UUID,
    source_data: DataSourceCreate,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Update a data source configuration.
    """
    try:
        result = await db.execute(
            update(DataSource)
            .where(DataSource.id == source_id)
            .values(
                name=source_data.name,
                description=source_data.description,
                source_type=source_data.source_type,
                connection_config=source_data.connection_config,
                tags=source_data.tags
            )
            .returning(DataSource)
        )
        
        updated_source = result.scalar_one_or_none()
        
        if not updated_source:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data source {source_id} not found"
            )
        
        await db.commit()
        
        logger.info(f"Updated data source {source_id}: {updated_source.name}")
        return DataSourceResponse.from_orm(updated_source)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to update data source {source_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to update data source: {str(e)}"
        )


@router.delete("/{source_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_data_source(
    source_id: UUID,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Delete a data source configuration.
    """
    try:
        result = await db.execute(
            delete(DataSource).where(DataSource.id == source_id)
        )
        
        if result.rowcount == 0:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data source {source_id} not found"
            )
        
        await db.commit()
        
        logger.info(f"Deleted data source {source_id}")
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to delete data source {source_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to delete data source: {str(e)}"
        )


@router.post("/{source_id}/activate", response_model=DataSourceResponse)
async def activate_data_source(
    source_id: UUID,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Activate a data source.
    """
    try:
        result = await db.execute(
            update(DataSource)
            .where(DataSource.id == source_id)
            .values(is_active=True)
            .returning(DataSource)
        )
        
        updated_source = result.scalar_one_or_none()
        
        if not updated_source:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data source {source_id} not found"
            )
        
        await db.commit()
        
        logger.info(f"Activated data source {source_id}")
        return DataSourceResponse.from_orm(updated_source)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to activate data source {source_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to activate data source: {str(e)}"
        )


@router.post("/{source_id}/deactivate", response_model=DataSourceResponse)
async def deactivate_data_source(
    source_id: UUID,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Deactivate a data source.
    """
    try:
        result = await db.execute(
            update(DataSource)
            .where(DataSource.id == source_id)
            .values(is_active=False)
            .returning(DataSource)
        )
        
        updated_source = result.scalar_one_or_none()
        
        if not updated_source:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data source {source_id} not found"
            )
        
        await db.commit()
        
        logger.info(f"Deactivated data source {source_id}")
        return DataSourceResponse.from_orm(updated_source)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to deactivate data source {source_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to deactivate data source: {str(e)}"
        )
