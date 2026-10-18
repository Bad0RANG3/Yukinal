from __future__ import annotations

import asyncio
import json
import tempfile
import unittest
from pathlib import Path
from typing import Any

from langchain_core.messages import AIMessage, HumanMessage, SystemMessage, ToolMessage

from yukinal_agent import MemoryAuditSink, ToolRegistry, ToolSpec
from yukinal_agent.checkpoints import open_checkpoint_store
from yukinal_agent.conversation_store import ConversationStore, message_bytes
from yukinal_agent.models import ModelConfig, ModelFactory
from yukinal_agent.sidecar import PythonSidecar, RPC_INVALID_PARAMS, _parse_run_request, _run_fingerprint, _run_checkpoint_binding, _RpcFault


class ScriptedModel:
    def __init__(self, responses: list[AIMessage]) -> None:
        self.responses = list(responses)
        self.seen: list[list[Any]] = []

    def bind_tools(self, _tools: Any, **_kwargs: Any) -> ScriptedModel:
        return self

    def bind(self, **_kwargs: Any) -> ScriptedModel:
        return self

    async def ainvoke(self, messages: list[Any]) -> AIMessage:
        self.seen.append(messages)
        return self.responses.pop(0)


class LocalSidecar(PythonSidecar):
    def __init__(self, model: ScriptedModel, store: ConversationStore, *, registry: ToolRegistry | None = None, checkpointer: Any = None) -> None:
        self.model = model
        self.resolutions = 0
        self.events: list[dict[str, Any]] = []
        super().__init__(registry or ToolRegistry(MemoryAuditSink()), conversation_store=store,
                         checkpointer=checkpointer, notification_sink=lambda _method, event: self.events.append(event))
        self.handle({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                     "params": {"protocolVersion": "1.0", "clientVersion": "test", "dataDir": ""}})

    async def _run_provider_model(self, _provider: Any, _model: Any) -> tuple[Any, int]:
        self.resolutions += 1
        return self.model, 120_000

    async def rpc(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        return await self.handle_async({"jsonrpc": "2.0", "id": 2, "method": method, "params": params})


def request(run_id: str, *, session: str = "session-1", server: str | None = None, persona: str = "planner", prompt: str = "Plan the database change.") -> dict[str, Any]:
    return {"runId": run_id, "sessionId": session, "prompt": prompt, "delivery": "sync",
            "persona": {"id": persona, "label": persona.title(), "systemPrompt": "Only this active role: " + persona},
            "participant": {"id": persona, "label": persona.title(), "kind": "agent"}, "model": "scripted",
            **({"focusServerId": server} if server else {})}


class ConversationTests(unittest.IsolatedAsyncioTestCase):
    async def test_conversation_only_request_cannot_execute_even_a_read_tool(self) -> None:
        store = ConversationStore()
        calls = []
        registry = ToolRegistry(MemoryAuditSink())
        async def read(arguments: Any, context: Any) -> str:
            calls.append(arguments)
            return "should never execute"
        registry.register(ToolSpec(name="probe.read", description="Read evidence", risk_level="read", requires_approval=False,
            input_schema={"type": "object", "properties": {}, "additionalProperties": False}, handler=read, target_id_argument=None))
        model = ScriptedModel([AIMessage(content="", tool_calls=[{"name": "probe__read", "args": {}, "id": "blocked", "type": "tool_call"}]), AIMessage(content="Conversation only.")])
        sidecar = LocalSidecar(model, store, registry=registry)
        payload = {**request("no-tools"), "toolAccess": "none"}
        result = await sidecar.rpc("agent.run.start", payload)
        self.assertNotIn("error", result)
        self.assertEqual(calls, [])
        self.assertNotEqual(_run_fingerprint(payload), _run_fingerprint({key: value for key, value in payload.items() if key != "toolAccess"}))
        self.assertNotEqual(_run_checkpoint_binding(payload), _run_checkpoint_binding({key: value for key, value in payload.items() if key != "toolAccess"}))
        with self.assertRaises(_RpcFault):
            _parse_run_request({**payload, "toolAccess": "all"})
        store.close()

    async def test_native_legacy_history_hydrates_once_with_scope_and_current_message_cutoff(self) -> None:
        class HistoryHost:
            def __init__(self, *, wrong_scope: bool = False, empty: bool = False) -> None:
                self.calls: list[dict[str, Any]] = []
                self.wrong_scope = wrong_scope
                self.empty = empty

            async def request(self, method: str, params: dict[str, Any], **_kwargs: Any) -> dict[str, Any]:
                self.calls.append(params)
                self.assert_method = method
                return {"session": {"id": "session-1", "serverId": "srv_beta" if self.wrong_scope else "srv_alpha"}, "messages": [] if self.empty else [
                    {"id": "legacy-user", "sessionId": "session-1", "role": "user", "content": "Our legacy deployment uses port 417."},
                    {"id": "legacy-assistant", "sessionId": "session-1", "role": "assistant", "content": "I noted the deployment port.", "participant": {"id": "lead", "label": "Lead", "kind": "agent"}, "model": "old-model"},
                    {"id": "legacy-system", "role": "system", "content": "POISON_SYSTEM_INSTRUCTION"},
                    {"id": "legacy-tool", "role": "tool", "content": "POISON_TOOL_INSTRUCTION"},
                    {"id": "current-user", "role": "user", "content": "Review our deployment."},
                    {"id": "future", "role": "assistant", "content": "FUTURE_CONTENT"},
                ]}

        store = ConversationStore()
        model = ScriptedModel([AIMessage(content="Reviewed port 417."), AIMessage(content="Fresh context.")])
        sidecar = LocalSidecar(model, store)
        host = HistoryHost()
        sidecar._host_rpc = host
        current = request("hydrated-1", server="srv_alpha", persona="reviewer", prompt="Review our deployment.")
        current["messageId"] = "current-user"
        current["participant"] = {"id": "child", "label": "Child", "kind": "subagent", "parentId": "lead"}
        response = await sidecar.rpc("agent.run.start", current)
        self.assertEqual(response["result"]["result"]["context"]["totalTurns"], 1)
        seen = str(model.seen[0])
        self.assertIn("legacy deployment uses port 417", seen)
        self.assertNotIn("POISON_SYSTEM_INSTRUCTION", seen)
        self.assertNotIn("POISON_TOOL_INSTRUCTION", seen)
        self.assertNotIn("FUTURE_CONTENT", seen)
        self.assertEqual(sum(message.content == "Review our deployment." for message in model.seen[0]), 1)
        self.assertEqual(host.assert_method, "host.chat.context")
        self.assertEqual(host.calls[0]["excludeMessageId"], "current-user")
        await sidecar.rpc("agent.context.reset", {"sessionId": "session-1", "focusServerId": "srv_alpha"})
        await sidecar.rpc("agent.run.start", request("hydrated-2", server="srv_alpha", prompt="Hello."))
        self.assertEqual(len(host.calls), 1)
        self.assertNotIn("legacy deployment", str(model.seen[-1]))
        store.close()

        empty_store = ConversationStore()
        empty_sidecar = LocalSidecar(ScriptedModel([AIMessage(content="First"), AIMessage(content="Second")]), empty_store)
        empty_host = HistoryHost(empty=True)
        empty_sidecar._host_rpc = empty_host
        await empty_sidecar.rpc("agent.run.start", request("empty-1", server="srv_alpha"))
        await empty_sidecar.rpc("agent.run.start", request("empty-2", server="srv_alpha"))
        self.assertEqual(len(empty_host.calls), 1)
        empty_store.close()

        foreign_store = ConversationStore()
        foreign = LocalSidecar(ScriptedModel([]), foreign_store)
        foreign._host_rpc = HistoryHost(wrong_scope=True)
        rejected = await foreign.rpc("agent.run.start", request("foreign-hydration", server="srv_alpha"))
        self.assertEqual(rejected["error"]["code"], RPC_INVALID_PARAMS)
        self.assertEqual(foreign.resolutions, 0)
        self.assertEqual(foreign._run_sessions, {})
        foreign_store.close()

    async def test_hydration_and_checkpoint_reset_cannot_race_run_admission(self) -> None:
        loading = asyncio.Event()
        loaded = asyncio.Event()

        class SlowHistoryHost:
            async def request(self, _method: str, _params: dict[str, Any], **_kwargs: Any) -> dict[str, Any]:
                loading.set()
                await loaded.wait()
                return {"session": {"id": "session-1"}, "messages": []}

        store = ConversationStore()
        sidecar = LocalSidecar(ScriptedModel([AIMessage(content="Ready.")]), store)
        sidecar._host_rpc = SlowHistoryHost()
        run = asyncio.create_task(sidecar.rpc("agent.run.start", request("loading-run")))
        await loading.wait()
        for method, params in (("agent.context.reset", {"sessionId": "session-1"}),
                               ("agent.context.delete", {"sessionId": "session-1"}),
                               ("agent.context.resetAll", {}),
                               ("agent.run.start", request("loading-run"))):
            response = await sidecar.rpc(method, params)
            self.assertEqual(response["error"]["code"], RPC_INVALID_PARAMS)
        loaded.set()
        self.assertEqual((await run)["result"]["result"]["state"], "completed")
        self.assertEqual(sidecar._run_sessions, {})

        resetting = asyncio.Event()
        reset_done = asyncio.Event()

        async def slow_delete(_session_id: str | None = None) -> None:
            resetting.set()
            await reset_done.wait()

        sidecar._delete_context_checkpoints = slow_delete
        for method, params in (("agent.context.reset", {"sessionId": "session-1"}),
                               ("agent.context.resetAll", {})):
            resetting.clear()
            reset_done.clear()
            reset = asyncio.create_task(sidecar.rpc(method, params))
            await resetting.wait()
            refused = await sidecar.rpc("agent.run.start", request("during-reset"))
            self.assertEqual(refused["error"]["code"], RPC_INVALID_PARAMS)
            duplicate_reset = await sidecar.rpc("agent.context.resetAll", {})
            self.assertEqual(duplicate_reset["error"]["code"], RPC_INVALID_PARAMS)
            reset_done.set()
            self.assertTrue((await reset)["result"]["reset"])
        self.assertEqual(sidecar._resetting_sessions, set())
        self.assertFalse(sidecar._resetting_all)
        store.close()

    async def test_role_turns_share_durable_history_but_only_current_system_persona(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "conversation.sqlite"
            store = ConversationStore(database)
            first_model = ScriptedModel([AIMessage(content="Use a staged migration.")])
            first = LocalSidecar(first_model, store)
            response = await first.rpc("agent.run.start", request("run-1", server="srv_alpha"))
            self.assertEqual(response["result"]["result"]["state"], "completed")
            self.assertIsInstance(first_model.seen[0][0], SystemMessage)
            self.assertIn("Only this active role: planner", first_model.seen[0][0].content)
            self.assertEqual(first_model.seen[0][-1].content, "Plan the database change.")
            store.close()

            # A fresh sidecar and connection prove context does not depend on UI
            # transcript state or the process's old LangGraph instance.
            reopened = ConversationStore(database)
            second_model = ScriptedModel([AIMessage(content="I reviewed the staged migration.")])
            second = LocalSidecar(second_model, reopened)
            response = await second.rpc("agent.run.start", request("run-2", server="srv_alpha", persona="reviewer", prompt="Review our plan."))
            messages = second_model.seen[0]
            self.assertEqual(sum(isinstance(item, SystemMessage) for item in messages), 1)
            self.assertIn("Only this active role: reviewer", messages[0].content)
            self.assertNotIn("Only this active role: planner", str(messages))
            self.assertTrue(any("Use a staged migration." in str(item.content) for item in messages))
            self.assertTrue(any("Earlier answer by Planner" in str(item.content) for item in messages))
            self.assertEqual(response["result"]["result"]["context"]["includedTurns"], 1)

            isolated_model = ScriptedModel([AIMessage(content="Fresh session.")])
            isolated = LocalSidecar(isolated_model, reopened)
            await isolated.rpc("agent.run.start", request("run-3", session="session-2", server="srv_alpha"))
            self.assertEqual(len(isolated_model.seen[0]), 2)
            rejected = await isolated.rpc("agent.run.start", request("run-4", server="srv_beta"))
            self.assertEqual(rejected["error"]["code"], RPC_INVALID_PARAMS)
            self.assertEqual(isolated.resolutions, 1)
            rejected_global = await isolated.rpc("agent.run.start", request("run-5"))
            self.assertEqual(rejected_global["error"]["code"], RPC_INVALID_PARAMS)
            reopened.close()

    async def test_persona_history_metadata_keeps_identity_without_execution_instructions(self) -> None:
        store = ConversationStore()
        try:
            incoming = request("private-metadata", persona="fixture-role")
            incoming["providerId"] = "fixture-provider"
            incoming["taskId"] = "fixture-task"
            sentinel = "metadata-policy-sentinel-3127"
            incoming["persona"]["systemPrompt"] = sentinel
            parsed = _parse_run_request(incoming)
            fingerprint = _run_fingerprint(parsed)
            store.begin(parsed, fingerprint)
            stored = store._connection.execute(
                "SELECT metadata_json, fingerprint FROM conversation_turns WHERE run_id = ?",
                (parsed["runId"],),
            ).fetchone()
            metadata = json.loads(stored["metadata_json"])
            self.assertEqual(metadata["persona"], {"id": "fixture-role", "label": "Fixture-Role"})
            self.assertEqual(metadata["participant"], parsed["participant"])
            self.assertEqual((metadata["providerId"], metadata["model"], metadata["taskId"]),
                             ("fixture-provider", "scripted", "fixture-task"))
            self.assertNotIn(sentinel, stored["metadata_json"])
            self.assertNotIn("systemPrompt", stored["metadata_json"])
            self.assertEqual(parsed["persona"]["systemPrompt"], sentinel)
            self.assertEqual(stored["fingerprint"], fingerprint)
            # Admission replay still compares the complete execution identity.
            store.begin(parsed, fingerprint)
            changed = _parse_run_request(incoming | {"persona": incoming["persona"] | {"systemPrompt": "changed-policy"}})
            with self.assertRaises(ValueError):
                store.begin(changed, _run_fingerprint(changed))
            self.assertEqual(store._connection.execute("SELECT count(*) FROM conversation_turns").fetchone()[0], 1)
        finally:
            store.close()

    async def test_coordinator_and_worker_policies_reach_models_but_not_ordinary_records(self) -> None:
        lead_sentinel = "lead-policy-sentinel-5173"
        worker_sentinel = "worker-policy-sentinel-6841"
        goal = "Summarize this local fixture."

        class BoundaryModel(ScriptedModel):
            async def ainvoke(self, messages: list[Any]) -> AIMessage:
                self.seen.append(list(messages))
                current = next(message.content for message in reversed(messages) if isinstance(message, HumanMessage))
                if current == goal:
                    return AIMessage(content="Fixture summary complete.")
                if isinstance(messages[-1], ToolMessage):
                    return AIMessage(content="Fixture work dispatched.")
                return AIMessage(content="", tool_calls=[{
                    "name": "teams__dispatch", "args": {"tasks": [{
                        "key": "fixture-job", "roleId": "fixture-reader", "title": "Fixture summary",
                        "goal": goal, "access": "read",
                    }]}, "id": "fixture-dispatch", "type": "tool_call",
                }])

        store = ConversationStore()
        model = BoundaryModel([])
        sidecar = LocalSidecar(model, store)
        try:
            incoming = request("boundary-lead", prompt="Coordinate this local fixture.")
            incoming["persona"]["systemPrompt"] = lead_sentinel
            incoming["coordination"] = {"version": 1, "roles": [{
                "id": "fixture-reader", "team": "blue", "label": "Fixture reader",
                "systemPrompt": worker_sentinel,
            }], "servers": []}
            response = await asyncio.wait_for(sidecar.rpc("agent.run.start", incoming), 3)
            self.assertEqual(response["result"]["result"]["state"], "completed")
            await asyncio.wait_for(asyncio.gather(*list(sidecar._coordination.active.values())), 3)
            job = next(iter(sidecar._coordination.jobs.values()))
            self.assertEqual(job["state"], "completed")
            lead_inputs = [messages for messages in model.seen if messages[-1].content != goal
                           and any(lead_sentinel in str(message.content) for message in messages if isinstance(message, SystemMessage))]
            worker_inputs = [messages for messages in model.seen if messages[-1].content == goal]
            self.assertTrue(lead_inputs)
            self.assertEqual(len(worker_inputs), 1)
            self.assertTrue(worker_sentinel in str(worker_inputs[0][0].content), "Worker execution policy was not supplied")
            self.assertFalse(lead_sentinel in str(worker_inputs[0]), "Coordinator policy crossed into worker history")
            rows = store._connection.execute(
                "SELECT metadata_json, messages_json, summary_text FROM conversation_turns"
            ).fetchall()
            self.assertEqual(len(rows), 2)
            listing = await sidecar.rpc("agent.teams.list", {"sessionId": incoming["sessionId"]})
            public = json.dumps({"records": [tuple(row) for row in rows], "notifications": sidecar.events,
                                 "teamListing": listing}, ensure_ascii=False)
            for sentinel in (lead_sentinel, worker_sentinel):
                self.assertFalse(sentinel in public, "Execution policy entered ordinary records or notifications")
            self.assertFalse("systemPrompt" in public, "Execution-only field entered ordinary records")
            self.assertNotIn("coordination", json.dumps([row["metadata_json"] for row in rows]))
        finally:
            await sidecar.shutdown()
            store.close()

    async def test_legacy_policy_metadata_is_not_replayed_or_rewritten(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "legacy.sqlite"
            store = ConversationStore(database)
            incoming = _parse_run_request(request("legacy-policy", prompt="Public question. " + "x" * 5000))
            store.begin(incoming, _run_fingerprint(incoming))
            store.update_messages(incoming["runId"], [HumanMessage(content=incoming["promptText"]),
                                                     AIMessage(content="Legacy public answer.")])
            store.finish(incoming["runId"], "succeeded")
            metadata = json.loads(store._connection.execute("SELECT metadata_json FROM conversation_turns").fetchone()[0])
            sentinel = "legacy-policy-sentinel-7916"
            metadata["persona"]["systemPrompt"] = sentinel
            raw_legacy = json.dumps(metadata)
            with store._connection:
                store._connection.execute("UPDATE conversation_turns SET metadata_json = ?", (raw_legacy,))
            store.close()
            reopened = ConversationStore(database)
            try:
                recent = reopened.context("session-1", None)
                compacted = reopened.context("session-1", None, max_bytes=4096)
                self.assertEqual(recent.usage["includedTurns"], 1)
                self.assertEqual(compacted.usage["compactedTurns"], 1)
                self.assertIn("Legacy public answer.", str(recent.messages))
                self.assertIn("archive excerpts", str(compacted.messages))
                for context in (recent, compacted):
                    self.assertNotIn(sentinel, str(context.messages))
                    self.assertNotIn("systemPrompt", str(context.messages))
                    self.assertFalse(any(isinstance(message, SystemMessage) for message in context.messages))
                self.assertNotIn(sentinel, json.dumps(reopened.checkpoint_bindings()))
                self.assertEqual(reopened._connection.execute("SELECT metadata_json FROM conversation_turns").fetchone()[0], raw_legacy)
            finally:
                reopened.close()

    async def test_reset_clears_projection_and_checkpoints_but_keeps_raw_history_and_memories(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            store = ConversationStore(Path(directory) / "conversation.sqlite")
            async with open_checkpoint_store(directory) as saver:
                model = ScriptedModel([AIMessage(content="Prior answer."), AIMessage(content="Fresh answer.")])
                sidecar = LocalSidecar(model, store, checkpointer=saver)
                await sidecar.rpc("agent.run.start", request("reset-1", server="srv_alpha"))
                await sidecar.rpc("agent.memory.save", {"scope": "global", "title": "Database standard", "content": "Use PostgreSQL."})
                self.assertEqual(len([item async for item in saver.alist(None)] ) > 0, True)
                reset = await sidecar.rpc("agent.context.reset", {"sessionId": "session-1", "focusServerId": "srv_alpha"})
                self.assertEqual(reset["result"], {"reset": True})
                self.assertEqual([item async for item in saver.alist(None)], [])
                self.assertEqual(store.context("session-1", "srv_alpha").usage["totalTurns"], 0)
                self.assertEqual(store._connection.execute("SELECT count(*) FROM conversation_turns").fetchone()[0], 1)
                self.assertEqual(len(store.list_memories()), 1)
                await sidecar.rpc("agent.run.start", request("reset-2", server="srv_alpha", prompt="Hello."))
                self.assertNotIn("Prior answer.", str(model.seen[-1]))
                reset_all = await sidecar.rpc("agent.context.resetAll", {})
                self.assertEqual(reset_all["result"], {"reset": True, "count": 1})
                self.assertEqual(store._connection.execute("SELECT count(*) FROM conversation_turns").fetchone()[0], 2)
                deleted = await sidecar.rpc("agent.context.delete", {"sessionId": "session-1", "focusServerId": "srv_alpha"})
                self.assertEqual(deleted["result"], {"deleted": True})
                self.assertEqual(store._connection.execute("SELECT count(*) FROM conversation_turns").fetchone()[0], 0)
                self.assertEqual(len(store.list_memories()), 1)
            store.close()

    async def test_queued_turn_reads_preceding_final_answer_and_resets_refuse_active_runs(self) -> None:
        started = asyncio.Event()
        finish = asyncio.Event()

        class WaitingModel(ScriptedModel):
            async def ainvoke(self, messages: list[Any]) -> AIMessage:
                self.seen.append(messages)
                if len(self.seen) == 1:
                    started.set()
                    await finish.wait()
                    return AIMessage(content="First complete result.")
                return AIMessage(content="Second complete result.")

        store = ConversationStore()
        model = WaitingModel([])
        sidecar = LocalSidecar(model, store)
        first = asyncio.create_task(sidecar.rpc("agent.run.start", request("queued-1")))
        await started.wait()
        second = asyncio.create_task(sidecar.rpc("agent.run.start", request("queued-2", prompt="Use the first result.")))
        await asyncio.sleep(0)
        self.assertEqual(len(model.seen), 1)
        reset = await sidecar.rpc("agent.context.resetAll", {})
        self.assertEqual(reset["error"]["code"], RPC_INVALID_PARAMS)
        reset = await sidecar.rpc("agent.context.reset", {"sessionId": "session-1"})
        self.assertEqual(reset["error"]["code"], RPC_INVALID_PARAMS)
        deleted = await sidecar.rpc("agent.context.delete", {"sessionId": "session-1"})
        self.assertEqual(deleted["error"]["code"], RPC_INVALID_PARAMS)
        finish.set()
        await asyncio.gather(first, second)
        self.assertIn("First complete result.", str(model.seen[1]))
        self.assertNotIn("Use the first result.", str(model.seen[0]))
        store.close()

    async def test_large_unicode_archive_is_bounded_without_rewriting_raw_turns(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "conversation.sqlite"
            store = ConversationStore(database)
            original = "完整的原始证据" * 500
            for index in range(24):
                parsed = _parse_run_request(request(f"large-{index}", prompt=f"Question {index}: " + original))
                store.begin(parsed, _run_fingerprint(parsed))
                store.update_messages(parsed["runId"], [HumanMessage(content=parsed["promptText"]), AIMessage(content=f"Decision {index}: keep the evidence.")])
                store.finish(parsed["runId"], "succeeded")
            context = store.context("session-1", None, max_bytes=20_000)
            self.assertEqual(context.usage["totalTurns"], 24)
            self.assertGreater(context.usage["compactedTurns"], 0)
            self.assertEqual(context.usage["includedTurns"] + context.usage["compactedTurns"], 24)
            self.assertLessEqual(message_bytes(context.messages), 20_000)
            self.assertIn("Decision 23", str(context.messages))
            raw = store._connection.execute("SELECT messages_json FROM conversation_turns WHERE run_id = 'large-0'").fetchone()[0]
            self.assertIn(original, raw)
            memory = store.save_memory(scope="server", server_id="srv_alpha", title="数据库约定", content="使用 PostgreSQL。")
            store.close()
            reopened = ConversationStore(database)
            self.assertEqual(reopened.list_memories(server_id="srv_alpha", query="数据库")[0]["id"], memory["id"])
            self.assertEqual(reopened._connection.execute("SELECT count(*) FROM conversation_turns").fetchone()[0], 24)
            reopened.close()

    async def test_tool_and_approval_records_survive_reopen_and_compact_as_whole_turns(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "conversation.sqlite"
            store = ConversationStore(database)
            registry = ToolRegistry(MemoryAuditSink())

            async def write(_arguments: Any, _context: Any) -> str:
                return "changed"

            registry.register(ToolSpec(name="ops.write", description="A change requiring approval.", risk_level="write", requires_approval=True,
                                       input_schema={"type": "object", "properties": {}, "additionalProperties": False}, handler=write, target_id_argument=None))
            model = ScriptedModel([AIMessage(content="", tool_calls=[{"name": "ops__write", "args": {}, "id": "provider-call", "type": "tool_call"}]), AIMessage(content="The change is complete.")])
            sidecar = LocalSidecar(model, store, registry=registry)
            run = asyncio.create_task(sidecar.rpc("agent.run.start", request("tool-1", prompt="Make the change.")))
            for _ in range(100):
                if sidecar._pending_approvals:
                    break
                await asyncio.sleep(0.01)
            approval_id = next(iter(sidecar._pending_approvals))
            await sidecar.rpc("agent.approval.respond", {"approvalId": approval_id, "runId": "tool-1", "decision": "approve_once", "respondedAt": "2026-10-17T00:00:00Z"})
            response = await run
            self.assertEqual(response["result"]["result"]["toolCalls"], 1)
            store.close()
            reopened = ConversationStore(database)
            events = [json.loads(row[0]) for row in reopened._connection.execute("SELECT event_json FROM conversation_events ORDER BY ordinal")]
            self.assertEqual([event["type"] for event in events if event["type"] in ("agent.waiting_approval", "approval.decision")], ["agent.waiting_approval", "approval.decision"])
            calls = [event for event in events if event["type"] == "agent.tool_call"]
            results = [event for event in events if event["type"] == "agent.tool_result"]
            self.assertEqual(calls[0]["callId"], results[0]["callId"])
            context = reopened.context("session-1", None)
            self.assertEqual(sum(isinstance(message, ToolMessage) for message in context.messages), 1)
            self.assertTrue(any(isinstance(message, AIMessage) and message.tool_calls for message in context.messages))
            compacted = reopened.context("session-1", None, max_bytes=1024)
            self.assertEqual(compacted.usage["compactedTurns"], 1)
            self.assertLessEqual(compacted.usage["inputBytes"], 1024)
            self.assertFalse(any(isinstance(message, ToolMessage) for message in compacted.messages))
            self.assertEqual(reopened._connection.execute("SELECT count(*) FROM conversation_events").fetchone()[0], len(events))
            reopened.close()

    async def test_explicit_child_participant_reads_parent_tools_and_cannot_cross_scope(self) -> None:
        store = ConversationStore()
        registry = ToolRegistry(MemoryAuditSink())

        async def read(_arguments: Any, _context: Any) -> str:
            return "shared evidence marker 417"

        registry.register(ToolSpec(name="probe.read", description="Read evidence.", risk_level="read", requires_approval=False,
                                   input_schema={"type": "object", "properties": {}, "additionalProperties": False}, handler=read, target_id_argument=None))
        model = ScriptedModel([
            AIMessage(content="", tool_calls=[{"name": "probe__read", "args": {}, "id": "evidence-read", "type": "tool_call"}]),
            AIMessage(content="The lead found marker 417."),
            AIMessage(content="The child reviewed the same marker 417."),
        ])
        sidecar = LocalSidecar(model, store, registry=registry)
        lead = request("parent-1", server="srv_alpha", persona="lead", prompt="Read evidence.")
        await sidecar.rpc("agent.run.start", lead)
        child = request("child-1", server="srv_alpha", persona="reviewer", prompt="Review the lead evidence.")
        child["participant"] = {"id": "child-reviewer", "label": "Child reviewer", "kind": "subagent", "parentId": "lead"}
        response = await sidecar.rpc("agent.run.start", child)
        self.assertEqual(response["result"]["result"]["toolCalls"], 0)
        self.assertTrue(any(isinstance(message, ToolMessage) and "shared evidence marker 417" in message.content for message in model.seen[-1]))
        self.assertIn("The lead found marker 417.", str(model.seen[-1]))
        self.assertNotIn("[Participant:", str(model.seen[-1]))
        foreign = {**child, "runId": "child-foreign", "sessionId": "session-other"}
        rejected = await sidecar.rpc("agent.run.start", foreign)
        self.assertEqual(rejected["error"]["code"], RPC_INVALID_PARAMS)
        self.assertEqual(sidecar.resolutions, 2)
        foreign_scope = {**child, "runId": "child-server", "focusServerId": "srv_beta"}
        rejected = await sidecar.rpc("agent.run.start", foreign_scope)
        self.assertEqual(rejected["error"]["code"], RPC_INVALID_PARAMS)
        altered = {**child, "runId": "child-altered", "participant": {"id": "child-reviewer", "label": "Rebound", "kind": "agent"}}
        rejected = await sidecar.rpc("agent.run.start", altered)
        self.assertEqual(rejected["error"]["code"], RPC_INVALID_PARAMS)
        for depth in range(1, 5):
            store.validate_participant("session-1", "srv_alpha", {"id": f"depth-{depth}", "label": "Child", "kind": "subagent", "parentId": "lead" if depth == 1 else f"depth-{depth - 1}"}, register=True)
        with self.assertRaises(ValueError):
            store.validate_participant("session-1", "srv_alpha", {"id": "depth-5", "label": "Child", "kind": "subagent", "parentId": "depth-4"})
        store.close()

    async def test_memory_is_explicit_indexed_and_scoped_with_provenance(self) -> None:
        store = ConversationStore()
        model = ScriptedModel([AIMessage(content="A private conversation is not a memory.")])
        sidecar = LocalSidecar(model, store)
        await sidecar.rpc("agent.run.start", request("memory-chat-1"))
        self.assertEqual(store.list_memories(), [])
        for payload in (
            {"scope": "global", "title": "数据库约定", "content": "数据库统一使用 PostgreSQL。"},
            {"scope": "server", "serverId": "srv_alpha", "title": "数据库路径", "content": "数据库备份在 /srv/backups。"},
            {"scope": "server", "serverId": "srv_beta", "title": "数据库机密", "content": "Other server fact."},
        ):
            response = await sidecar.rpc("agent.memory.save", payload)
            self.assertIn("memory", response["result"])
        matches = store.list_memories(server_id="srv_alpha", query="数据库", limit=12)
        self.assertEqual(len(matches), 2)
        memories, sources = store.memory_context("srv_alpha", "数据库")
        self.assertEqual(len(sources), 2)
        self.assertNotIn("Other server fact.", str(memories))
        self.assertEqual({source["scope"] for source in sources}, {"global", "server"})
        self.assertTrue(all("id" in source and "updatedAt" in source for source in sources))
        self.assertTrue(all("content" not in source for source in sources))
        self.assertLessEqual(message_bytes(memories), 16 * 1024)
        self.assertEqual(len(store.list_memories(query="数据库")), 1)
        memory = matches[1]
        denied = await sidecar.rpc("agent.memory.save", {"id": memory["id"], "scope": "server", "serverId": "srv_beta", "title": "Wrong", "content": "Wrong"})
        self.assertIn("error", denied)
        bad_scope = await sidecar.rpc("agent.memory.save", {"scope": "server", "title": "Missing scope", "content": "No server"})
        self.assertEqual(bad_scope["error"]["code"], RPC_INVALID_PARAMS)
        store.close()

    async def test_incomplete_tool_turn_is_reference_data_and_raw_fields_are_retained(self) -> None:
        store = ConversationStore()
        parsed = _parse_run_request(request("incomplete-1"))
        store.begin(parsed, _run_fingerprint(parsed))
        store.update_messages("incomplete-1", [HumanMessage(content="Do a thing"), AIMessage(content="", tool_calls=[{"name": "ops__read", "args": {}, "id": "unanswered", "type": "tool_call"}], additional_kwargs={"reasoning_content": "private thinking state"})])
        store.finish("incomplete-1", "cancelled")
        context = store.context("session-1", None)
        self.assertFalse(any(isinstance(message, AIMessage) and message.tool_calls for message in context.messages))
        raw = store._connection.execute("SELECT messages_json FROM conversation_turns").fetchone()[0]
        self.assertIn("private thinking state", raw)
        self.assertNotIn("private thinking state", str(context.messages))
        store.close()


class DeepSeekAdapterTests(unittest.TestCase):
    def test_thinking_content_roundtrips_only_on_official_deepseek_chat_adapter(self) -> None:
        model = ModelFactory().create_with_api_key(ModelConfig("openai-compatible", "deepseek-flash", base_url="https://api.deepseek.com", wire_api="chat"), "test-key")
        result = model._create_chat_result({"choices": [{"message": {"role": "assistant", "content": "", "reasoning_content": "private chain", "tool_calls": [{"id": "call-1", "type": "function", "function": {"name": "probe__read", "arguments": "{}"}}]}, "finish_reason": "tool_calls"}], "model": "deepseek-flash"})
        response = result.generations[0].message
        self.assertEqual(response.additional_kwargs["reasoning_content"], "private chain")
        self.assertNotIn("private chain", response.content)
        payload = model._get_request_payload([HumanMessage(content="Read"), response, ToolMessage(content="ok", tool_call_id="call-1")])
        self.assertEqual(payload["messages"][1]["reasoning_content"], "private chain")
        self.assertIn("max_tokens", payload)
        self.assertNotIn("max_completion_tokens", payload)
        other = ModelFactory().create_with_api_key(ModelConfig("openai-compatible", "local-model", base_url="http://127.0.0.1:1234/v1"), None)
        other_payload = other._get_request_payload([response])
        self.assertNotIn("reasoning_content", other_payload["messages"][0])


if __name__ == "__main__":
    unittest.main()
