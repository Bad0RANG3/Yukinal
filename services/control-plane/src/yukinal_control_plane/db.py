"""Async SQLAlchemy engine/session wiring and the declarative base."""

from __future__ import annotations

from collections.abc import AsyncIterator

from sqlalchemy.ext.asyncio import (
    AsyncEngine,
    AsyncSession,
    async_sessionmaker,
    create_async_engine,
)
from sqlalchemy.orm import DeclarativeBase


class Base(DeclarativeBase):
    """Declarative base for every control-plane table."""


def create_engine(database_url: str) -> AsyncEngine:
    """Create the async engine for ``database_url``.

    ``pool_pre_ping`` is on so a dropped PostgreSQL connection (or a restarted
    server) is detected and replaced instead of surfacing a stale-connection
    error to a node.
    """
    return create_async_engine(database_url, pool_pre_ping=True, future=True)


def create_session_factory(engine: AsyncEngine) -> async_sessionmaker[AsyncSession]:
    return async_sessionmaker(engine, expire_on_commit=False)


async def create_schema(engine: AsyncEngine) -> None:
    """Create every table.

    Production deployments run Alembic (``migrations/``); this helper exists so
    tests and local development can build a schema without a migration runner.
    """
    async with engine.begin() as connection:
        await connection.run_sync(Base.metadata.create_all)


async def session_scope(
    factory: async_sessionmaker[AsyncSession],
) -> AsyncIterator[AsyncSession]:
    """Yield a session whose transaction commits on success and rolls back on error."""
    async with factory() as session:
        async with session.begin():
            yield session
