from __future__ import annotations

import asyncio
import json
import os
import threading
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Protocol


@dataclass(frozen=True)
class AuditEvent:
    event_id: str
    status: str
    task_id: str
    tenant_id: str
    user_id: str
    tool_name: str
    target_id: str | None
    started_at: str
    finished_at: str | None
    argument_keys: tuple[str, ...]
    argument_types: tuple[tuple[str, str], ...]
    result_summary: dict[str, object] | None = None
    error_code: str | None = None
    approval_status: str = "not_required"
    #: The agent-level call identity. It is the only key that ties the pre-approval
    #: row, the post-approval row and the final approval decision together: a
    #: rejected or expired call never executes, so it would otherwise leave a
    #: `started`/`awaiting_approval` pair with no terminal row at all and the audit
    #: could not answer "what was decided about this call?".
    call_id: str = ""
    #: Wall-clock time this call spent waiting for a person, when it waited. The
    #: attempt rows otherwise measure approval latency as tool execution time.
    approval_wait_ms: int | None = None


class AuditSink(Protocol):
    async def record(self, event: AuditEvent) -> None: ...


class MemoryAuditSink:
    """In-memory audit implementation for tests and embedded deployments."""

    def __init__(self) -> None:
        self.events: list[AuditEvent] = []
        self._lock = asyncio.Lock()

    async def record(self, event: AuditEvent) -> None:
        async with self._lock:
            self.events.append(event)


class JsonlAuditSink:
    """Append-only JSONL audit sink; callers provide already-redacted summaries."""

    def __init__(self, path: str | os.PathLike[str]) -> None:
        self._path = Path(path)
        self._lock = threading.Lock()

    async def record(self, event: AuditEvent) -> None:
        line = json.dumps(asdict(event), ensure_ascii=False, separators=(",", ":")) + "\n"
        await asyncio.to_thread(self._append, line.encode("utf-8"))

    def _append(self, payload: bytes) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True)
        flags = os.O_APPEND | os.O_CREAT | os.O_WRONLY
        descriptor = os.open(self._path, flags, 0o600)
        try:
            with self._lock:
                offset = 0
                while offset < len(payload):
                    offset += os.write(descriptor, payload[offset:])
        finally:
            os.close(descriptor)
