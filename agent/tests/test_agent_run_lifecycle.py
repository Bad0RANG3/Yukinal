"""Permanent-hang regressions for the Python Agent Node run lifecycle.

Every test here asserts the two halves of a closed run at once: the terminal event
reaching the notification sink (the only thing that clears the desktop's `running`
state) *and* the terminal accounting on disk and in memory. Asserting only
`agent.started` would pass on a sidecar that never lets the UI leave `thinking`.
"""

from __future__ import annotations

import asyncio
import unittest
from typing import Any
from unittest import mock

from langchain_core.messages import AIMessage

from yukinal_agent import MemoryAuditSink, ToolRegistry
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.sidecar import RPC_INVALID_PARAMS, PythonSidecar

TERMINAL_TYPES = ("agent.completed", "agent.failed")


class ScriptedModel:
    """A model that counts entry, so a test can prove its coroutine never ran."""

    def __init__(self, answer: str = "Verified.") -> None:
        self.answer = answer
        self.entered = 0
        self.seen: list[list[Any]] = []

    def bind_tools(self, _tools: Any, **_kwargs: Any) -> ScriptedModel:
        return self

    def bind(self, **_kwargs: Any) -> ScriptedModel:
        return self

    async def ainvoke(self, messages: list[Any]) -> AIMessage:
        self.entered += 1
        self.seen.append(messages)
        return AIMessage(content=self.answer)


