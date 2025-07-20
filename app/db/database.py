import logging
from typing import AsyncGenerator

from sqlalchemy.ext.asyncio import create_async_engine, AsyncSession, async_sessionmaker
from sqlalchemy.orm import DeclarativeBase
from sqlalchemy.pool import NullPool

from ..core.config import get_settings


logger = logging.getLogger(__name__)


class Base(DeclarativeBase):
    """Base class for all database models."""
    pass


class DatabaseManager:
    """Database connection manager."""
    
    def __init__(self):
        self.settings = get_settings()
        self._engine = None
        self._session_factory = None
    
    @property
    def engine(self):
        """Get or create async engine."""
        if self._engine is None:
            self._engine = create_async_engine(
                self.settings.database_url,
                echo=self.settings.postgres_echo,
                poolclass=NullPool,  # Use NullPool for async
                future=True
            )
        return self._engine
    
    @property
    def session_factory(self):
        """Get or create session factory."""
        if self._session_factory is None:
            self._session_factory = async_sessionmaker(
                bind=self.engine,
                class_=AsyncSession,
                expire_on_commit=False,
                autoflush=False,
                autocommit=False
            )
        return self._session_factory
    
    async def create_tables(self):
        """Create all database tables."""
        async with self.engine.begin() as conn:
            await conn.run_sync(Base.metadata.create_all)
            logger.info("Database tables created successfully")
    
    async def close(self):
        """Close database connections."""
        if self._engine:
            await self._engine.dispose()
            logger.info("Database connections closed")


# Global database manager instance
db_manager = DatabaseManager()


async def get_db_session() -> AsyncGenerator[AsyncSession, None]:
    """Get database session generator."""
    async with db_manager.session_factory() as session:
        try:
            yield session
            await session.commit()
        except Exception as e:
            await session.rollback()
            logger.error(f"Database session error: {e}")
            raise
        finally:
            await session.close()


async def init_db():
    """Initialize database."""
    try:
        await db_manager.create_tables()
        logger.info("Database initialized successfully")
    except Exception as e:
        logger.error(f"Failed to initialize database: {e}")
        raise


async def close_db():
    """Close database connections."""
    await db_manager.close()
