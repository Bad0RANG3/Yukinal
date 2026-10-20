"""Real LangGraph/sidecar concurrency tests, with local deterministic models only."""
from __future__ import annotations

import asyncio
import json
import tempfile
import unittest
from pathlib import Path

from langchain_core.messages import AIMessage, HumanMessage, SystemMessage, ToolMessage

from yukinal_agent import MemoryAuditSink, ToolRegistry, ToolSpec
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.coordination import CoordinationRuntime, parse_coordination
from yukinal_agent.sidecar import PythonSidecar, _parse_run_request, _run_fingerprint


def config():
    return {"version": 1, "roles": [
        {"id": "blue-reader", "team": "blue", "label": "蓝队", "systemPrompt": "Collect read-only evidence."},
        {"id": "yellow-builder", "team": "yellow", "label": "黄队", "systemPrompt": "Build only the assigned environment."},
    ], "servers": [{"id": "srv_alpha", "name": "Alpha", "environment": "development"},
                    {"id": "srv_beta", "name": "Beta", "environment": "production"}]}


def request(run, prompt="parallel", session="session-main", **extra):
    return {"runId": run, "sessionId": session, "prompt": prompt, "delivery": "sync", "coordination": config(), **extra}


def task(key, server="srv_alpha", access="read"):
    return {"key": key, "roleId": "blue-reader", "title": key + " title", "goal": key + " private brief", "access": access, "serverId": server}


class Sidecar(PythonSidecar):
    def __init__(self, model, store, registry=None):
        self.model, self.events = model, []
        super().__init__(registry or ToolRegistry(MemoryAuditSink()), conversation_store=store,
                         notification_sink=lambda _name, value: self.events.append(value))
        self.handle({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "1.0", "clientVersion": "test", "dataDir": ""}})

    async def _run_provider_model(self, provider, model):
        return self.model, 120_000

    async def rpc(self, method, params):
        response = await self.handle_async({"jsonrpc": "2.0", "id": 2, "method": method, "params": params})
        if "error" in response:
            raise AssertionError(response["error"])
        return response["result"]


class RoutingModel:
    def __init__(self, tasks):
        self.tasks = tasks
        self.started = {item["goal"]: asyncio.Event() for item in tasks}
        self.release = {item["goal"]: asyncio.Event() for item in tasks}
        self.histories = {}

    def bind_tools(self, tools, **kwargs):
        return self

    def bind(self, **kwargs):
        return self

    async def ainvoke(self, messages):
        prompt = next(message.content for message in reversed(messages) if isinstance(message, HumanMessage))
        system = next(message.content for message in messages if isinstance(message, SystemMessage))
        if "Active role: 协调助手" in system:
            self.histories[prompt] = messages
            if prompt == "parallel" and not isinstance(messages[-1], ToolMessage):
                return AIMessage(content="", tool_calls=[{"name": "teams__dispatch", "args": {"tasks": self.tasks}, "id": "dispatch-call", "type": "tool_call"}])
            return AIMessage(content="已安排团队，仍然可以继续对话。" if prompt == "parallel" else "协调助手继续回复。")
        self.histories[prompt] = messages
        self.started[prompt].set()
        await self.release[prompt].wait()
        return AIMessage(content=prompt + " verified result", usage_metadata={"input_tokens": 11, "output_tokens": 7, "total_tokens": 18})


