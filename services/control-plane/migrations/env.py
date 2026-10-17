"""Alembic environment.

The async engine matches the application (``create_async_engine``), so the same
``postgresql+asyncpg://`` / ``sqlite+aiosqlite://`` URLs work for migrations and
for serving.
"""

from __future__ import annotations

import asyncio
from logging.config import fileConfig

from alembic import context
from sqlalchemy import pool
from sqlalchemy.ext.asyncio import async_engine_from_config

from yukinal_control_plane import models  # noqa: F401  (registers the tables)
from yukinal_control_plane.config import Settings
from yukinal_control_plane.db import Base

config = context.config
if config.config_file_name is not None:
    fileConfig(config.config_file_name)

# A caller (or CI) may set `sqlalchemy.url` explicitly; otherwise it comes from
# YUKINAL_CONTROL_DATABASE_URL, so serving and migrating share one source.
if not config.get_main_option("sqlalchemy.url"):
    config.set_main_option("sqlalchemy.url", Settings().database_url)
target_metadata = Base.metadata


def run_migrations_offline() -> None:
    context.configure(
        url=config.get_main_option("sqlalchemy.url"),
        target_metadata=target_metadata,
        literal_binds=True,
        dialect_opts={"paramstyle": "named"},
    )
    with context.begin_transaction():
        context.run_migrations()


def do_run_migrations(connection) -> None:
    # render_as_batch keeps ALTER support on SQLite, which the tests use.
    context.configure(
        connection=connection, target_metadata=target_metadata, render_as_batch=True
    )
    with context.begin_transaction():
        context.run_migrations()


async def run_migrations_online() -> None:
    connectable = async_engine_from_config(
        config.get_section(config.config_ini_section, {}),
        prefix="sqlalchemy.",
        poolclass=pool.NullPool,
    )
    async with connectable.connect() as connection:
        await connection.run_sync(do_run_migrations)
    await connectable.dispose()


if context.is_offline_mode():
    run_migrations_offline()
else:
    asyncio.run(run_migrations_online())
