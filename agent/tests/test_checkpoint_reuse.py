"""Checkpoint reuse: the same turn must resolve to the same thread.

These pin the three defects the R4 audit found in the recovery path, each named by
the operator-visible symptom:

* `_prepare_resume_key` used to record only a `runid:` fallback key, so a repeat of
  the same request opened a *second* thread and paused again instead of handing the
  pending approval back.
* `_checkpoint_matches` used to compare the whole message prefix, so any turn whose
  history had been re-projected (a paused turn collapses into one incomplete-turn
  record, memory reorders, compaction moves anchors) was refused with
  `checkpoint_context_mismatch`.
* A finished turn that was repeated minted a fresh `uuid` thread, and the reset path
  only knows the turn-derived ids, so those snapshots — prompt and tool output
  included — could never be collected.
"""
from __future__ import annotations

import sqlite3
import tempfile
import unittest
from pathlib import Path
from typing import Any

from langchain_core.messages import AIMessage, HumanMessage, SystemMessage

from yukinal_agent import MemoryAuditSink, ToolRegistry
from yukinal_agent.checkpoints import open_checkpoint_store
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.model_agent import _budget_binding, _checkpoint_matches
from yukinal_agent.sidecar import (
    PythonSidecar,
    _parse_run_request,
    _run_fingerprint,
    _turn_digest,
)
from test_agent_conversation_context import LocalSidecar, ScriptedModel, request


def _threads(database: Path) -> dict[str, int]:
    """Thread ids and their checkpoint counts, read outside the sidecar's connection."""
    connection = sqlite3.connect(database)
    try:
        rows = connection.execute(
            "SELECT thread_id, count(*) FROM checkpoints GROUP BY thread_id"
        ).fetchall()
    finally:
        connection.close()
    return {str(thread): int(count) for thread, count in rows}


class ResumeKeyTests(unittest.IsolatedAsyncioTestCase):
    """`_prepare_resume_key` must record the turn key, not only a run fallback."""

    def setUp(self) -> None:
        self.store = ConversationStore()
        self.sidecar = PythonSidecar(ToolRegistry(MemoryAuditSink()), conversation_store=self.store)

    def tearDown(self) -> None:
        self.store.close()

    def _admit(self, run_id: str, **overrides: Any) -> dict[str, Any]:
        parsed = _parse_run_request(request(run_id, **overrides))
        self.store.begin(parsed, _run_fingerprint(parsed))
        return parsed

    async def test_the_first_attempt_records_the_turn_key(self) -> None:
        parsed = self._admit("run-first")
        self.sidecar._prepare_resume_key(parsed, "run-first")
        stored = self.store.latest_resume_key(parsed["sessionId"], "run-first")
        self.assertEqual(stored, "turn:" + _turn_digest(parsed))
        self.assertFalse(stored.startswith("runid:"), "a run-derived key cannot locate a turn")

    async def test_repeating_the_request_lands_on_the_same_key(self) -> None:
        first = self._admit("run-first")
        self.sidecar._prepare_resume_key(first, "run-first")
        # The repeat is a new attempt: the UI always sends a fresh runId.
        second = self._admit("run-second")
        self.sidecar._prepare_resume_key(second, "run-second")
        self.assertEqual(
            self.store.latest_resume_key(first["sessionId"], "run-first"),
            self.store.latest_resume_key(second["sessionId"], "run-second"),
        )

    async def test_another_window_gets_its_own_key(self) -> None:
        here = self._admit("run-here")
        there = self._admit("run-there", session="session-2")
        self.sidecar._prepare_resume_key(here, "run-here")
        self.sidecar._prepare_resume_key(there, "run-there")
        self.assertNotEqual(_turn_digest(here), _turn_digest(there))

    async def test_a_different_prompt_is_a_different_turn(self) -> None:
        first = self._admit("run-a", prompt="Plan the database change.")
        second = self._admit("run-b", prompt="Roll the database change back.")
        self.assertNotEqual(_turn_digest(first), _turn_digest(second))

    async def test_an_already_recorded_key_is_not_overwritten(self) -> None:
        parsed = self._admit("run-keep")
        self.store.record_resume_key(parsed["sessionId"], "run-keep", "turn:" + "a" * 64)
        self.sidecar._prepare_resume_key(parsed, "run-keep")
        self.assertEqual(
            self.store.latest_resume_key(parsed["sessionId"], "run-keep"), "turn:" + "a" * 64
        )