class CoordinatorTests(unittest.IsolatedAsyncioTestCase):
    async def test_stable_system_prefix_excludes_changing_task_state(self):
        store = ConversationStore()
        async def launch(*_args): return {"state": "completed", "text": "done"}
        runtime = CoordinationRuntime(ToolRegistry(MemoryAuditSink()), store, lambda _event: None, launch)
        parent = {"runId": "run-prefix", "sessionId": "session-prefix", "promptText": "检查 Alpha", "coordination": config()}
        try:
            before = runtime.context(parent, include_tasks=False)
            runtime.jobs["job"] = {"id":"job", "sessionId":"session-prefix", "team":"blue", "title":"check", "state":"running", "access":"read"}
            running = runtime.turn_context(parent)
            runtime.jobs["job"]["state"] = "completed"
            runtime.jobs["other"] = {"id":"foreign", "sessionId":"other-session", "state":"failed", "title":"FOREIGN_PRIVATE_TASK"}
            parent["promptText"] = "继续 Beta"
            self.assertEqual(runtime.context(parent, include_tasks=False), before)
            self.assertNotEqual(runtime.turn_context(parent), running)
            self.assertIn('"state":"completed"', runtime.turn_context(parent))
            self.assertNotIn("FOREIGN_PRIVATE_TASK", runtime.turn_context(parent))
            self.assertNotIn('"tasks"', before)
        finally: store.close()

    async def test_two_team_approvals_are_independent_and_chat_continues_while_waiting(self):
        executed = []
        waiting = asyncio.Event()
        class ApprovalModel(RoutingModel):
            async def ainvoke(self, messages):
                system = str(messages[0].content)
                if "Active role: 协调助手" in system:
                    return await super().ainvoke(messages)
                prompt = next(message.content for message in reversed(messages) if isinstance(message, HumanMessage))
                if not isinstance(messages[-1], ToolMessage):
                    return AIMessage(content="", tool_calls=[{"name": "probe__change", "args": {"value": prompt}, "id": "change", "type": "tool_call"}])
                return AIMessage(content="团队已返回批准结果。")
        registry = ToolRegistry(MemoryAuditSink())
        async def change(arguments, context):
            executed.append((arguments["value"], context.run_id))
            return {"ok": True}
        registry.register(ToolSpec(name="probe.change", description="Local test-only change", risk_level="write", requires_approval=True,
            target_id_argument=None, input_schema={"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"], "additionalProperties": False}, handler=change))
        items = [task("approve-alpha", access="write"), task("reject-beta", "srv_beta", access="write")]
        store = ConversationStore()
        sidecar = Sidecar(ApprovalModel(items), store, registry)
        original_sink = sidecar._notification_sink
        def sink(name, event):
            original_sink(name, event)
            if len([job for job in sidecar._coordination.jobs.values() if job["state"] == "waiting_approval"]) == 2:
                waiting.set()
        sidecar.set_notification_sink(sink)
        try:
            await sidecar.rpc("agent.run.start", request("approve-lead"))
            await asyncio.wait_for(waiting.wait(), 3)
            jobs = list(sidecar._coordination.jobs.values())
            alpha, beta = jobs[0], jobs[1]
            self.assertEqual(executed, [])
            await asyncio.wait_for(sidecar.rpc("agent.run.start", request("approve-chat", "Explain while waiting.")), 3)
            wrong = await sidecar.rpc("agent.approval.respond", {"approvalId": alpha["approval"]["approvalId"], "runId": beta["runId"], "decision": "approve_once", "respondedAt": "2026-10-17T12:00:00Z"})
            self.assertFalse(wrong["accepted"])
            for job, decision in ((alpha, "approve_once"), (beta, "reject")):
                response = await sidecar.rpc("agent.approval.respond", {"approvalId": job["approval"]["approvalId"], "runId": job["runId"], "decision": decision, "respondedAt": "2026-10-17T12:00:00Z"})
                self.assertTrue(response["accepted"])
            await asyncio.wait_for(asyncio.gather(*list(sidecar._coordination.active.values())), 3)
            self.assertEqual(executed, [("approve-alpha private brief", alpha["runId"])])
            self.assertTrue(all("approval" not in job for job in jobs))
        finally:
            await sidecar.shutdown()
            store.close()

    async def test_parallel_workers_do_not_block_foreground_or_mix_context(self):
        items = [task("alpha"), task("beta", "srv_beta")]
        model, store = RoutingModel(items), ConversationStore()
        sidecar = Sidecar(model, store)
        try:
            response = await asyncio.wait_for(sidecar.rpc("agent.run.start", request("lead-1")), 3)
            self.assertEqual(response["result"]["state"], "completed")
            # Both workers must reach the model before either is released.
            await asyncio.wait_for(asyncio.gather(*(event.wait() for event in model.started.values())), 3)
            followup = await asyncio.wait_for(sidecar.rpc("agent.run.start", request("lead-2", "Can we still talk?")), 3)
            self.assertEqual(followup["result"]["text"], "协调助手继续回复。")
            self.assertTrue(all(not event.is_set() for event in model.release.values()))
            self.assertNotIn("beta private brief", str(model.histories["alpha private brief"]))
            self.assertNotIn("alpha private brief", str(model.histories["beta private brief"]))
            jobs = (await sidecar.rpc("agent.teams.list", {"sessionId": "session-main"}))["tasks"]
            self.assertEqual(len(jobs), 2)
            self.assertEqual({job["sessionId"] for job in jobs}, {"session-main"})
            alpha = next(job for job in jobs if job["serverId"] == "srv_alpha")
            beta = next(job for job in jobs if job["serverId"] == "srv_beta")
            self.assertTrue((await sidecar.rpc("agent.run.stop", {"runId": beta["runId"]}))["stopped"])
            await asyncio.sleep(0.05)
            self.assertEqual(sidecar._coordination.jobs[beta["id"]]["state"], "cancelled")
            self.assertEqual(sidecar._coordination.jobs[alpha["id"]]["state"], "running")
            model.release["alpha private brief"].set()
            await asyncio.wait_for(asyncio.gather(*list(sidecar._coordination.active.values())), 3)
            self.assertEqual(sidecar._coordination.jobs[alpha["id"]]["state"], "completed")
            usage = sidecar._coordination.jobs[alpha["id"]]["usage"]
            self.assertEqual({key: usage[key] for key in ("inputTokens", "outputTokens", "reportedRequests", "unreportedRequests")},
                             {"inputTokens": 11, "outputTokens": 7, "reportedRequests": 1, "unreportedRequests": 0})
            self.assertGreaterEqual(usage["lastRequest"]["durationMs"], 0)
            self.assertNotIn("verified result", str(store.context("session-main", None).messages))
            self.assertTrue(any(event["type"] == "agent.team_task" for event in sidecar.events))
            # A subsequent coordinator turn gets actual completed results, not a fabricated success.
            await sidecar.rpc("agent.run.start", request("lead-3", "What finished?"))
            self.assertIn("alpha private brief verified result", str(model.histories["What finished?"]))
            self.assertRegex(str(model.histories["What finished?"]), r'"state"\s*:\s*"cancelled"')
        finally:
            await sidecar.shutdown()
            store.close()

    async def test_batch_validation_idempotence_scope_and_readonly(self):
        model, store = RoutingModel([]), ConversationStore()
        sidecar = Sidecar(model, store)
        try:
            parent = _parse_run_request(request("bounded", focusServerId="srv_alpha", mode="readonly"))
            sidecar._coordination.activate(parent)
            from yukinal_agent.tools import ExecutionContext, ToolCallError
            context = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id="bounded", allowed_tools=frozenset({"teams.dispatch"}), allowed_target_ids=frozenset(), approved_actions={})
            for invalid in ([task("ok"), task("wrong", "srv_beta")], [task("write", access="write")]):
                with self.assertRaises(ToolCallError):
                    await sidecar._coordination._dispatch({"tasks": invalid}, context)
                self.assertEqual(sidecar._coordination.jobs, {})
            # Prevent the worker model from running; cancellation while queued still settles durably.
            gate = asyncio.Event()
            async def launch(*args):
                await gate.wait()
                return {"state": "completed", "text": "done"}
            sidecar._coordination.launch = launch
            first = await sidecar._coordination._dispatch({"tasks": [task("same")]}, context)
            second = await sidecar._coordination._dispatch({"tasks": [task("same")]}, context)
            self.assertEqual(first["tasks"][0]["id"], second["tasks"][0]["id"])
            self.assertEqual(len(sidecar._coordination.jobs), 1)
            with self.assertRaises(ToolCallError):
                await sidecar._coordination._dispatch({"tasks": [{**task("same"), "goal": "different"}]}, context)
            run_id = first["tasks"][0]["runId"]
            self.assertTrue(sidecar._coordination.stop(run_id))
            await asyncio.sleep(0)
            await asyncio.sleep(0)
            self.assertEqual(next(iter(sidecar._coordination.jobs.values()))["state"], "cancelled")
            # Worker run identities cannot act as a coordinator.
            outsider = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id=run_id, allowed_tools=frozenset({"teams.dispatch"}), allowed_target_ids=frozenset(), approved_actions={})
            with self.assertRaises(ToolCallError):
                await sidecar._coordination._dispatch({"tasks": [task("recursive")]}, outsider)
        finally:
            await sidecar.shutdown()
            store.close()

    async def test_same_target_write_excludes_reads_and_other_target_still_runs(self):
        store = ConversationStore()
        started, release = {}, {}
        async def launch(job, *args):
            started[job["title"]] = True
            await release[job["title"]].wait()
            return {"state": "completed", "text": "evidence"}
        runtime = CoordinationRuntime(ToolRegistry(MemoryAuditSink()), store, lambda event: None, launch)
        parent = _parse_run_request(request("gates"))
        runtime.activate(parent)
        from yukinal_agent.tools import ExecutionContext
        context = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id="gates", allowed_tools=frozenset({"teams.dispatch"}), allowed_target_ids=frozenset(), approved_actions={})
        items = [task("writer", access="write"), task("reader"), task("other", "srv_beta")]
        release = {item["title"]: asyncio.Event() for item in items}
        try:
            await runtime._dispatch({"tasks": items}, context)
            await asyncio.sleep(0.05)
            self.assertIn("writer title", started)
            self.assertIn("other title", started)
            self.assertNotIn("reader title", started)
            release["writer title"].set()
            await asyncio.sleep(0.05)
            self.assertIn("reader title", started)
        finally:
            await runtime.shutdown()
            store.close()

    async def test_approval_and_restart_are_task_bound_and_no_automatic_replay(self):
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory)/"tasks.sqlite"
            store = ConversationStore(database)
            events = []
            never = asyncio.Event()
            async def launch(*args):
                await never.wait()
            runtime = CoordinationRuntime(ToolRegistry(MemoryAuditSink()), store, events.append, launch)
            parent = _parse_run_request(request("persist"))
            runtime.activate(parent)
            from yukinal_agent.tools import ExecutionContext
            context = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id="persist", allowed_tools=frozenset({"teams.dispatch"}), allowed_target_ids=frozenset(), approved_actions={})
            response = await runtime._dispatch({"tasks": [task("approval")]}, context)
            await asyncio.sleep(0)
            job = next(iter(runtime.jobs.values()))
            runtime.observe({"type": "agent.waiting_approval", "runId": job["runId"], "approval": {"approvalId": "apr", "runId": job["runId"], "hostInput": {"secret": "do-not-project"}}})
            runtime.observe({"type": "agent.usage", "runId": job["runId"], "usage": {"inputTokens": 3, "outputTokens": 2}})
            self.assertEqual(job["state"], "waiting_approval")
            self.assertIn("approval", job)
            self.assertNotIn("hostInput", str(events))
            # Simulate a crash snapshot without executing/resuming anything on restore.
            recovered_store = ConversationStore(database)
            recovered = CoordinationRuntime(ToolRegistry(MemoryAuditSink()), recovered_store, events.append, launch)
            self.assertEqual(recovered.list()[0]["state"], "interrupted")
            self.assertEqual(recovered.active, {})
            self.assertNotIn("approval", recovered.list()[0])
            recovered_store.close()
            await runtime.shutdown()
            store.close()

    def test_config_is_in_admission_fingerprint_and_invalid_config_fails(self):
        first = _parse_run_request(request("config"))
        second = _parse_run_request(request("config", coordination={**config(), "preferredRoleId": "blue-reader"}))
        self.assertNotEqual(_run_fingerprint(first), _run_fingerprint(second))
        with self.assertRaises(ValueError):
            parse_coordination({**config(), "version": True})

    def test_large_roster_context_is_complete_json_and_keeps_the_focused_server(self):
        store = ConversationStore()
        large = {**config(), "servers": [{"id": f"srv_{index}", "name": "server" + "x" * 190, "environment": "development"} for index in range(128)]}
        runtime = CoordinationRuntime(ToolRegistry(MemoryAuditSink()), store, lambda event: None, None)
        try:
            encoded = runtime.context(_parse_run_request(request("context", focusServerId="srv_127", coordination=large))).rsplit("\n", 1)[1]
            context = json.loads(encoded)
            self.assertLessEqual(len(encoded), 14000)
            self.assertEqual(context["servers"][0]["id"], "srv_127")
            self.assertGreater(context["omittedServers"], 0)
            self.assertEqual(context["roles"][0]["id"], "blue-reader")
        finally:
            store.close()


if __name__ == "__main__":
    unittest.main()
