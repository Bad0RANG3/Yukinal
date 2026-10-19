from __future__ import annotations

import asyncio
from contextlib import ExitStack, redirect_stderr
from io import StringIO
import sqlite3
import unittest
from unittest.mock import patch

from yukinal_agent.audit import MemoryAuditSink
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.model_agent import ModelAgentResult
from yukinal_agent.sidecar import PythonSidecar, _PendingApproval, _parse_run_request, _run_fingerprint
from yukinal_agent.tools import ToolRegistry


class RecordingStore(ConversationStore):
    def __init__(self) -> None:
        super().__init__()
        self.finishes: list[tuple[str, str]] = []
        self.fail_finish = False

    def finish(self, run_id: str, status: str) -> None:
        self.finishes.append((run_id, status))
        if self.fail_finish:
            raise sqlite3.OperationalError("private fixture persistence diagnostic")
        super().finish(run_id, status)


class InertAgent:
    def __init__(self, status: str = "succeeded", event_sink=None) -> None:
        self.event_sink = event_sink
        self.status = status
        self.calls = 0
        self.entered = asyncio.Event()
        self.block = False

    def conversation_budget_bytes(self, *_args) -> int:
        return 32 * 1024

    async def run(self, **_kwargs) -> ModelAgentResult:
        self.calls += 1
        self.entered.set()
        if self.block:
            await asyncio.Event().wait()
        if self.event_sink is not None:
            self.event_sink({"type": "agent.text", "runId": _kwargs["run_id"], "textDelta": "inert fixture complete", "at": "2026-10-18T00:00:00Z"})
        return ModelAgentResult("task_fixture", self.status,
            "inert fixture complete" if self.status == "succeeded" else "", (), (), 0, 0,
            error_code=None if self.status == "succeeded" else "model_timeout")


class RunFinalizationTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        self.fixtures: list[tuple[PythonSidecar, RecordingStore]] = []

    async def asyncTearDown(self) -> None:
        for sidecar, store in self.fixtures:
            await sidecar.shutdown()
            store.close()

    def fixture(self):
        store = RecordingStore()
        events: list[dict] = []
        sidecar = PythonSidecar(ToolRegistry(MemoryAuditSink()), agent_version="local-fixture",
            conversation_store=store, notification_sink=lambda _method, event: events.append(event))
        request = _parse_run_request({"runId": "run_fixture", "sessionId": "session_fixture",
            "taskId": "task_fixture", "messageId": "msg_fixture", "prompt": "inert local fixture",
            "target": {"host": "local", "environment": "development"}})
        store.begin(request, _run_fingerprint(request))
        sidecar._admissions[request["messageId"]] = {
            "sessionId": request["sessionId"], "fingerprint": _run_fingerprint(request),
            "runId": request["runId"], "started": True, "completed": False}
        self.fixtures.append((sidecar, store))
        return sidecar, store, request, events

    def launch(self, sidecar, request, agent):
        run_id = request["runId"]
        sidecar._run_sessions[run_id] = request["sessionId"]
        task = asyncio.create_task(sidecar._execute_run(request, run_id, agent))
        sidecar._active_runs[run_id] = task
        return task

    def assert_finished(self, sidecar, store, request, status):
        run_id = request["runId"]
        self.assertEqual(store.finishes, [(run_id, status)])
        self.assertNotIn(run_id, sidecar._active_runs)
        self.assertNotIn(run_id, sidecar._run_sessions)
        self.assertNotIn(request["sessionId"], sidecar._session_locks)
        self.assertIn(run_id, sidecar._completed_runs)
        self.assertTrue(sidecar._admissions[request["messageId"]]["completed"])
        if not store.fail_finish:
            row = store._connection.execute("SELECT status FROM conversation_turns WHERE run_id = ?", (run_id,)).fetchone()
            self.assertEqual(row["status"], status)

    def terminals(self, events):
        return [event for event in events if event["type"] in ("agent.failed", "agent.completed")]

    async def test_preparation_failures_emit_one_failed_and_finalize_before_model(self):
        phases = ("budget", "history", "memory", "coordinator")
        for phase in phases:
            with self.subTest(phase=phase):
                sidecar, store, request, events = self.fixture()
                agent = InertAgent()
                diagnostic = StringIO()
                with ExitStack() as stack, redirect_stderr(diagnostic):
                    error = sqlite3.OperationalError("private fixture persistence diagnostic")
                    if phase == "budget":
                        stack.enter_context(patch.object(agent, "conversation_budget_bytes", side_effect=error))
                    elif phase in ("history", "memory"):
                        stack.enter_context(patch.object(store, "context" if phase == "history" else "memory_context", side_effect=error))
                    else:
                        request["coordination"] = {"fixture": True}
                        stack.enter_context(patch.object(sidecar._coordination, "context", side_effect=error))
                    result = await self.launch(sidecar, request, agent)
                self.assertEqual(result["state"], "failed")
                self.assertEqual(agent.calls, 0)
                self.assertEqual(result["toolCalls"], 0)
                self.assertEqual([(event["type"], event["runId"], event["taskId"]) for event in self.terminals(events)],
                    [("agent.failed", request["runId"], request["taskId"])])
                self.assertNotIn("private fixture", str(self.terminals(events)))
                self.assertNotIn("private fixture", diagnostic.getvalue())
                self.assert_finished(sidecar, store, request, "failed")

    async def test_preparation_cancel_is_cancelled_without_fabricated_context(self):
        sidecar, store, request, events = self.fixture()
        agent = InertAgent()
        with patch.object(agent, "conversation_budget_bytes", side_effect=asyncio.CancelledError):
            result = await self.launch(sidecar, request, agent)
        self.assertEqual(result["state"], "cancelled")
        self.assertNotIn("context", result)
        self.assertEqual(agent.calls, 0)
        self.assertEqual([event["type"] for event in self.terminals(events)], ["agent.completed"])
        self.assertEqual(self.terminals(events)[0]["taskId"], request["taskId"])
        self.assert_finished(sidecar, store, request, "cancelled")

    async def test_queued_cancel_has_task_identity_and_finishes_one_turn(self):
        sidecar, store, request, events = self.fixture()
        agent = InertAgent()
        lock = asyncio.Lock()
        await lock.acquire()
        sidecar._session_locks[request["sessionId"]] = lock
        task = self.launch(sidecar, request, agent)
        await asyncio.sleep(0)
        task.cancel()
        result = await task
        lock.release()
        self.assertEqual(result["state"], "cancelled")
        self.assertEqual(agent.calls, 0)
        self.assertEqual(len(self.terminals(events)), 1)
        self.assertEqual(self.terminals(events)[0]["taskId"], request["taskId"])
        self.assert_finished(sidecar, store, request, "cancelled")

    async def test_active_cancel_keeps_one_cancelled_terminal_and_clears_pending(self):
        sidecar, store, request, events = self.fixture()
        agent = InertAgent()
        agent.block = True
        task = self.launch(sidecar, request, agent)
        await asyncio.wait_for(agent.entered.wait(), timeout=1)
        future = asyncio.get_running_loop().create_future()
        sidecar._pending_approvals["apr_inert"] = _PendingApproval(request["runId"], "checkpoint_inert", "fingerprint_inert", future)
        task.cancel()
        result = await task
        self.assertEqual(result["state"], "cancelled")
        self.assertEqual(len(self.terminals(events)), 1)
        self.assertEqual(self.terminals(events)[0]["taskId"], request["taskId"])
        self.assertNotIn("apr_inert", sidecar._pending_approvals)
        self.assertTrue(future.cancelled())
        self.assert_finished(sidecar, store, request, "cancelled")

    async def test_success_failure_and_timeout_keep_existing_terminal_semantics(self):
        for status in ("succeeded", "failed", "timed_out"):
            with self.subTest(status=status):
                sidecar, store, request, events = self.fixture()
                result = await self.launch(sidecar, request, InertAgent(status))
                expected = "completed" if status == "succeeded" else "failed"
                self.assertEqual(result["state"], expected)
                self.assertEqual([event["type"] for event in self.terminals(events)], ["agent.completed" if status == "succeeded" else "agent.failed"])
                self.assertIn("context", result)
                self.assert_finished(sidecar, store, request, status)

    async def test_terminal_record_failure_does_not_suppress_or_duplicate_delivery(self):
        sidecar, store, request, events = self.fixture()
        record = store.record_event
        def fail_terminal(run_id, event):
            if event["type"] in ("agent.completed", "agent.failed"):
                raise sqlite3.OperationalError("private fixture persistence diagnostic")
            return record(run_id, event)
        diagnostics = StringIO()
        with patch.object(store, "record_event", side_effect=fail_terminal), redirect_stderr(diagnostics):
            result = await self.launch(sidecar, request, InertAgent())
        self.assertEqual(result["state"], "completed")
        self.assertEqual(len(self.terminals(events)), 1)
        self.assertNotIn("private fixture", diagnostics.getvalue())
        self.assert_finished(sidecar, store, request, "succeeded")

    async def test_failed_finish_releases_reservations_without_replaying(self):
        sidecar, store, request, events = self.fixture()
        store.fail_finish = True
        diagnostics = StringIO()
        with redirect_stderr(diagnostics):
            result = await self.launch(sidecar, request, InertAgent())
        self.assertEqual(result["state"], "completed")
        self.assertEqual(len(self.terminals(events)), 1)
        self.assertNotIn("private fixture", diagnostics.getvalue())
        self.assert_finished(sidecar, store, request, "succeeded")

    async def test_sink_failure_after_receipt_does_not_create_a_second_terminal(self):
        sidecar, store, request, events = self.fixture()
        def sink(_method, event):
            events.append(event)
            if event["type"] in ("agent.completed", "agent.failed"):
                raise OSError("private fixture delivery diagnostic")
        sidecar.set_notification_sink(sink)
        diagnostics = StringIO()
        with redirect_stderr(diagnostics):
            result = await self.launch(sidecar, request, InertAgent())
        self.assertEqual(result["state"], "completed")
        self.assertEqual([event["type"] for event in self.terminals(events)], ["agent.completed"])
        self.assertNotIn("private fixture", diagnostics.getvalue())
        self.assert_finished(sidecar, store, request, "succeeded")

    async def test_text_event_failure_finalizes_failed_instead_of_succeeded(self):
        sidecar, store, request, events = self.fixture()
        record = store.record_event
        def fail_text(run_id, event):
            if event["type"] == "agent.text":
                raise sqlite3.OperationalError("private fixture persistence diagnostic")
            return record(run_id, event)
        with patch.object(store, "record_event", side_effect=fail_text):
            result = await self.launch(sidecar, request, InertAgent(event_sink=sidecar._emit))
        self.assertEqual(result["state"], "failed")
        self.assertEqual([event["type"] for event in self.terminals(events)], ["agent.failed"])
        self.assert_finished(sidecar, store, request, "failed")


if __name__ == "__main__":
    unittest.main()