class LocalSidecar(PythonSidecar):
    def __init__(self, model: ScriptedModel, store: ConversationStore) -> None:
        self.model = model
        self.events: list[dict[str, Any]] = []
        # Every *attempt* the sidecar makes at the sink, including the ones this
        # transport then refuses. `events` holds deliveries; `attempts` holds offers,
        # and the difference between the two is the whole point of the at-most-once
        # guards: a run may offer its terminal event again, but must never deliver twice.
        self.attempts: list[dict[str, Any]] = []
        # Lets one test make the notification sink fail for a single event type, which
        # is how the prologue's `_emit` ("agent.started") is made to raise.
        self.fail_event_type: str | None = None
        self.fail_event_remaining = 0
        super().__init__(
            ToolRegistry(MemoryAuditSink()),
            conversation_store=store,
            notification_sink=self._notify,
        )
        self.handle({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "1.0", "clientVersion": "test", "dataDir": ""},
        })

    def _notify(self, _method: str, event: dict[str, Any]) -> None:
        # A sinking transport fails *before* the event is handed over, so an event that
        # failed here is genuinely still waiting for delivery. `fail_event_remaining`
        # distinguishes a transport that is down from one that recovers.
        run_id = event.get("runId")
        self.attempts.append({
            "type": event.get("type"),
            "runId": run_id,
            # Snapshot of the two ledgers *at the moment of the offer*. It is what lets a
            # test assert that a delivered terminal is remembered only afterwards, and
            # that a repeat offer is refused before the sink is ever called.
            "armed": isinstance(run_id, str) and run_id in self._terminal_emitted,
            "stashed": isinstance(run_id, str) and run_id in self._undelivered_terminals,
        })
        if self.fail_event_remaining > 0 and event.get("type") == self.fail_event_type:
            self.fail_event_remaining -= 1
            raise RuntimeError("the notification transport is closed")
        self.events.append(event)

    async def _run_provider_model(self, _provider: Any, _model: Any) -> tuple[Any, int]:
        return self.model, 120_000

    async def rpc(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        return await self.handle_async({"jsonrpc": "2.0", "id": 2, "method": method, "params": params})


def run_payload(
    run_id: str,
    *,
    message_id: str,
    resume: bool,
    delivery: str = "async",
    session: str = "session-1",
) -> dict[str, Any]:
    return {
        "runId": run_id,
        "sessionId": session,
        "messageId": message_id,
        "resume": resume,
        "prompt": "Verify the deployment.",
        "delivery": delivery,
        "model": "scripted",
        "persona": {"id": "reviewer", "label": "Reviewer", "systemPrompt": "Only this active role: reviewer"},
        "participant": {"id": "reviewer", "label": "Reviewer", "kind": "agent"},
    }


def terminal_events(sidecar: LocalSidecar, run_id: str) -> list[dict[str, Any]]:
    return [
        event for event in sidecar.events
        if event.get("runId") == run_id and event.get("type") in TERMINAL_TYPES
    ]


def terminal_deliveries(sidecar: LocalSidecar, run_id: str) -> int:
    """How many terminal events the sink actually accepted for this run.

    "The event exists" and "the event was delivered exactly once" are different claims,
    and only the count can tell an at-most-once guard from a code path that happens to
    run once. Every test that names "exactly once" has to assert this number.
    """
    return len(terminal_events(sidecar, run_id))


def terminal_attempts(sidecar: LocalSidecar, run_id: str) -> list[dict[str, Any]]:
    """Every offer of a terminal event for this run, refused ones included.

    This is where an at-most-once guard becomes observable: a guard that works keeps
    the count at one, while a deleted guard lets the extra offer reach the sink.
    """
    return [
        attempt for attempt in sidecar.attempts
        if attempt["runId"] == run_id and attempt["type"] in TERMINAL_TYPES
    ]


def recorded_events(sidecar: LocalSidecar, run_id: str) -> int:
    """Durable audit rows for a run. `_emit` writes one per *attempt*, not per delivery."""
    return sidecar._conversations._connection.execute(
        "SELECT count(*) FROM conversation_events WHERE run_id = ?", (run_id,)
    ).fetchone()[0]


def turn_status(sidecar: LocalSidecar, run_id: str) -> str | None:
    row = sidecar._conversations._connection.execute(
        "SELECT status FROM conversation_turns WHERE run_id = ?", (run_id,)
    ).fetchone()
    return row["status"] if row is not None else None


async def settle(sidecar: LocalSidecar, run_id: str, attempts: int = 500) -> None:
    """Give the run task and its done callbacks a chance to run to completion."""
    for _ in range(attempts):
        if terminal_events(sidecar, run_id):
            return
        await asyncio.sleep(0)


class RunLifecycleTests(unittest.IsolatedAsyncioTestCase):
    async def test_stop_before_the_first_step_still_closes_and_notifies_the_run(self) -> None:
        # `Task.cancel()` on a coroutine that has not taken its first step settles the
        # task without executing one statement of its body, so `try`/`except`/`finally`
        # inside the run never run and the run used to be stuck `running` forever.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        admitted = await sidecar.rpc("agent.run.start", run_payload("run-pre-step", message_id="msg-1", resume=False))
        self.assertEqual(admitted["result"], {"runId": "run-pre-step", "started": False})

        started = await sidecar.rpc("agent.run.start", run_payload("run-pre-step", message_id="msg-1", resume=True))
        self.assertTrue(started["result"]["started"])
        # Precondition: this is a pre-step cancellation, not a mid-run one.
        self.assertEqual(model.entered, 0)

        stopped = await sidecar.rpc("agent.run.stop", {"runId": "run-pre-step"})
        self.assertEqual(stopped["result"], {"stopped": True})
        await settle(sidecar, "run-pre-step")

        self.assertEqual(model.entered, 0)
        terminals = terminal_events(sidecar, "run-pre-step")
        self.assertEqual([event["type"] for event in terminals], ["agent.completed"])
        self.assertEqual(terminals[0]["result"]["state"], "cancelled")
        self.assertEqual(turn_status(sidecar, "run-pre-step"), "cancelled")
        self.assertIn("run-pre-step", sidecar._completed_runs)
        self.assertNotIn("run-pre-step", sidecar._active_runs)
        self.assertNotIn("run-pre-step", sidecar._run_sessions)
        self.assertTrue(sidecar._admissions["msg-1"]["started"])
        self.assertTrue(sidecar._admissions["msg-1"]["completed"])
        store.close()

    async def test_stopped_before_the_first_step_still_emits_exactly_one_terminal_event(self) -> None:
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-once", message_id="msg-once", resume=False))
        await sidecar.rpc("agent.run.start", run_payload("run-once", message_id="msg-once", resume=True))
        await sidecar.rpc("agent.run.stop", {"runId": "run-once"})
        await settle(sidecar, "run-once")
        # A duplicate `agent.completed` would double-apply the same result in the UI.
        self.assertEqual(
            [event["type"] for event in sidecar.events if event.get("runId") == "run-once" and event["type"] in TERMINAL_TYPES],
            ["agent.completed"],
        )
        store.close()

    async def test_a_synchronous_run_stopped_before_the_first_step_still_answers(self) -> None:
        # Two changes meet here. `delivery: "sync"` makes `agent.run.start` await the run
        # task, and a task cancelled before its first step raises `CancelledError` out of
        # that await — which `handle_async` cannot turn into an error frame because
        # `asyncio.CancelledError` is a `BaseException`. The host then waited out its
        # 15-minute `agent_run_start` timeout for a response that could never be written.
        # Cancelling the task right after creation reproduces the real race in the host:
        # `respond(start)` is already suspended on `await task` by the time the stop frame
        # is read, so the run never takes its first step.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-sync", message_id="msg-sync", resume=False))

        real_create_task = asyncio.create_task
        created: list[asyncio.Task[Any]] = []

        def cancel_before_first_step(coro: Any, **kwargs: Any) -> asyncio.Task[Any]:
            task = real_create_task(coro, **kwargs)
            if getattr(getattr(coro, "cr_code", None), "co_name", None) == "_execute_run":
                created.append(task)
                task.cancel()
            return task

        with mock.patch.object(asyncio, "create_task", cancel_before_first_step):
            response = await asyncio.wait_for(
                sidecar.rpc(
                    "agent.run.start",
                    run_payload("run-sync", message_id="msg-sync", resume=True, delivery="sync"),
                ),
                timeout=5,
            )
        self.assertEqual(len(created), 1)
        self.assertTrue(created[0].cancelled(), "the run must still end cancelled")
        self.assertNotIn("error", response)
        self.assertEqual(response["result"]["result"]["state"], "cancelled")
        self.assertEqual(model.entered, 0)

        terminals = terminal_events(sidecar, "run-sync")
        self.assertEqual([event["type"] for event in terminals], ["agent.completed"])
        self.assertEqual(terminals[0]["result"]["state"], "cancelled")
        self.assertEqual(turn_status(sidecar, "run-sync"), "cancelled")
        self.assertIn("run-sync", sidecar._completed_runs)
        self.assertTrue(sidecar._admissions["msg-sync"]["completed"])
        store.close()

    async def test_context_prologue_failure_still_fails_the_run(self) -> None:
        # The prologue (`context`/`memory_context`/`context.loaded`/`agent.started`) ran
        # outside the `try`, so its failure escaped the task with no terminal event and
        # no `_completed_runs` entry: disk said `running`, the UI said `thinking`.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-prologue", message_id="msg-2", resume=False))

        def broken_context(*_args: Any, **_kwargs: Any) -> Any:
            raise RuntimeError("context projection failed")

        sidecar._conversations.context = broken_context  # type: ignore[method-assign]
        response = await sidecar.rpc(
            "agent.run.start", run_payload("run-prologue", message_id="msg-2", resume=True, delivery="sync")
        )
        self.assertNotIn("error", response)
        self.assertEqual(response["result"]["result"]["state"], "failed")

        terminals = terminal_events(sidecar, "run-prologue")
        self.assertEqual([event["type"] for event in terminals], ["agent.failed"])
        self.assertEqual(turn_status(sidecar, "run-prologue"), "failed")
        self.assertIn("run-prologue", sidecar._completed_runs)
        self.assertNotIn("run-prologue", sidecar._active_runs)
        self.assertTrue(sidecar._admissions["msg-2"]["completed"])
        store.close()

    async def test_stop_while_the_model_is_thinking_still_ends_cancelled(self) -> None:
        # The counterpart of the pre-step test: cancelling a run that *has* started must
        # still take effect (the model call is abandoned) and still close the run.
        store = ConversationStore()
        entered = asyncio.Event()
        release = asyncio.Event()

        class WaitingModel(ScriptedModel):
            async def ainvoke(self, messages: list[Any]) -> AIMessage:
                self.entered += 1
                self.seen.append(messages)
                entered.set()
                await release.wait()
                return AIMessage(content="This answer must never be delivered.")

        model = WaitingModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-mid", message_id="msg-9", resume=False))
        started = await sidecar.rpc("agent.run.start", run_payload("run-mid", message_id="msg-9", resume=True))
        self.assertTrue(started["result"]["started"])
        await asyncio.wait_for(entered.wait(), timeout=5)
        self.assertEqual(model.entered, 1)

        stopped = await sidecar.rpc("agent.run.stop", {"runId": "run-mid"})
        self.assertEqual(stopped["result"], {"stopped": True})
        await settle(sidecar, "run-mid")

        terminals = terminal_events(sidecar, "run-mid")
        self.assertEqual([event["type"] for event in terminals], ["agent.completed"])
        self.assertEqual(terminals[0]["result"]["state"], "cancelled")
        self.assertEqual(terminals[0]["result"]["text"], "")
        self.assertEqual(turn_status(sidecar, "run-mid"), "cancelled")
        self.assertIn("run-mid", sidecar._completed_runs)
        self.assertNotIn("run-mid", sidecar._active_runs)
        self.assertNotIn("run-mid", sidecar._run_sessions)
        self.assertTrue(sidecar._admissions["msg-9"]["completed"])
        # A stopped run must not keep the session unresetable, which is what the stale
        # `_run_sessions` entry used to do.
        reset = await sidecar.rpc("agent.context.reset", {"sessionId": "session-1"})
        self.assertEqual(reset["result"], {"reset": True})
        store.close()

    async def test_prologue_started_notification_failure_still_fails_the_run(self) -> None:
        # The prologue's own `_emit` ("agent.started") was outside the `try` too, so a
        # failing notification transport killed the run before any terminal accounting.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-started", message_id="msg-8", resume=False))
        sidecar.fail_event_type = "agent.started"
        sidecar.fail_event_remaining = 1
        response = await sidecar.rpc(
            "agent.run.start", run_payload("run-started", message_id="msg-8", resume=True, delivery="sync")
        )
        self.assertNotIn("error", response)
        self.assertEqual(response["result"]["result"]["state"], "failed")
        self.assertEqual(model.entered, 0)

        terminals = terminal_events(sidecar, "run-started")
        self.assertEqual([event["type"] for event in terminals], ["agent.failed"])
        self.assertEqual(turn_status(sidecar, "run-started"), "failed")
        self.assertIn("run-started", sidecar._completed_runs)
        self.assertNotIn("run-started", sidecar._active_runs)
        self.assertNotIn("run-started", sidecar._run_sessions)
        self.assertTrue(sidecar._admissions["msg-8"]["completed"])
        store.close()

    async def test_audit_write_failure_does_not_lose_the_run(self) -> None:
        # The first durable write after `begin` is `context.loaded`. When it failed the
        # run vanished before `agent.started` was even announced.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-audit", message_id="msg-3", resume=False))

        original = sidecar._conversations.record_event

        def flaky_record_event(run_id: str, event: dict[str, Any]) -> None:
            if event.get("type") == "context.loaded":
                raise RuntimeError("database is locked")
            original(run_id, event)

        sidecar._conversations.record_event = flaky_record_event  # type: ignore[method-assign]
        response = await sidecar.rpc(
            "agent.run.start", run_payload("run-audit", message_id="msg-3", resume=True, delivery="sync")
        )
        self.assertNotIn("error", response)
        self.assertEqual(response["result"]["result"]["state"], "completed")

        terminals = terminal_events(sidecar, "run-audit")
        self.assertEqual([event["type"] for event in terminals], ["agent.completed"])
        self.assertEqual(turn_status(sidecar, "run-audit"), "succeeded")
        self.assertIn("run-audit", sidecar._completed_runs)
        self.assertTrue(sidecar._admissions["msg-3"]["completed"])
        self.assertEqual(model.entered, 1)
        store.close()

    async def test_terminal_audit_write_failure_still_delivers_the_terminal_event(self) -> None:
        # `_emit` wrote the audit row before notifying, so a failing store consumed
        # `agent.completed`/`agent.failed` and the UI stayed in `running` while the
        # turn on disk was already `succeeded`.
        store = ConversationStore()
        model = ScriptedModel(answer="Verified.")
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-terminal", message_id="msg-4", resume=False))

        original = sidecar._conversations.record_event

        def flaky_record_event(run_id: str, event: dict[str, Any]) -> None:
            if event.get("type") in TERMINAL_TYPES:
                raise RuntimeError("conversation store is not writable")
            original(run_id, event)

        sidecar._conversations.record_event = flaky_record_event  # type: ignore[method-assign]
        response = await sidecar.rpc(
            "agent.run.start", run_payload("run-terminal", message_id="msg-4", resume=True, delivery="sync")
        )
        self.assertNotIn("error", response)
        self.assertEqual(response["result"]["result"]["state"], "completed")

        terminals = terminal_events(sidecar, "run-terminal")
        self.assertEqual([event["type"] for event in terminals], ["agent.completed"])
        self.assertEqual(terminals[0]["result"]["state"], "completed")
        self.assertEqual(turn_status(sidecar, "run-terminal"), "succeeded")
        self.assertIn("run-terminal", sidecar._completed_runs)
        self.assertTrue(sidecar._admissions["msg-4"]["completed"])
        store.close()

    async def test_terminal_event_lost_to_a_failing_sink_is_delivered_once_the_run_ends(self) -> None:
        # If the notification transport drops the terminal event, the run's done-callback
        # still has the durable state and the receipt, so the last hook of the run has to
        # hand the event over again instead of leaving the UI in `running`.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-sink", message_id="msg-7", resume=False))
        sidecar.fail_event_type = "agent.completed"
        sidecar.fail_event_remaining = 1
        response = await sidecar.rpc(
            "agent.run.start", run_payload("run-sink", message_id="msg-7", resume=True, delivery="sync")
        )
        self.assertNotIn("error", response)
        self.assertEqual(response["result"]["result"]["state"], "completed")
        self.assertEqual([event["type"] for event in terminal_events(sidecar, "run-sink")], ["agent.completed"])
        self.assertEqual(turn_status(sidecar, "run-sink"), "succeeded")
        self.assertIn("run-sink", sidecar._completed_runs)
        self.assertTrue(sidecar._admissions["msg-7"]["completed"])
        self.assertEqual(sidecar._undelivered_terminals, {})
        store.close()

    async def test_rejected_admission_stays_retryable_after_begin_fails(self) -> None:
        # `admission["started"]` used to be set before `begin`, so a rejected attempt
        # left a `duplicate: true` receipt for a run that never opened a turn.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-begin", message_id="msg-5", resume=False))
        self.assertFalse(sidecar._admissions["msg-5"]["started"])

        original = sidecar._conversations.begin

        def flaky_begin(*_args: Any, **_kwargs: Any) -> None:
            raise ValueError("runId already belongs to a different conversation turn")

        sidecar._conversations.begin = flaky_begin  # type: ignore[method-assign]
        rejected = await sidecar.rpc("agent.run.start", run_payload("run-begin", message_id="msg-5", resume=True))
        self.assertEqual(rejected["error"]["code"], RPC_INVALID_PARAMS)
        self.assertFalse(sidecar._admissions["msg-5"]["started"])
        self.assertNotIn("run-begin", sidecar._completed_runs)
        self.assertEqual(sidecar._run_sessions, {})
        self.assertEqual(sidecar._active_runs, {})

        sidecar._conversations.begin = original  # type: ignore[method-assign]
        retried = await sidecar.rpc(
            "agent.run.start", run_payload("run-begin", message_id="msg-5", resume=True, delivery="sync")
        )
        self.assertNotIn("error", retried)
        self.assertTrue(retried["result"]["started"])
        self.assertEqual(retried["result"]["result"]["state"], "completed")
        self.assertEqual(turn_status(sidecar, "run-begin"), "succeeded")
        self.assertEqual(
            [event["type"] for event in terminal_events(sidecar, "run-begin")], ["agent.completed"]
        )
        store.close()

    async def test_policy_id_is_rejected_before_any_run_state_exists(self) -> None:
        # This node declares `permissionPolicyIds: []`, so an explicit selection can
        # only be rejected. The rejection must be total: no admission, no session and
        # no turn may survive for a run that never started.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        payload = {**run_payload("run-policy", message_id="msg-6", resume=False), "policyId": "policy.local"}
        response = await sidecar.rpc("agent.run.start", payload)
        self.assertEqual(response["error"]["code"], RPC_INVALID_PARAMS)
        self.assertIn("not supported", response["error"]["message"])
        self.assertEqual(sidecar._admissions, {})
        self.assertEqual(sidecar._run_sessions, {})
        self.assertEqual(sidecar._active_runs, {})
        self.assertEqual(sidecar._completed_runs, {})
        self.assertEqual(store._connection.execute("SELECT count(*) FROM conversation_turns").fetchone()[0], 0)
        self.assertEqual(store._connection.execute("SELECT count(*) FROM conversation_sessions").fetchone()[0], 0)
        described = sidecar.handle({"jsonrpc": "2.0", "id": 3, "method": "system.describe"})["result"]
        self.assertEqual(described["permissionPolicyIds"], [])
        store.close()

    async def test_a_second_terminal_event_for_the_same_run_never_reaches_the_sink(self) -> None:
        # `_terminal_emitted` is the at-most-once guard: a run's terminal event can be
        # offered more than once — the session run emits it, the done-callback re-checks
        # the run, and `_retry_terminal_delivery` re-offers a stashed event — but the UI
        # may only ever receive one, because a second terminal event double-applies the
        # run's outcome. Nothing else pins that: the pre-step test passes because the
        # call graph happens to reach the emitter once, not because a second offer was
        # refused. This drives the second offer, and the stale-stash re-entry the
        # done-callback really performs, through the guard.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-twice", message_id="msg-twice", resume=False))
        response = await sidecar.rpc(
            "agent.run.start", run_payload("run-twice", message_id="msg-twice", resume=True, delivery="sync")
        )
        self.assertNotIn("error", response)
        self.assertEqual(response["result"]["result"]["state"], "completed")
        self.assertEqual(terminal_deliveries(sidecar, "run-twice"), 1)
        self.assertIn("run-twice", sidecar._terminal_emitted)

        # Everything the guard promises to leave alone, photographed before it is tested.
        armed = set(sidecar._terminal_emitted)
        stashed = dict(sidecar._undelivered_terminals)
        recorded = recorded_events(sidecar, "run-twice")
        fingerprint = sidecar._completed_runs["run-twice"]
        delivered = terminal_events(sidecar, "run-twice")

        # A second emitter deciding the same run is over. The event contradicts the
        # delivered one, so its arrival would flip the run to `failed` after the UI had
        # already rendered `completed`.
        sidecar._emit_terminal({
            "type": "agent.failed",
            "runId": "run-twice",
            "error": "agent_run_failed",
            "at": "2026-01-01T00:00:00Z",
        })
        # And the hook the guard actually protects: the run's own done-callback finding a
        # stashed event for a run whose terminal event it has already handed over.
        sidecar._undelivered_terminals["run-twice"] = {
            "type": "agent.completed",
            "runId": "run-twice",
            "result": {"runId": "run-twice", "state": "cancelled", "text": "", "steps": 0, "toolCalls": 0},
            "at": "2026-01-01T00:00:00Z",
        }
        sidecar._retry_terminal_delivery("run-twice")

        self.assertEqual(terminal_deliveries(sidecar, "run-twice"), 1)
        self.assertEqual([event["type"] for event in terminal_events(sidecar, "run-twice")], ["agent.completed"])
        self.assertEqual(terminal_events(sidecar, "run-twice"), delivered)
        # The second offer must not even reach the sink: a guard that only deduplicated
        # after delivering would still expose the transport to a duplicate terminal event.
        self.assertEqual(len(terminal_attempts(sidecar, "run-twice")), 1)
        self.assertEqual(sidecar._terminal_emitted, armed)
        self.assertEqual(sidecar._undelivered_terminals, stashed)
        self.assertEqual(recorded_events(sidecar, "run-twice"), recorded)
        self.assertEqual(sidecar._completed_runs["run-twice"], fingerprint)
        self.assertEqual(turn_status(sidecar, "run-twice"), "succeeded")
        self.assertEqual(model.entered, 1)
        store.close()

    async def test_a_terminal_event_is_retried_after_a_failed_delivery_and_recorded_only_then(self) -> None:
        # The other half of the at-most-once pair: a terminal event whose delivery failed
        # is kept in `_undelivered_terminals` and must be handed over again, because the
        # UI only leaves `running` when one of these events arrives. The run is remembered
        # as delivered only by the attempt that succeeded — a ledger written before the
        # delivery would make the retry refuse to send, leaving the UI in `running`
        # forever with a durable turn that says `succeeded`.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-retry", message_id="msg-retry", resume=False))
        sidecar.fail_event_type = "agent.completed"
        sidecar.fail_event_remaining = 1
        response = await sidecar.rpc(
            "agent.run.start", run_payload("run-retry", message_id="msg-retry", resume=True, delivery="sync")
        )
        self.assertNotIn("error", response)
        # The failed delivery is not allowed to fail the run: the run succeeded, only the
        # transport was down.
        self.assertEqual(response["result"]["result"]["state"], "completed")
        await settle(sidecar, "run-retry")

        attempts = terminal_attempts(sidecar, "run-retry")
        # Two offers, one delivery. The count is the point: "the event exists" would also
        # hold for a sidecar that never retried and never closed the UI.
        self.assertEqual([attempt["type"] for attempt in attempts], ["agent.completed", "agent.completed"])
        self.assertEqual(len(attempts), 2)
        self.assertEqual(terminal_deliveries(sidecar, "run-retry"), 1)
        self.assertEqual(turn_status(sidecar, "run-retry"), "succeeded")
        self.assertIn("run-retry", sidecar._completed_runs)
        # The ledger was empty at both offers — when the first was refused and when the
        # retry was made — and holds the run only after the delivery that succeeded.
        self.assertFalse(attempts[0]["armed"])
        self.assertFalse(attempts[1]["armed"])
        self.assertFalse(attempts[0]["stashed"])
        # `_retry_terminal_delivery` pops before re-offering, so the retry is the only
        # chance the event gets and a failing retry cannot loop.
        self.assertFalse(attempts[1]["stashed"])
        self.assertIn("run-retry", sidecar._terminal_emitted)
        self.assertEqual(sidecar._undelivered_terminals, {})
        self.assertEqual(
            [event["type"] for event in terminal_events(sidecar, "run-retry")], ["agent.completed"]
        )
        self.assertTrue(sidecar._admissions["msg-retry"]["completed"])
        store.close()

    async def test_the_done_callback_retries_its_own_terminal_event_after_a_failed_delivery(self) -> None:
        # The pre-step cancellation path emits its terminal event from inside the done
        # callback, so the callback's trailing retry is the only thing standing between a
        # failed delivery and a run the UI never leaves `running` for. That retry is a
        # different line from the one the run body's own stash relies on, and this test is
        # what tells the two apart.
        store = ConversationStore()
        model = ScriptedModel()
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", run_payload("run-cb-retry", message_id="msg-cb", resume=False))
        await sidecar.rpc("agent.run.start", run_payload("run-cb-retry", message_id="msg-cb", resume=True))
        sidecar.fail_event_type = "agent.completed"
        sidecar.fail_event_remaining = 1
        self.assertEqual(model.entered, 0)

        stopped = await sidecar.rpc("agent.run.stop", {"runId": "run-cb-retry"})
        self.assertEqual(stopped["result"], {"stopped": True})
        await settle(sidecar, "run-cb-retry")

        attempts = terminal_attempts(sidecar, "run-cb-retry")
        # The callback tried, the transport refused, and the same hook tried once more.
        self.assertEqual([attempt["type"] for attempt in attempts], ["agent.completed", "agent.completed"])
        self.assertEqual(terminal_deliveries(sidecar, "run-cb-retry"), 1)
        self.assertEqual(model.entered, 0)
        terminals = terminal_events(sidecar, "run-cb-retry")
        self.assertEqual(terminals[0]["result"]["state"], "cancelled")
        self.assertEqual(turn_status(sidecar, "run-cb-retry"), "cancelled")
        self.assertIn("run-cb-retry", sidecar._completed_runs)
        self.assertEqual(sidecar._undelivered_terminals, {})
        store.close()


if __name__ == "__main__":
    unittest.main()
