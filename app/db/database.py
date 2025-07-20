import logging
from typing import AsyncGenerator, Generator
from contextlib import contextmanager

from sqlalchemy import create_engine
from sqlalchemy.ext.asyncio import create_async_engine, AsyncSession, async_sessionmaker
from sqlalchemy.orm import DeclarativeBase, sessionmaker, Session
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
        self._async_engine = None
        self._sync_engine = None
        self._async_session_factory = None
        self._sync_session_factory = None
    
    @property
    def async_engine(self):
        """Get or create async engine."""
        if self._async_engine is None:
            # Construct database URL
            if hasattr(self.settings, 'database_url') and self.settings.database_url:
                database_url = self.settings.database_url
            else:
                database_url = (
                    f"postgresql+asyncpg://{self.settings.postgres_user}:{self.settings.postgres_password}"
                    f"@{self.settings.postgres_server}:{self.settings.postgres_port}/{self.settings.postgres_db}"
                )
            
            self._async_engine = create_async_engine(
                database_url,
                echo=self.settings.postgres_echo,
                poolclass=NullPool,  # Use NullPool for async
                future=True
            )
        return self._async_engine
    
    @property
    def sync_engine(self):
        """Get or create sync engine."""
        if self._sync_engine is None:
            # Construct database URL
            if hasattr(self.settings, 'database_url') and self.settings.database_url:
                database_url = self.settings.database_url
            else:
                database_url = (
                    f"postgresql+asyncpg://{self.settings.postgres_user}:{self.settings.postgres_password}"
                    f"@{self.settings.postgres_server}:{self.settings.postgres_port}/{self.settings.postgres_db}"
                )
            
            # Convert to sync URL
            sync_url = database_url.replace('postgresql+asyncpg://', 'postgresql://')
            self._sync_engine = create_engine(
                sync_url,
                echo=self.settings.postgres_echo,
                future=True
            )
        return self._sync_engine
    
    @property
    def async_session_factory(self):
        """Get or create async session factory."""
        if self._async_session_factory is None:
            self._async_session_factory = async_sessionmaker(
                bind=self.async_engine,
                class_=AsyncSession,
                expire_on_commit=False,
                autoflush=False,
                autocommit=False
            )
        return self._async_session_factory
    
    @property
    def sync_session_factory(self):
        """Get or create sync session factory."""
        if self._sync_session_factory is None:
            self._sync_session_factory = sessionmaker(
                bind=self.sync_engine,
                autoflush=False,
                autocommit=False
            )
        return self._sync_session_factory
    
    async def create_tables(self):
        """Create all database tables."""
        async with self.async_engine.begin() as conn:
            await conn.run_sync(Base.metadata.create_all)
            logger.info("Database tables created successfully")
    
    def create_tables_sync(self):
        """Create all database tables synchronously."""
        Base.metadata.create_all(self.sync_engine)
        logger.info("Database tables created successfully (sync)")
    
    async def close(self):
        """Close database connections."""
        if self._async_engine:
            await self._async_engine.dispose()
        if self._sync_engine:
            self._sync_engine.dispose()
        logger.info("Database connections closed")


# Global database manager instance
db_manager = DatabaseManager()


async def get_db_session() -> AsyncGenerator[AsyncSession, None]:
    """Get async database session generator."""
    async with db_manager.async_session_factory() as session:
        try:
            yield session
            await session.commit()
        except Exception as e:
            await session.rollback()
            logger.error(f"Database session error: {e}")
            raise
        finally:
            await session.close()


@contextmanager
def get_db_session_sync() -> Generator[Session, None, None]:
    """Get sync database session context manager."""
    session = db_manager.sync_session_factory()
    try:
        yield session
        session.commit()
    except Exception as e:
        session.rollback()
        logger.error(f"Database session error: {e}")
        raise
    finally:
        session.close()


async def init_db():
    """Initialize database."""
    try:
        await db_manager.create_tables()
        logger.info("Database initialized successfully")
    except Exception as e:
        logger.error(f"Failed to initialize database: {e}")
        raise


def init_db_sync():
    """Initialize database synchronously."""
    try:
        db_manager.create_tables_sync()
        logger.info("Database initialized successfully (sync)")
    except Exception as e:
        logger.error(f"Failed to initialize database: {e}")
        raise


async def close_db():
    """Close database connections."""
    await db_manager.close()
