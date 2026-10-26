from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import stat
from contextlib import closing
from pathlib import Path
from typing import Any


class FilesystemActionLedgerError(RuntimeError):
    """A previously started filesystem action cannot be replayed safely."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


class FilesystemActionLedger:
    """Durable exact-call fence for filesystem writes run directly by Python/C++."""

    def __init__(self, database: Path) -> None:
        self._database = database
        self._prepare_database_file()

    @classmethod
    def from_environment(cls) -> FilesystemActionLedger | None:
        data_dir = os.environ.get("YUKINAL_DATA_DIR", "").strip()
        if not data_dir:
            return None
        root = Path(data_dir).expanduser().resolve()
        state_dir = root / "agent-state"
        state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        if os.name != "nt":
            if not stat.S_ISDIR(state_dir.lstat().st_mode):
                raise ValueError("Agent state path must be a directory")
            state_dir.chmod(0o700)
        return cls(state_dir / "filesystem-actions.sqlite")

    async def claim(
        self,
        *,
        run_id: str,
        trace_id: str,
        call_id: str,
        tool_name: str,
        target_id: str,
        arguments: dict[str, Any],
        plan_id: str,
        plan_step_id: str,
        evidence_ids: tuple[str, ...],
    ) -> dict[str, Any] | None:
        """Reserve a call before remote I/O, or return its exact cached success."""
        # Only the call identity is required. A conversation has no plan; the run,
        # trace and call ids still make the exactly-once fence exact.
        if not all(isinstance(value, str) and value for value in (
            run_id, trace_id, call_id, tool_name, target_id,
        )) or not isinstance(plan_id, str) or not isinstance(plan_step_id, str):
            raise ValueError("filesystem action identity is incomplete")
        material = json.dumps(
            {
                "tool": tool_name,
                "target": target_id,
                "arguments": arguments,
                "planId": plan_id,
                "planStepId": plan_step_id,
                "evidenceIds": list(evidence_ids),
            },
            sort_keys=True,
            ensure_ascii=False,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
        fingerprint = hashlib.sha256(material).hexdigest()
        return await _run_in_thread(
            self._claim_sync, run_id, trace_id, call_id, fingerprint
        )

    async def complete(
        self,
        *,
        run_id: str,
        trace_id: str,
        call_id: str,
        tool_name: str,
        target_id: str,
        arguments: dict[str, Any],
        plan_id: str,
        plan_step_id: str,
        evidence_ids: tuple[str, ...],
        output: dict[str, Any],
    ) -> None:
        material = json.dumps(
            {
                "tool": tool_name,
                "target": target_id,
                "arguments": arguments,
                "planId": plan_id,
                "planStepId": plan_step_id,
                "evidenceIds": list(evidence_ids),
            },
            sort_keys=True,
            ensure_ascii=False,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
        fingerprint = hashlib.sha256(material).hexdigest()
        encoded_output = json.dumps(
            output, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        )
        if len(encoded_output.encode("utf-8")) > 16 * 1024:
            raise ValueError("filesystem action result exceeds its storage limit")
        await _run_in_thread(
            self._complete_sync,
            run_id,
            trace_id,
            call_id,
            fingerprint,
            encoded_output,
        )

    def _prepare_database_file(self) -> None:
        if os.name != "nt":
            try:
                descriptor = os.open(
                    self._database,
                    os.O_CREAT | os.O_EXCL | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0),
                    0o600,
                )
                os.close(descriptor)
            except FileExistsError:
                metadata = self._database.lstat()
                if not stat.S_ISREG(metadata.st_mode):
                    raise ValueError("Agent filesystem action ledger must be a regular file")
                self._database.chmod(0o600)
        elif self._database.exists() and not self._database.is_file():
            raise ValueError("Agent filesystem action ledger must be a regular file")
        with closing(self._connect()) as connection:
            connection.execute(
                """CREATE TABLE IF NOT EXISTS filesystem_actions (
                    run_id TEXT NOT NULL,
                    trace_id TEXT NOT NULL,
                    call_id TEXT NOT NULL,
                    fingerprint TEXT NOT NULL,
                    status TEXT NOT NULL CHECK(status IN ('running', 'succeeded')),
                    output_json TEXT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (run_id, trace_id, call_id),
                    CHECK ((status = 'running' AND output_json IS NULL)
                        OR (status = 'succeeded' AND output_json IS NOT NULL))
                )"""
            )

    def _connect(self) -> sqlite3.Connection:
        connection = sqlite3.connect(self._database, timeout=10, isolation_level=None)
        connection.execute("PRAGMA busy_timeout = 10000")
        connection.execute("PRAGMA journal_mode = WAL")
        return connection

    def _claim_sync(
        self, run_id: str, trace_id: str, call_id: str, fingerprint: str
    ) -> dict[str, Any] | None:
        with closing(self._connect()) as connection:
            connection.execute("BEGIN IMMEDIATE")
            row = connection.execute(
                "SELECT fingerprint, status, output_json FROM filesystem_actions "
                "WHERE run_id = ? AND trace_id = ? AND call_id = ?",
                (run_id, trace_id, call_id),
            ).fetchone()
            if row is None:
                connection.execute(
                    "INSERT INTO filesystem_actions "
                    "(run_id, trace_id, call_id, fingerprint, status) VALUES (?, ?, ?, ?, 'running')",
                    (run_id, trace_id, call_id, fingerprint),
                )
                connection.commit()
                return None
            existing_fingerprint, status, output_json = row
            connection.commit()
        if existing_fingerprint != fingerprint:
            raise FilesystemActionLedgerError(
                "duplicate_call_id", "This tool call ID is already bound to different filesystem input"
            )
        if status == "succeeded" and isinstance(output_json, str):
            try:
                output = json.loads(output_json)
            except json.JSONDecodeError:
                raise FilesystemActionLedgerError(
                    "ledger_corrupt", "The saved filesystem action result is invalid"
                ) from None
            if isinstance(output, dict):
                return output
            raise FilesystemActionLedgerError(
                "ledger_corrupt", "The saved filesystem action result is invalid"
            )
        raise FilesystemActionLedgerError(
            "action_outcome_unknown",
            "This filesystem action was already started; inspect the target before issuing a new call",
        )

    def _complete_sync(
        self,
        run_id: str,
        trace_id: str,
        call_id: str,
        fingerprint: str,
        output_json: str,
    ) -> None:
        with closing(self._connect()) as connection:
            connection.execute("BEGIN IMMEDIATE")
            cursor = connection.execute(
                "UPDATE filesystem_actions SET status = 'succeeded', output_json = ? "
                "WHERE run_id = ? AND trace_id = ? AND call_id = ? "
                "AND fingerprint = ? AND status = 'running'",
                (output_json, run_id, trace_id, call_id, fingerprint),
            )
            if cursor.rowcount != 1:
                connection.rollback()
                raise FilesystemActionLedgerError(
                    "ledger_conflict", "The filesystem action ledger no longer matches this call"
                )
            connection.commit()


async def _run_in_thread(function: Any, *args: Any) -> Any:
    import asyncio

    return await asyncio.to_thread(function, *args)
