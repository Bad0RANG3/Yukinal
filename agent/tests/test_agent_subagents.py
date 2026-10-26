"""Real sidecar/subagent admission, isolation and approvals; local fixtures only.

The coordinator roster was replaced by role-free subagents that inherit the
parent run's frozen model, target, permission mode and policy. These tests pin
the behaviour that replaced it: scope isolation, durable task identity,
independent approvals, same-target write exclusion, capacity and restart
handling, and configuration admission.
"""
from __future__ import annotations

import asyncio
import json
import tempfile
import unittest
from pathlib import Path

from langchain_core.messages import AIMessage, HumanMessage, SystemMessage, ToolMessage

from yukinal_agent import MemoryAuditSink, ToolRegistry, ToolSpec
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.sidecar import PythonSidecar, _parse_run_request, _run_fingerprint
from yukinal_agent.subagents import parse_subagents
from yukinal_agent.tools import ExecutionContext, ToolCallError


SERVERS = {"version": 1, "servers": [
    {"id": "srv_alpha", "name": "Alpha", "environment": "development"},
    {"id": "srv_beta", "name": "Beta", "environment": "production"},
]}
DELEGATING = "并行检查防火墙配置和独立审计应用代码"
FOLLOWUP = "Can we still talk?"


def request(run, prompt=DELEGATING, session="session-main", **extra):
    return {"runId": run, "sessionId": session, "prompt": prompt, "delivery": "sync", "subagents": SERVERS, **extra}


def task(key, server=None, access="read", goal=None):
    return {"key": key, "title": key + " title", "goal": goal or key + " private brief",
            "access": access, **({"serverId": server} if server else {})}


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
    """One model instance per run; the worker waits so outcomes are observable."""

    def __init__(self, tasks, *, dispatch=True):
        self.tasks, self.dispatch = tasks, dispatch
        self.started = {item["goal"]: asyncio.Event() for item in tasks}
        self.release = {item["goal"]: asyncio.Event() for item in tasks}
        self.histories: dict[str, list[Any]] = {}

    def bind_tools(self, tools, **kwargs):
        self.tools = {tool["function"]["name"] for tool in tools}
        return self

    def bind(self, **kwargs):
        return self

    async def ainvoke(self, messages):
        prompt = next(message.content for message in reversed(messages) if isinstance(message, HumanMessage))
        self.histories[prompt] = messages
        if "subagents__start" in getattr(self, "tools", set()) and prompt == DELEGATING and not isinstance(messages[-1], ToolMessage):
            return AIMessage(content="", tool_calls=[{"name": "subagents__start", "args": {"tasks": self.tasks}, "id": "dispatch-call", "type": "tool_call"}])
        if prompt in self.started:
            self.started[prompt].set()
            await self.release[prompt].wait()
            return AIMessage(content=prompt + " verified result", usage_metadata={"input_tokens": 11, "output_tokens": 7, "total_tokens": 18})
        if "subagents__status" in getattr(self, "tools", set()) and isinstance(messages[-1], ToolMessage):
            return AIMessage(content="Results checked.")
        return AIMessage(content="Foreground still responsive.")


class ApprovalModel(RoutingModel):
    async def ainvoke(self, messages):
        if isinstance(messages[-1], ToolMessage):
            return AIMessage(content="approval fixture answered")
        prompt = next(message.content for message in reversed(messages) if isinstance(message, HumanMessage))
        if prompt in self.started:
            return AIMessage(content="", tool_calls=[{"name": "probe__change", "args": {"value": prompt}, "id": "change", "type": "tool_call"}])
        return await super().ainvoke(messages)


def registry_with_change():
    executed: list[tuple[str, str]] = []
    registry = ToolRegistry(MemoryAuditSink())
    async def change(arguments, context):
        executed.append((arguments["value"], context.run_id))
        return {"ok": True}
    registry.register(ToolSpec(name="probe.change", description="Local test-only change", risk_level="write",
        requires_approval=True, target_id_argument=None, handler=change,
        input_schema={"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"], "additionalProperties": False}))
    return registry, executed


