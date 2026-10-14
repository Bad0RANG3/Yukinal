from __future__ import annotations

import os
import stat
from contextlib import asynccontextmanager
from pathlib import Path
from typing import AsyncIterator


@asynccontextmanager
async def open_checkpoint_store(data_dir: str | Path) -> AsyncIterator[object]:
    """Open the per-user SQLite checkpoint store with strict deserialization."""
    if not str(data_dir).strip():
        raise ValueError("a private per-user data directory is required for checkpoints")
    root = Path(data_dir).expanduser().resolve()
    state_dir = root / "agent-state"
    state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    if os.name != "nt":
        if not stat.S_ISDIR(state_dir.lstat().st_mode):
            raise ValueError("Agent state path must be a directory")
        state_dir.chmod(0o700)

    database = state_dir / "langgraph-checkpoints.sqlite"
    if os.name != "nt":
        try:
            descriptor = os.open(
                database,
                os.O_CREAT | os.O_EXCL | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0),
                0o600,
            )
            os.close(descriptor)
        except FileExistsError:
            if not stat.S_ISREG(database.lstat().st_mode):
                raise ValueError("Agent checkpoint path must be a regular file") from None
            database.chmod(0o600)

    # Checkpoints contain prompts and tool output. The SQLite saver serializes
    # LangGraph messages with msgpack, so never allow arbitrary class loading
    # from a modified on-disk checkpoint.
    os.environ["LANGGRAPH_STRICT_MSGPACK"] = "true"
    from langgraph.checkpoint.sqlite.aio import AsyncSqliteSaver

    async with AsyncSqliteSaver.from_conn_string(str(database)) as saver:
        if os.name != "nt":
            if not stat.S_ISREG(database.lstat().st_mode):
                raise ValueError("Agent checkpoint path must be a regular file")
            database.chmod(0o600)
        yield saver
