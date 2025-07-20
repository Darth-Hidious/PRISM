import logging
from uuid import UUID, uuid4
from typing import List, Optional

from fastapi import APIRouter, Depends, HTTPException, status, Query
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, update, delete

from ....core.dependencies import get_database_session
from ....db.models import DataDestination
from ....schemas import DataDestinationCreate, DataDestinationResponse

logger = logging.getLogger(__name__)
router = APIRouter()


@router.post("/", response_model=DataDestinationResponse, status_code=status.HTTP_201_CREATED)
async def create_data_destination(
    destination_data: DataDestinationCreate,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Create a new data destination configuration.
    """
    try:
        destination = DataDestination(
            id=uuid4(),
            name=destination_data.name,
            description=destination_data.description,
            destination_type=destination_data.destination_type,
            connection_config=destination_data.connection_config,
            tags=destination_data.tags
        )
        
        db.add(destination)
        await db.commit()
        await db.refresh(destination)
        
        logger.info(f"Created data destination {destination.id}: {destination.name}")
        return DataDestinationResponse.from_orm(destination)
        
    except Exception as e:
        logger.error(f"Failed to create data destination: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to create data destination: {str(e)}"
        )


@router.get("/", response_model=List[DataDestinationResponse])
async def list_data_destinations(
    skip: int = Query(0, ge=0),
    limit: int = Query(100, ge=1, le=1000),
    destination_type: Optional[str] = Query(None, description="Filter by destination type"),
    active_only: bool = Query(True, description="Show only active destinations"),
    db: AsyncSession = Depends(get_database_session)
):
    """
    List data destination configurations.
    """
    try:
        query = select(DataDestination)
        
        if active_only:
            query = query.where(DataDestination.is_active == True)
        
        if destination_type:
            query = query.where(DataDestination.destination_type == destination_type)
        
        query = query.offset(skip).limit(limit).order_by(DataDestination.created_at.desc())
        
        result = await db.execute(query)
        destinations = result.scalars().all()
        
        return [DataDestinationResponse.from_orm(destination) for destination in destinations]
        
    except Exception as e:
        logger.error(f"Failed to list data destinations: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to list data destinations: {str(e)}"
        )


@router.get("/{destination_id}", response_model=DataDestinationResponse)
async def get_data_destination(
    destination_id: UUID,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Get a specific data destination by ID.
    """
    try:
        result = await db.execute(
            select(DataDestination).where(DataDestination.id == destination_id)
        )
        destination = result.scalar_one_or_none()
        
        if not destination:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data destination {destination_id} not found"
            )
        
        return DataDestinationResponse.from_orm(destination)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to get data destination {destination_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to get data destination: {str(e)}"
        )


@router.put("/{destination_id}", response_model=DataDestinationResponse)
async def update_data_destination(
    destination_id: UUID,
    destination_data: DataDestinationCreate,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Update a data destination configuration.
    """
    try:
        result = await db.execute(
            update(DataDestination)
            .where(DataDestination.id == destination_id)
            .values(
                name=destination_data.name,
                description=destination_data.description,
                destination_type=destination_data.destination_type,
                connection_config=destination_data.connection_config,
                tags=destination_data.tags
            )
            .returning(DataDestination)
        )
        
        updated_destination = result.scalar_one_or_none()
        
        if not updated_destination:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data destination {destination_id} not found"
            )
        
        await db.commit()
        
        logger.info(f"Updated data destination {destination_id}: {updated_destination.name}")
        return DataDestinationResponse.from_orm(updated_destination)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to update data destination {destination_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to update data destination: {str(e)}"
        )


@router.delete("/{destination_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_data_destination(
    destination_id: UUID,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Delete a data destination configuration.
    """
    try:
        result = await db.execute(
            delete(DataDestination).where(DataDestination.id == destination_id)
        )
        
        if result.rowcount == 0:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data destination {destination_id} not found"
            )
        
        await db.commit()
        
        logger.info(f"Deleted data destination {destination_id}")
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to delete data destination {destination_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to delete data destination: {str(e)}"
        )


@router.post("/{destination_id}/activate", response_model=DataDestinationResponse)
async def activate_data_destination(
    destination_id: UUID,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Activate a data destination.
    """
    try:
        result = await db.execute(
            update(DataDestination)
            .where(DataDestination.id == destination_id)
            .values(is_active=True)
            .returning(DataDestination)
        )
        
        updated_destination = result.scalar_one_or_none()
        
        if not updated_destination:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data destination {destination_id} not found"
            )
        
        await db.commit()
        
        logger.info(f"Activated data destination {destination_id}")
        return DataDestinationResponse.from_orm(updated_destination)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to activate data destination {destination_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to activate data destination: {str(e)}"
        )


@router.post("/{destination_id}/deactivate", response_model=DataDestinationResponse)
async def deactivate_data_destination(
    destination_id: UUID,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Deactivate a data destination.
    """
    try:
        result = await db.execute(
            update(DataDestination)
            .where(DataDestination.id == destination_id)
            .values(is_active=False)
            .returning(DataDestination)
        )
        
        updated_destination = result.scalar_one_or_none()
        
        if not updated_destination:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Data destination {destination_id} not found"
            )
        
        await db.commit()
        
        logger.info(f"Deactivated data destination {destination_id}")
        return DataDestinationResponse.from_orm(updated_destination)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to deactivate data destination {destination_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to deactivate data destination: {str(e)}"
        )
