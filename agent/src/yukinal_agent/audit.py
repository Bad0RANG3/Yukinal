from __future__ import annotations

import asyncio
import json
import os
import re
import threading
from dataclasses import asdict, dataclass, replace
from pathlib import Path
from typing import Protocol

_SECRET_FIELD = re.compile(r"password|passwd|passphrase|credential|private.?key|token|secret|authorization|cookie", re.I)
_SECRET_TEXT = re.compile(
    r"-----BEGIN(?: [A-Z0-9]+)? PRIVATE KEY-----[\s\S]*?(?:-----END(?: [A-Z0-9]+)? PRIVATE KEY-----|$)"
    r"|\b(?:Bearer|Basic)\s+[^\s]+"
    r"|\b(?:password|passwd|passphrase|token|secret|api[_-]?key)\s*[:=]\s*(?:\"[^\"]*\"|'[^']*'|[^\s,;]+)"
    r"|\b[a-z][a-z0-9+.-]*://[^\s/@]+:[^\s/@]+@", re.I,
)


def redact_audit(value: object, depth: int = 0) -> object:
    """Redact at the sink, including exception summaries from future callers."""
    if depth > 16:
        return "[truncated]"
    if isinstance(value, dict):
        return {str(key): "[redacted]" if _SECRET_FIELD.search(str(key)) else redact_audit(item, depth + 1)
                for key, item in value.items()}
    if isinstance(value, (tuple, list)):
        return [redact_audit(item, depth + 1) for item in value]
    if isinstance(value, str):
        return _SECRET_TEXT.sub("[redacted]", value)
    return value


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
            self.events.append(replace(event, result_summary=redact_audit(event.result_summary)))


class JsonlAuditSink:
    """Append-only JSONL audit sink with mandatory structured redaction."""

    def __init__(self, path: str | os.PathLike[str]) -> None:
        self._path = Path(path)
        self._lock = threading.Lock()

    async def record(self, event: AuditEvent) -> None:
        line = json.dumps(redact_audit(asdict(event)), ensure_ascii=False, separators=(",", ":")) + "\n"
        await asyncio.to_thread(self._append, line.encode("utf-8"))

    def _append(self, payload: bytes) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True)
        flags = os.O_APPEND | os.O_CREAT | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(self._path, flags, 0o600)
        try:
            with self._lock:
                offset = 0
                while offset < len(payload):
                    offset += os.write(descriptor, payload[offset:])
        finally:
            os.close(descriptor)
