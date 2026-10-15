from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from yukinal_agent.filesystem_action_ledger import (
    FilesystemActionLedger,
    FilesystemActionLedgerError,
)


class FilesystemActionLedgerTests(unittest.IsolatedAsyncioTestCase):
    async def test_exact_success_is_replayed_from_durable_storage(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "filesystem-actions.sqlite"
            ledger = FilesystemActionLedger(path)
            scope = _scope()

            self.assertIsNone(await ledger.claim(**scope))
            output = {"path": "/etc/app.conf", "bytesWritten": 4}
            await ledger.complete(**scope, output=output)

            reopened = FilesystemActionLedger(path)
            self.assertEqual(await reopened.claim(**scope), output)

    async def test_started_action_is_never_replayed_automatically(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            ledger = FilesystemActionLedger(Path(temporary) / "filesystem-actions.sqlite")
            scope = _scope()
            self.assertIsNone(await ledger.claim(**scope))

            with self.assertRaises(FilesystemActionLedgerError) as raised:
                await ledger.claim(**scope)

            self.assertEqual(raised.exception.code, "action_outcome_unknown")

    async def test_call_id_cannot_be_rebound_to_different_input(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            ledger = FilesystemActionLedger(Path(temporary) / "filesystem-actions.sqlite")
            scope = _scope()
            await ledger.claim(**scope)

            with self.assertRaises(FilesystemActionLedgerError) as raised:
                await ledger.claim(**{**scope, "arguments": {"path": "/etc/other", "content": "x"}})

            self.assertEqual(raised.exception.code, "duplicate_call_id")


def _scope() -> dict[str, object]:
    return {
        "run_id": "run-1",
        "trace_id": "trace-1",
        "call_id": "call-1",
        "tool_name": "filesystem.write",
        "target_id": "srv_stage1",
        "arguments": {"path": "/etc/app.conf", "content": "data"},
        "plan_id": "plan-1",
        "plan_step_id": "step-1",
        "evidence_ids": ("evidence-1",),
    }
