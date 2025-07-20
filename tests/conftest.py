import pytest
import asyncio
from sqlalchemy.ext.asyncio import create_async_engine, AsyncSession
from sqlalchemy.orm import sessionmaker
from redis.asyncio import Redis

from app.core.config import get_settings
from app.db.database import Base, get_db_session


@pytest.fixture(scope="session")
def event_loop():
    """Create an instance of the default event loop for each test session."""
    loop = asyncio.get_event_loop_policy().new_event_loop()
    yield loop
    loop.close()


@pytest.fixture(scope="session")
async def test_db_engine():
    """Create a test database engine."""
    settings = get_settings()
    engine = create_async_engine(settings.database_url, echo=settings.postgres_echo)
    async with engine.begin() as conn:
        await conn.run_sync(Base.metadata.create_all)
    yield engine
    async with engine.begin() as conn:
        await conn.run_sync(Base.metadata.drop_all)


@pytest.fixture(scope="function")
async def db_session(test_db_engine):
    """Create a new database session for a test."""
    connection = await test_db_engine.connect()
    trans = await connection.begin()
    Session = sessionmaker(connection, class_=AsyncSession, expire_on_commit=False)
    session = Session()

    yield session

    await session.close()
    await trans.rollback()
    await connection.close()


@pytest.fixture(scope="function")
async def redis_client():
    """Create a new Redis client for a test."""
    settings = get_settings()
    client = Redis(
        host=settings.redis_host,
        port=settings.redis_port,
        db=settings.redis_db,
        password=settings.redis_password,
        decode_responses=True,
    )
    await client.flushdb()
    yield client
    await client.close()