class SubagentRuntimeTests(unittest.IsolatedAsyncioTestCase):
    async def test_context_prefix_is_stable_and_foreign_sessions_are_excluded(self):
        store = ConversationStore()
        sidecar = PythonSidecar(ToolRegistry(MemoryAuditSink()), conversation_store=store)
        parent = _parse_run_request(request("run-prefix", prompt="检查 Alpha"))
        try:
            runtime = sidecar._subagents
            runtime.parents["run-prefix"] = parent
            before = runtime.context(parent, include_tasks=False)
            runtime.jobs["job"] = {"id": "job", "sessionId": "session-main", "title": "check", "state": "running", "access": "read"}
            running = runtime.turn_context(parent)
            runtime.jobs["job"]["state"] = "completed"
            runtime.jobs["other"] = {"id": "foreign", "sessionId": "other-session", "state": "failed", "title": "FOREIGN_PRIVATE_TASK"}
            # The immutable prefix (policy, servers) never changes with task state.
            self.assertEqual(runtime.context(parent, include_tasks=False), before)
            self.assertNotEqual(runtime.turn_context(parent), running)
            self.assertIn('"state":"completed"', runtime.turn_context(parent))
            self.assertNotIn("FOREIGN_PRIVATE_TASK", runtime.turn_context(parent))
            self.assertNotIn('"tasks"', before)
            self.assertIn("subagents", before.lower())
        finally:
            await sidecar.shutdown()
            store.close()

    async def test_parallel_workers_do_not_block_foreground_or_mix_context(self):
        items = [task("alpha", "srv_alpha"), task("beta", "srv_beta")]
        model, store = RoutingModel(items), ConversationStore()
        sidecar = Sidecar(model, store)
        try:
            response = await sidecar.rpc("agent.run.start", request("lead-1"))
            self.assertEqual(response["result"]["state"], "completed")
            await asyncio.wait_for(asyncio.gather(*(event.wait() for event in model.started.values())), 3)
            followup = await asyncio.wait_for(sidecar.rpc("agent.run.start", request("lead-2", FOLLOWUP)), 3)
            self.assertEqual(followup["result"]["text"], "Foreground still responsive.")
            self.assertTrue(all(not event.is_set() for event in model.release.values()))
            self.assertNotIn("beta private brief", str(model.histories["alpha private brief"]))
            self.assertNotIn("alpha private brief", str(model.histories["beta private brief"]))
            jobs = (await sidecar.rpc("agent.subagents.list", {"sessionId": "session-main"}))["tasks"]
            self.assertEqual(len(jobs), 2)
            self.assertEqual({job["sessionId"] for job in jobs}, {"session-main"})
            alpha = next(job for job in jobs if job["serverId"] == "srv_alpha")
            beta = next(job for job in jobs if job["serverId"] == "srv_beta")
            self.assertNotIn("goal", alpha, "The private brief never leaves the runtime")
            self.assertTrue((await sidecar.rpc("agent.run.stop", {"runId": beta["runId"]}))["stopped"])
            await asyncio.sleep(0.05)
            self.assertEqual(sidecar._subagents.jobs[beta["id"]]["state"], "cancelled")
            self.assertEqual(sidecar._subagents.jobs[alpha["id"]]["state"], "running")
            model.release["alpha private brief"].set()
            await asyncio.wait_for(asyncio.gather(*list(sidecar._subagents.active.values())), 3)
            self.assertEqual(sidecar._subagents.jobs[alpha["id"]]["state"], "completed")
            usage = sidecar._subagents.jobs[alpha["id"]]["usage"]
            self.assertEqual({key: usage[key] for key in ("inputTokens", "outputTokens", "reportedRequests", "unreportedRequests")},
                             {"inputTokens": 11, "outputTokens": 7, "reportedRequests": 1, "unreportedRequests": 0})
            self.assertGreaterEqual(usage["lastRequest"]["durationMs"], 0)
            self.assertNotIn("verified result", str(store.context("session-main", None).messages))
            self.assertTrue(any(event["type"] == "agent.subagent_task" for event in sidecar.events))
            # A later turn gets actual completed results, not a fabricated success.
            await sidecar.rpc("agent.run.start", request("lead-3", FOLLOWUP))
            self.assertIn("alpha private brief verified result", str(model.histories[FOLLOWUP]))
            self.assertRegex(str(model.histories[FOLLOWUP]), r'"state"\s*:\s*"cancelled"')
        finally:
            await sidecar.shutdown()
            store.close()

    async def test_batch_validation_idempotence_scope_and_readonly(self):
        store = ConversationStore()
        async def launch(*_args):
            await asyncio.Event().wait()
        sidecar = Sidecar(RoutingModel([]), store)
        runtime = sidecar._subagents
        parent = _parse_run_request(request("bounded", mode="readonly", focusServerId="srv_alpha"))
        runtime.activate(parent)
        context = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id="bounded",
            allowed_tools=frozenset({"subagents.start"}), allowed_target_ids=frozenset({"srv_alpha"}), approved_actions={})
        try:
            for invalid in ([task("ok"), task("wrong", "srv_beta")], [task("write", access="write")]):
                with self.assertRaises(ToolCallError):
                    await runtime._start({"tasks": invalid}, context)
                self.assertEqual(runtime.jobs, {})
            runtime.launch = launch
            first = await runtime._start({"tasks": [task("same")]}, context)
            second = await runtime._start({"tasks": [task("same")]}, context)
            self.assertEqual(first["tasks"][0]["id"], second["tasks"][0]["id"])
            self.assertEqual(len(runtime.jobs), 1)
            self.assertEqual(len(runtime.active), 1, "An accepted duplicate must not start a second job")
            with self.assertRaises(ToolCallError):
                await runtime._start({"tasks": [{**task("same"), "goal": "different"}]}, context)
            run_id = first["tasks"][0]["runId"]
            self.assertTrue(runtime.stop(run_id))
            await asyncio.sleep(0)
            await asyncio.sleep(0)
            self.assertEqual(next(iter(runtime.jobs.values()))["state"], "cancelled")
            outsider = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id=run_id,
                allowed_tools=frozenset({"subagents.start"}), allowed_target_ids=frozenset(), approved_actions={})
            with self.assertRaises(ToolCallError):
                await runtime._start({"tasks": [task("recursive")]}, outsider)
        finally:
            await sidecar.shutdown()
            store.close()

    async def test_two_subagent_approvals_are_independent_and_chat_continues_while_waiting(self):
        registry, executed = registry_with_change()
        waiting = asyncio.Event()
        store = ConversationStore()
        items = [task("approve-alpha", access="write"), task("reject-beta", "srv_beta", access="write")]
        sidecar = Sidecar(ApprovalModel(items), store, registry)
        runtime = sidecar._subagents
        original_sink = sidecar._notification_sink
        def sink(name, event):
            original_sink(name, event)
            if len([job for job in runtime.jobs.values() if job["state"] == "waiting_approval"]) == 2:
                waiting.set()
        sidecar.set_notification_sink(sink)
        try:
            await sidecar.rpc("agent.run.start", request("approve-lead"))
            await asyncio.wait_for(waiting.wait(), 3)
            jobs = list(runtime.jobs.values())
            alpha, beta = jobs
            self.assertEqual(executed, [])
            await asyncio.wait_for(sidecar.rpc("agent.run.start", request("approve-chat", FOLLOWUP)), 3)
            wrong = await sidecar.rpc("agent.approval.respond", {"approvalId": alpha["approval"]["approvalId"],
                "runId": beta["runId"], "decision": "approve_once", "respondedAt": "2026-10-17T12:00:00Z"})
            self.assertFalse(wrong["accepted"], "A decision cannot cross into another run")
            for job, decision in ((alpha, "approve_once"), (beta, "reject")):
                response = await sidecar.rpc("agent.approval.respond", {"approvalId": job["approval"]["approvalId"],
                    "runId": job["runId"], "decision": decision, "respondedAt": "2026-10-17T12:00:00Z"})
                self.assertTrue(response["accepted"])
            await asyncio.wait_for(asyncio.gather(*list(runtime.active.values())), 3)
            self.assertEqual(executed, [("approve-alpha private brief", alpha["runId"])],
                             "Only the approved subagent executes its change")
            self.assertTrue(all("approval" not in job for job in jobs))
            # A rejected call ends the run with an explanation, not a silent success:
            # the tool never ran, and the job keeps the run's terminal state.
            self.assertEqual({job["state"] for job in runtime.jobs.values()}, {"completed"})
            # The approval projection never carries the exact host input.
            self.assertNotIn("probeChange", str(sidecar.events))
        finally:
            await sidecar.shutdown()
            store.close()

    async def test_same_target_write_excludes_reads_and_other_target_still_runs(self):
        store = ConversationStore()
        started, release = {}, {}
        async def launch(job, *_args):
            started[job["title"]] = True
            await release[job["title"]].wait()
            return {"state": "completed", "text": "evidence"}
        sidecar = Sidecar(RoutingModel([]), store)
        runtime = sidecar._subagents
        parent = _parse_run_request(request("gates"))
        runtime.activate(parent)
        runtime.launch = launch
        context = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id="gates",
            allowed_tools=frozenset({"subagents.start"}), allowed_target_ids=frozenset(), approved_actions={})
        items = [task("writer", access="write"), task("reader"), task("other", "srv_beta")]
        release = {item["title"]: asyncio.Event() for item in items}
        try:
            await runtime._start({"tasks": items}, context)
            await asyncio.sleep(0.05)
            self.assertIn("writer title", started)
            self.assertIn("other title", started)
            self.assertNotIn("reader title", started)
            release["writer title"].set()
            await asyncio.sleep(0.05)
            self.assertIn("reader title", started)
        finally:
            await sidecar.shutdown()
            store.close()

    async def test_approval_and_restart_are_task_bound_and_never_replayed(self):
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "tasks.sqlite"
            store = ConversationStore(database)
            sidecar = Sidecar(RoutingModel([]), store)
            runtime = sidecar._subagents
            parent = _parse_run_request(request("persist"))
            runtime.activate(parent)
            runtime.launch = lambda *_args: asyncio.sleep(3600)
            context = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id="persist",
                allowed_tools=frozenset({"subagents.start"}), allowed_target_ids=frozenset(), approved_actions={})
            try:
                await runtime._start({"tasks": [task("approval")]}, context)
                await asyncio.sleep(0)
                job = next(iter(runtime.jobs.values()))
                runtime.observe({"type": "agent.waiting_approval", "runId": job["runId"],
                    "approval": {"approvalId": "apr", "runId": job["runId"], "hostInput": {"secret": "do-not-project"}}})
                runtime.observe({"type": "agent.usage", "runId": job["runId"], "usage": {"inputTokens": 3, "outputTokens": 2}})
                self.assertEqual(job["state"], "waiting_approval")
                self.assertIn("approval", job)
                self.assertNotIn("hostInput", str(sidecar.events))
                recovered_store = ConversationStore(database)
                recovered = PythonSidecar(ToolRegistry(MemoryAuditSink()), conversation_store=recovered_store)
                self.assertEqual(recovered._subagents.list()[0]["state"], "interrupted")
                self.assertEqual(recovered._subagents.active, {})
                self.assertNotIn("approval", recovered._subagents.list()[0])
                recovered_store.close()
            finally:
                await sidecar.shutdown()
                store.close()

    async def test_capacity_is_bounded_per_session_and_globally(self):
        store = ConversationStore()
        sidecar = Sidecar(RoutingModel([]), store)
        runtime = sidecar._subagents
        runtime.launch = lambda *_args: asyncio.sleep(3600)
        try:
            for scope in range(4):
                parent = _parse_run_request(request(f"cap-{scope}", session=f"capacity-{scope}"))
                runtime.activate(parent)
                context = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id=f"cap-{scope}",
                    allowed_tools=frozenset({"subagents.start"}), allowed_target_ids=frozenset(), approved_actions={})
                accepted = await runtime._start({"tasks": [task(f"job-{scope}-{index}") for index in range(4)]}, context)
                self.assertEqual(len(accepted["tasks"]), 4)
                accepted = await runtime._start({"tasks": [task(f"job-{scope}-{index}") for index in range(4, 8)]}, context)
                self.assertEqual(len(accepted["tasks"]), 4)
                self.assertEqual(len(runtime.active), 8 * (scope + 1))
                # The per-session cap is eight; a ninth distinct key is rejected.
                with self.assertRaises(ToolCallError) as per_session:
                    await runtime._start({"tasks": [task(f"extra-{scope}")]}, context)
                self.assertEqual(per_session.exception.code, "subagent_queue_full")
                with self.assertRaises(ToolCallError) as duplicate:
                    await runtime._start({"tasks": [task(f"job-{scope}-0"), task(f"job-{scope}-0")]}, context)
                self.assertEqual(duplicate.exception.code, "invalid_arguments",
                                 "A repeated key inside one batch is rejected, never admitted twice")
            self.assertEqual(len(runtime.active), 32)
            parent = _parse_run_request(request("cap-overflow", session="capacity-overflow"))
            runtime.activate(parent)
            context = ExecutionContext(task_id="task", tenant_id="local", user_id="user", run_id="cap-overflow",
                allowed_tools=frozenset({"subagents.start"}), allowed_target_ids=frozenset(), approved_actions={})
            with self.assertRaises(ToolCallError) as global_cap:
                await runtime._start({"tasks": [task("overflow")]}, context)
            self.assertEqual(global_cap.exception.code, "subagent_queue_full")
            self.assertEqual(len(runtime.jobs), 32)
        finally:
            await sidecar.shutdown()
            store.close()


