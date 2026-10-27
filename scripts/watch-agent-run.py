#!/usr/bin/env python3
"""Read-only live monitor for a running Yukinal Agent session.

It never writes to the product database. It tails two append-only stores that the
Python Agent Node already owns:

  agent-state/conversations.sqlite :: conversation_events  (every agent.stream event)
  audit/python-agent-tools.jsonl                            (every tool attempt)

Usage:
  python scripts/watch-agent-run.py                 # from now on
  python scripts/watch-agent-run.py --backfill 40   # replay the last 40 events first
  python scripts/watch-agent-run.py --once          # print what exists and exit
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import time
from datetime import UTC, datetime
from pathlib import Path

SECRET_HINTS = ("password", "token", "secret", "private", "credential", "passphrase")


def data_dir() -> Path:
    override = os.environ.get("YUKINAL_DATA_DIR", "").strip()
    if override:
        return Path(override)
    if sys.platform == "win32":
        base = os.environ.get("APPDATA", "")
        return Path(base or Path.home() / "AppData" / "Roaming") / "dev.yukinal.workspace"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "dev.yukinal.workspace"
    base = os.environ.get("XDG_DATA_HOME", "")
    return Path(base or Path.home() / ".local" / "share") / "dev.yukinal.workspace"


def clock() -> str:
    return datetime.now(UTC).astimezone().strftime("%H:%M:%S")


def clip(value: object, limit: int = 240) -> str:
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False, default=str)
    text = " ".join(text.split())
    return text if len(text) <= limit else text[:limit] + f"…(+{len(text) - limit})"


def describe(event: dict) -> str:
    kind = event.get("type", "?")
    parts = [f"{kind:<22}"]
    run = event.get("runId")
    if isinstance(run, str) and run:
        parts.append(f"run={run[:14]}")
    if event.get("taskId"):
        parts.append(f"task={str(event['taskId'])[:12]}")

    if kind in ("agent.tool_call", "agent.tool_result"):
        parts.append(f"{event.get('toolName', '?')}")
        if event.get("callId"):
            parts.append(f"call={str(event['callId'])[:10]}")
        if kind == "agent.tool_call":
            parts.append(f"decision={event.get('decision')}")
            parts.append(f"risk={event.get('riskLevel')}")
            parts.append(f"input={clip(event.get('input'), 200)}")
        else:
            parts.append(f"status={event.get('status')}")
            if event.get("errorCode"):
                parts.append(f"error={event['errorCode']}")
            if event.get("durationMs") is not None:
                parts.append(f"{event['durationMs']}ms")
            if event.get("outputSummary"):
                parts.append(f"out={clip(event['outputSummary'], 300)}")
    elif kind == "agent.stage":
        parts.append(f"stage={event.get('previous')}->{event.get('stage')}")
    elif kind == "agent.tool_decision":
        parts.append(f"{event.get('toolName', '?')} decision={event.get('decision')}")
    elif kind == "agent.waiting_approval":
        approval = event.get("approval") or {}
        parts.append(f"{approval.get('toolName', '?')}")
        parts.append(f"approvalId={str(approval.get('approvalId'))[:12]}")
        parts.append(f"fingerprint={str(approval.get('inputFingerprint'))[:12]}")
        parts.append(f"target={clip(approval.get('target'), 120)}")
        parts.append(f"plan={approval.get('planId')}/{approval.get('planStepId')}")
        parts.append(f"input={clip(approval.get('input'), 240)}")
        if approval.get("hostInput"):
            parts.append(f"hostInput={clip(approval['hostInput'], 240)}")
    elif kind in ("agent.approval_expired", "approval.decision"):
        parts.append(f"approvalId={str(event.get('approvalId'))[:12]}")
        if event.get("decision"):
            parts.append(f"decision={event['decision']}")
    elif kind in ("agent.text", "agent.thinking"):
        parts.append(f"delta={clip(event.get('textDelta'), 400)}")
    elif kind == "agent.usage":
        usage = event.get("usage") or {}
        last = usage.get("lastRequest") or {}
        parts.append(
            f"tokens in={usage.get('inputTokens')} out={usage.get('outputTokens')} "
            f"req={clip(last, 200)}"
        )
    elif kind in ("agent.completed", "agent.failed", "agent.cancelled"):
        result = event.get("result") or {}
        parts.append(f"state={result.get('state')}")
        parts.append(f"steps={result.get('steps')} calls={result.get('toolCalls')}")
        if event.get("error"):
            parts.append(f"error={event['error']}")
        if result.get("text"):
            parts.append(f"text={clip(result['text'], 600)}")
    elif kind == "context.loaded":
        parts.append(clip(event, 240))
    return "  ".join(str(part) for part in parts if part)


def print_audit(row: dict) -> None:
    parts = [
        f"{'audit':<22}",
        f"{row.get('status', '?'):<17}",
        f"{row.get('tool_name', '?')}",
        f"target={row.get('target_id')}",
        f"args={','.join(row.get('argument_keys') or [])}",
    ]
    if row.get("approval_status") and row["approval_status"] != "not_required":
        parts.append(f"approval={row['approval_status']}")
    if row.get("error_code"):
        parts.append(f"error={row['error_code']}")
    if row.get("result_summary"):
        parts.append(f"result={clip(row['result_summary'], 160)}")
    print("  ".join(parts), flush=True)


class EventTail:
    def __init__(self, database: Path, audit: Path, since_ordinal: int, audit_offset: int) -> None:
        self.database = database
        self.audit = audit
        self.ordinal = since_ordinal
        self.audit_offset = audit_offset

    def connection(self) -> sqlite3.Connection | None:
        if not self.database.exists():
            return None
        try:
            connection = sqlite3.connect(f"file:{self.database}?mode=ro", uri=True, timeout=1.0)
            connection.row_factory = sqlite3.Row
            connection.execute("PRAGMA busy_timeout=1000")
            return connection
        except sqlite3.Error:
            return None

    def drain(self) -> int:
        emitted = 0
        connection = self.connection()
        if connection is not None:
            try:
                rows = connection.execute(
                    "SELECT ordinal, run_id, event_json FROM conversation_events "
                    "WHERE ordinal > ? ORDER BY ordinal LIMIT 200",
                    (self.ordinal,),
                ).fetchall()
            except sqlite3.Error:
                rows = []
            finally:
                connection.close()
            for row in rows:
                self.ordinal = row["ordinal"]
                try:
                    event = json.loads(row["event_json"])
                except (TypeError, ValueError):
                    continue
                print(f"{clock()}  {describe(event)}", flush=True)
                emitted += 1

        if self.audit.exists():
            try:
                with self.audit.open("rb") as handle:
                    handle.seek(self.audit_offset)
                    chunk = handle.read()
                    self.audit_offset = handle.tell()
            except OSError:
                chunk = b""
            for line in chunk.splitlines():
                if not line.strip():
                    continue
                try:
                    print_audit(json.loads(line))
                    emitted += 1
                except (TypeError, ValueError):
                    continue
        return emitted


def max_ordinal(database: Path) -> int:
    if not database.exists():
        return 0
    try:
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True, timeout=1.0)
        try:
            row = connection.execute("SELECT COALESCE(MAX(ordinal), 0) FROM conversation_events").fetchone()
            return int(row[0])
        finally:
            connection.close()
    except sqlite3.Error:
        return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--backfill", type=int, default=0, help="replay this many earlier events first")
    parser.add_argument("--once", action="store_true", help="drain what exists, then exit")
    parser.add_argument("--interval", type=float, default=0.4)
    parser.add_argument("--data-dir", default="")
    arguments = parser.parse_args()

    root = Path(arguments.data_dir) if arguments.data_dir else data_dir()
    database = root / "agent-state" / "conversations.sqlite"
    audit = root / "audit" / "python-agent-tools.jsonl"
    if not database.exists() and not audit.exists():
        print(f"No Agent data directory at {root}", file=sys.stderr)
        return 2

    latest = max_ordinal(database)
    start_ordinal = max(0, latest - arguments.backfill) if arguments.backfill else latest
    audit_offset = audit.stat().st_size if (arguments.backfill == 0 and audit.exists()) else 0

    print(f"# data dir {root}", flush=True)
    print(f"# watching conversation_events from ordinal {start_ordinal + (0 if arguments.backfill else 0)}"
          f", audit from byte {audit_offset}", flush=True)

    tail = EventTail(database, audit, start_ordinal, audit_offset)
    if arguments.once:
        tail.drain()
        return 0
    try:
        while True:
            tail.drain()
            time.sleep(arguments.interval)
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