class SidecarThreadReuseTests(unittest.IsolatedAsyncioTestCase):
    """Repeating a turn end-to-end keeps one thread instead of leaking snapshots."""

    async def test_repeating_a_turn_reuses_one_checkpoint_thread(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "agent-state" / "langgraph-checkpoints.sqlite"
            store = ConversationStore(Path(directory) / "conversation.sqlite")
            try:
                async with open_checkpoint_store(directory) as saver:
                    model = ScriptedModel([AIMessage(content="First answer."), AIMessage(content="Second answer.")])
                    sidecar = LocalSidecar(model, store, checkpointer=saver)
                    first = await sidecar.rpc("agent.run.start", request("run-first"))
                    self.assertEqual(first["result"]["result"]["state"], "completed")
                    second = await sidecar.rpc("agent.run.start", request("run-second"))
                    self.assertEqual(second["result"]["result"]["state"], "completed")
                    threads = _threads(database)
                    # Before the fix this was two threads: the repeat resolved to a
                    # fresh `runid:` key and a fresh uuid thread was minted on top.
                    self.assertEqual(len(threads), 1, threads)
                    await sidecar.rpc("agent.context.reset", {"sessionId": "session-1"})
                    self.assertEqual(_threads(database), {}, "reset must be able to collect it")
            finally:
                store.close()


class CheckpointMatchTests(unittest.TestCase):
    """Only the authorization fields and the turn itself may gate a restore."""

    @staticmethod
    def _initial(history: list[Any], prompt: str = "Write x.") -> dict[str, Any]:
        return {
            "tenant_id": "local", "user_id": "session-1", "task_id": "task-1",
            "allowed_tools": ["ops.write"], "allowed_target_ids": ["srv_1"],
            "host_target": {"host": "remote", "serverId": "srv_1", "environment": "development"},
            "host_task_id": None, "checkpoint_binding": "binding-1",
            "budget_binding": {"max_tool_calls": 8},
            "goal_loop_enabled": False,
            "messages": [SystemMessage(content="system"), *history, HumanMessage(content=prompt)],
        }

    def _saved(self, initial: dict[str, Any], *, appended: list[Any] | None = None) -> dict[str, Any]:
        return {**initial, "messages": [*initial["messages"], *(appended or [])]}

    def test_grown_history_prefix_does_not_block_the_restore(self) -> None:
        saved = self._saved(self._initial([], prompt="Write x."), appended=[
            AIMessage(content="", tool_calls=[{"name": "ops.write", "args": {"value": "x"},
                                               "id": "call-1", "type": "tool_call"}]),
        ])
        # The repeat arrives with the paused turn already projected as history.
        repeated = self._initial([
            HumanMessage(content="Write x."),
            HumanMessage(content='{"historicalIncompleteTurn":"Historical incomplete turn (running)"}'),
        ], prompt="Write x.")
        self.assertTrue(_checkpoint_matches(saved, repeated, resume_key="turn:abc"))

    def test_a_different_prompt_is_refused(self) -> None:
        saved = self._saved(self._initial([], prompt="Write x."))
        self.assertFalse(_checkpoint_matches(saved, self._initial([], prompt="Delete x."), resume_key="turn:abc"))

    def test_a_different_system_prompt_is_refused(self) -> None:
        saved = self._saved(self._initial([]))
        changed = self._initial([])
        changed["messages"][0] = SystemMessage(content="other system")
        self.assertFalse(_checkpoint_matches(saved, changed, resume_key="turn:abc"))

    def test_authorization_drift_is_refused(self) -> None:
        saved = self._saved(self._initial([]))
        for field, value in (
            ("allowed_tools", ["ops.read"]),
            ("allowed_target_ids", ["srv_2"]),
            ("checkpoint_binding", "binding-2"),
            ("budget_binding", {"max_tool_calls": 99}),
            ("goal_loop_enabled", True),
        ):
            with self.subTest(field=field):
                drifted = self._initial([])
                drifted[field] = value
                self.assertFalse(_checkpoint_matches(saved, drifted, resume_key="turn:abc"))

    def test_the_turn_key_freezes_the_attempt_identity(self) -> None:
        saved = self._saved(self._initial([]))
        other_attempt = {**self._initial([]), "task_id": "task-2"}
        # With a turn key the durable task identity is covered by `checkpoint_binding`,
        # so a new attempt id must not refuse the restore.
        self.assertTrue(_checkpoint_matches(saved, other_attempt, resume_key="turn:abc"))
        # Without one the thread is located by the attempt, so it must match.
        self.assertFalse(_checkpoint_matches(saved, other_attempt))

    def test_an_empty_saved_state_is_not_a_match(self) -> None:
        self.assertFalse(_checkpoint_matches({"messages": []}, self._initial([]), resume_key="turn:abc"))


class BudgetBindingTests(unittest.TestCase):
    def test_inferred_output_budget_is_not_bound(self) -> None:
        from yukinal_agent.model_agent import AgentBudget

        binding = _budget_binding(AgentBudget(max_model_output_tokens=2048))
        self.assertNotIn("max_model_output_tokens", binding)
        # The declared tool-call ceiling is an authorization, so it stays bound.
        self.assertIn("max_tool_calls", binding)
        self.assertNotEqual(binding, _budget_binding(AgentBudget(max_tool_calls=1)))


if __name__ == "__main__":
    unittest.main(verbosity=2)