class SubagentConfigTests(unittest.TestCase):
    def test_config_is_in_admission_fingerprint_and_invalid_config_fails(self):
        first = _parse_run_request(request("config"))
        second = _parse_run_request(request("config", subagents={"version": 1, "servers": []}))
        self.assertNotEqual(_run_fingerprint(first), _run_fingerprint(second))
        with self.assertRaises(ValueError):
            parse_subagents({"version": True, "servers": []})
        with self.assertRaises(ValueError):
            parse_subagents({"version": 1, "servers": [{"id": "bad", "name": "x", "environment": "development"}]})

    def test_large_server_list_is_bounded_json_and_keeps_the_focused_server(self):
        store = ConversationStore()
        large = {"version": 1, "servers": [
            {"id": "srv_alpha", "name": "Alpha", "environment": "development"},
            {"id": "srv_beta", "name": "Beta", "environment": "production"},
        ]}
        sidecar = PythonSidecar(ToolRegistry(MemoryAuditSink()), conversation_store=store)
        try:
            parent = _parse_run_request(request("context", focusServerId="srv_alpha", subagents=large))
            encoded = sidecar._subagents.context(parent).rsplit("\n", 1)[1]
            context = json.loads(encoded)
            self.assertEqual(context["servers"][0]["id"], "srv_alpha")
            self.assertEqual(context["currentServerId"], "srv_alpha")
            self.assertLessEqual(len(encoded), 14000)
        finally:
            store.close()


if __name__ == "__main__":
    unittest.main()
