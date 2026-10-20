"""Continuous, persistent multi-session workflow tests through real Agent loops.

Only model transport and provider eligibility are local fixtures. Sidecar RPC,
provider/model selection, coordination, worker admission, LangGraph, tool checks,
event finalization and SQLite persistence use production implementations.
Dependencies are explicit orchestration with teams.status, not an invented DAG
or automatic retry API. No real provider, credential service or target is used.
"""
from __future__ import annotations

import asyncio
import copy
from contextlib import closing
from dataclasses import dataclass, field
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from typing import Callable

from langchain_core.messages import AIMessageChunk, HumanMessage, ToolMessage

from yukinal_agent import MemoryAuditSink, ToolRegistry, ToolSpec
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.sidecar import PythonSidecar
from yukinal_agent.tools import ToolCallError


TERMINAL = {"completed", "failed", "cancelled", "interrupted"}


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def digest(value):
    return hashlib.sha256(encoded(value).encode()).hexdigest()


@dataclass(frozen=True)
class Work:
    identifier: str
    scope: str
    key: str
    role: str
    route: tuple[str, str]
    dependencies: tuple[str, ...] = ()
    fails: bool = False

    @property
    def nonce(self):
        return digest([self.scope, self.identifier])[:16]

    @property
    def goal(self):
        return encoded({"scope": self.scope, "work": self.identifier,
                        "nonce": self.nonce, "dependencies": self.dependencies})

    def task(self):
        return {"key": self.key, "roleId": self.role, "title": self.identifier,
                "goal": self.goal, "access": "read"}


@dataclass
class Action:
    tool: str
    arguments: dict
    check: Callable[[dict], None] | None = None


@dataclass
class Turn:
    identifier: str
    scope: str
    route: tuple[str, str]
    actions: list[Action] = field(default_factory=list)

    @property
    def prompt(self):
        return encoded({"instruction": "Coordinate independent workstreams", "control": self.identifier, "scope": self.scope})


class Oracle:
    """Independent expected data and receipts; never records system messages."""
    def __init__(self):
        self.works, self.turns, self.receipts = {}, {}, {}
        self.started, self.gates = {}, {}
        self.created, self.calls, self.executions, self.replies = [], [], [], []
        self.violations, self.events = [], []
        self.changed = asyncio.Event()
        self.checks = self.active_models = self.peak_models = 0
        self.peak_running = self.peak_admitted = 0

    def check(self, condition, message):
        self.checks += 1
        if not condition:
            self.violations.append(message)
            raise AssertionError(message)

    def add(self, work, *, held=False):
        self.check(work.identifier not in self.works, "Work identifiers must be unique")
        self.works[work.identifier] = work
        self.started[work.identifier], self.gates[work.identifier] = asyncio.Event(), asyncio.Event()
        if not held:
            self.gates[work.identifier].set()
        return work

    def publish(self, name, value, fixture):
        self.events.append({"generation": fixture.generation, **copy.deepcopy(value)})
        jobs = fixture.sidecar._coordination.jobs.values()
        self.peak_running = max(self.peak_running, sum(job["state"] == "running" for job in jobs))
        self.peak_admitted = max(self.peak_admitted, len(fixture.sidecar._coordination.active))
        self.changed.set()

    async def wait(self, predicate):
        async def find():
            while True:
                self.changed.clear()
                if predicate():
                    return
                await self.changed.wait()
        # Full Windows CI runs the same durable-workflow scenarios under much
        # heavier scheduling load than the focused local test. Keep the event
        # based assertion, but allow the workers enough time to publish their
        # terminal states before declaring a coordination failure.
        await asyncio.wait_for(find(), 30)

    def evidence(self, name, extra):
        self.check(not self.violations, "All strict prompt/model/tool checks must pass")
        summary = {"scenario": name, "strictChecks": self.checks,
                   "foregroundTurns": len(self.turns), "modelInstances": len(self.created),
                   "modelRequests": len(self.calls), "localToolExecutions": len(self.executions),
                   "successfulReceipts": len(self.receipts), "peakWorkerModels": self.peak_models,
                   "peakRunningJobs": self.peak_running, "peakAdmittedJobs": self.peak_admitted,
                   "externalRequests": 0, "violations": [], **extra}
        folder = os.environ.get("YUKINAL_LARGE_QA_EVIDENCE")
        if folder:
            target = Path(folder) / "large-coordination-summary.json"
            target.parent.mkdir(parents=True, exist_ok=True)
            content = json.loads(target.read_text(encoding="utf-8")) if target.exists() else {}
            content[name] = summary
            target.write_text(json.dumps(content, indent=2) + "\n", encoding="utf-8")
        print("LARGE_WORKFLOW_QA " + encoded(summary))


class ProviderSnapshot:
    # Independent eligible connections exercise selection without changing the
    # production store's single-active-provider policy or reading its database.
    def list_ai(self):
        return [{"id": identifier, "kind": "openai-compatible", "label": identifier,
                 "baseUrl": f"http://127.0.0.1:{port}/{identifier}", "model": f"{identifier}-default", "enabled": True}
                for identifier, port in (("alpha", 1), ("beta", 2))]

    def normalize_ai_state(self, usable_ids):
        return [{**item, "enabled": item["id"] in usable_ids} for item in self.list_ai()]


class ModelFactory:
    def __init__(self, oracle, generation):
        self.oracle, self.generation = oracle, generation

    def create_with_api_key(self, config, api_key):
        self.oracle.check(api_key is None, "Local workflow fixtures must never receive credentials")
        provider = next(item["id"] for item in ProviderSnapshot().list_ai() if item["baseUrl"] == config.base_url)
        model = ScriptedModel(self.oracle, (provider, config.model), self.generation)
        self.oracle.created.append({"generation": self.generation, "route": model.route, "identity": id(model)})
        return model


class ScriptedModel:
    def __init__(self, oracle, route, generation):
        self.oracle, self.route, self.generation = oracle, route, generation
        self.tools, self.cursor, self.pending = set(), 0, None

    def bind_tools(self, tools, *, parallel_tool_calls):
        self.oracle.check(parallel_tool_calls is True, "Provider can propose batches; only safe reads may overlap")
        self.tools = {item["function"]["name"] for item in tools}
        return self

    def bind(self, **kwargs):
        self.oracle.check(kwargs.get("max_tokens", 0) > 0, "Each actual model stays output bounded")
        return self

    def response(self, messages, tool, call_id):
        last = messages[-1]
        self.oracle.check(isinstance(last, ToolMessage), "A model continuation must receive an actual tool message")
        self.oracle.check(last.name == tool.replace(".", "__") and last.tool_call_id == call_id,
                          "Tool name and call identity must match the preceding model request")
        result = json.loads(last.content)
        self.oracle.replies.append({"generation": self.generation, "tool": tool, "callId": call_id, "digest": digest(result)})
        return result

    async def astream(self, messages):
        try:
            async for chunk in self._stream(messages):
                yield chunk
        except Exception as error:
            # Real model loops deliberately sanitize transport errors. Keep
            # synthetic-fixture failures visible without recording messages.
            self.oracle.violations.append(f"Local fixture {type(error).__name__}: {error}")
            raise

    async def _stream(self, messages):
        humans = [message.content for message in messages if isinstance(message, HumanMessage)]
        self.oracle.check(bool(humans), "Every invocation needs a public user prompt")
        prompt = humans[-1]
        self.oracle.calls.append({"generation": self.generation, "route": self.route, "promptHash": digest(prompt)})
        if "teams__dispatch" in self.tools:
            turn = self.oracle.turns[prompt]
            self.oracle.check(self.route == turn.route, f"Foreground model route changed in {turn.identifier}")
            for value in humans:
                if value.startswith("本轮状态快照，仅供参考"):
                    snapshot = json.loads(value.rsplit("\n", 1)[-1])
                    self.oracle.check(isinstance(snapshot.get("tasks"), list) and isinstance(snapshot.get("roles"), list),
                                      "Coordinator status is structured reference data outside the stable system prefix")
                    continue
                if value.startswith("Historical incomplete turn ("):
                    # Failed calls intentionally replay as non-authorizing
                    # references. Validate their original public turn identity.
                    header, source = value.split("\n", 1)
                    self.oracle.check("no approval is granted by this record" in header,
                                      "An incomplete historical call cannot grant approval")
                    self.oracle.check(source.startswith(("User: ", "User goal: ")), "Historical references retain their public user source")
                    value = source.removeprefix("User: ").removeprefix("User goal: ").split("\n", 1)[0]
                public = json.loads(value)
                self.oracle.check(public.get("scope") == turn.scope and "control" in public,
                                  "Foreground history must not contain another session or worker's brief")
                self.oracle.check(value in self.oracle.turns, "Historical public prompts must belong to real foreground turns")
            if self.pending is not None:
                action, call_id = self.pending
                result = self.response(messages, action.tool, call_id)
                if action.check:
                    action.check(result)
                self.pending = None
            if self.cursor < len(turn.actions):
                action = turn.actions[self.cursor]
                call_id = f"{turn.identifier}-call-{self.cursor}"
                self.cursor += 1
                self.pending = action, call_id
                self.oracle.check(action.tool.replace(".", "__") in self.tools, "Coordinator may call only registered tools")
                yield AIMessageChunk(content="", tool_calls=[{"name": action.tool.replace(".", "__"),
                    "args": action.arguments, "id": call_id, "type": "tool_call"}])
                return
            yield AIMessageChunk(content=encoded({"turn": turn.identifier, "checkedReplies": self.cursor}))
            return

        public = json.loads(prompt)
        work = self.oracle.works[public["work"]]
        self.oracle.check(humans == [work.goal], f"Worker context is not isolated for {work.identifier}")
        self.oracle.check(self.route == work.route, f"Worker model route changed for {work.identifier}")
        self.oracle.check(self.tools == {"qa__evidence"}, "Workers must not acquire recursive dispatch or unrelated tools")
        call_id = f"evidence-{work.identifier}"
        if self.cursor == 0:
            self.cursor = 1
            self.oracle.active_models += 1
            self.oracle.peak_models = max(self.oracle.peak_models, self.oracle.active_models)
            self.oracle.started[work.identifier].set()
            self.oracle.changed.set()
            try:
                await self.oracle.gates[work.identifier].wait()
                self.oracle.check(all(item in self.oracle.receipts for item in work.dependencies),
                                  "A dependent worker cannot invent missing evidence")
                inputs = {item: self.oracle.receipts[item]["digest"] for item in work.dependencies}
                yield AIMessageChunk(content="", tool_calls=[{"name": "qa__evidence", "args": {
                    "work": work.identifier, "nonce": work.nonce, "inputs": inputs},
                    "id": call_id, "type": "tool_call"}])
            finally:
                self.oracle.active_models -= 1
            return
        self.oracle.check(self.cursor == 1, "A worker must not repeat an evidence call")
        receipt = self.response(messages, "qa.evidence", call_id)
        self.oracle.check(receipt == self.oracle.receipts[work.identifier], "Worker must use the exact real tool receipt")
        self.cursor += 1
        yield AIMessageChunk(content=encoded({"work": work.identifier, "proof": receipt["digest"]}))


def roles(version):
    bindings = {
        1: ({"providerId": "alpha", "model": "work-v1"}, {"providerId": "beta", "model": "work-v1"}),
        2: ({"model": "work-v2"}, {"providerId": "alpha", "model": "work-v2"}),
        3: ({"providerId": "beta"}, {"model": "work-v3"}),
    }[version]
    return [{"id": identifier, "team": "blue", "label": identifier,
             "systemPrompt": "Perform only the assigned local fixture task.", **binding}
            for identifier, binding in zip(("reader", "reviewer", "inherited"), (*bindings, {}))]


class Fixture:
    def __init__(self, database, oracle, generation):
        self.oracle, self.generation, self.closed = oracle, generation, False
        self.request_id = 0
        self.store = ConversationStore(database)
        registry = ToolRegistry(MemoryAuditSink())
        registry.register(ToolSpec(name="qa.evidence", description="Pure local synthetic evidence receipt",
            input_schema={"type": "object", "additionalProperties": False,
                "required": ["work", "nonce", "inputs"], "properties": {
                    "work": {"type": "string"}, "nonce": {"type": "string"},
                    "inputs": {"type": "object", "additionalProperties": {"type": "string"}}}},
            risk_level="read", requires_approval=False, target_id_argument=None, handler=self.evidence))
        self.sidecar = PythonSidecar(registry, conversation_store=self.store, provider_store=ProviderSnapshot(),
            model_factory=ModelFactory(oracle, generation), notification_sink=lambda name, event: oracle.publish(name, event, self))
        response = self.sidecar.handle({"jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {"protocolVersion": "1.0", "clientVersion": "large-local-qa", "dataDir": ""}})
        oracle.check("error" not in response, "Real sidecar must initialize")

    def jobs(self, scope=None):
        return [job for job in self.sidecar._coordination.jobs.values() if scope is None or job["sessionId"] == scope]

    def job(self, identifier):
        return next(job for job in self.jobs() if job["title"] == identifier)

    async def evidence(self, arguments, context):
        work = self.oracle.works[arguments["work"]]
        job = self.job(work.identifier)
        self.oracle.check(job["runId"] == context.run_id and job["sessionId"] == work.scope and job["goal"] == work.goal,
                          "Tool execution must belong to the exact persisted worker/run/scope/brief")
        self.oracle.check(arguments["nonce"] == work.nonce, "Tool input nonce must match the immutable brief")
        self.oracle.check(not context.allowed_target_ids, "Pure computation must not gain a target scope")
        inputs = {item: self.oracle.receipts[item]["digest"] for item in work.dependencies}
        self.oracle.check(arguments["inputs"] == inputs, "Tool inputs must equal actual prerequisite receipt hashes")
        self.oracle.check(all(self.job(item)["state"] == "completed" for item in work.dependencies),
                          "Dependencies must have actual completed job states, not just dispatch acknowledgements")
        self.oracle.check(not any(item["work"] == work.identifier for item in self.oracle.executions),
                          "An admitted work item may execute evidence only once")
        self.oracle.executions.append({"work": work.identifier, "runId": context.run_id,
                                      "scope": work.scope, "inputs": inputs, "failed": work.fails})
        if work.fails:
            raise ToolCallError("fixture_failure", "Expected local evidence failure; explicitly schedule a new key to retry")
        receipt = {"work": work.identifier, "nonce": work.nonce, "inputs": inputs}
        receipt["digest"] = digest(receipt)
        self.oracle.receipts[work.identifier] = receipt
        return receipt

    async def rpc(self, method, params):
        self.request_id += 1
        response = await self.sidecar.handle_async({"jsonrpc": "2.0", "id": self.request_id, "method": method, "params": params})
        self.oracle.check(isinstance(response, dict), f"Real sidecar must reply to numbered {method}")
        self.oracle.check("error" not in response, f"Unexpected RPC admission error in {method}")
        return response["result"]

    def dispatch(self, works):
        def checked(reply):
            self.oracle.check(reply.get("accepted") is True, "Dispatch acknowledgement must come from the real tool")
            self.oracle.check({item["title"] for item in reply["tasks"]} == {item.identifier for item in works},
                              "Dispatch may neither omit nor invent tasks")
            for item in reply["tasks"]:
                job = self.job(item["title"])
                self.oracle.check((item["id"], item["runId"]) == (job["id"], job["runId"]),
                                  "Dispatch reply identities must match durable jobs")
        return Action("teams.dispatch", {"tasks": [item.task() for item in works]}, checked)

    def status(self, scope, expected):
        def checked(reply):
            self.oracle.check({item["title"] for item in reply["tasks"]} == set(expected), "Status must contain exactly this scope's real jobs")
            for item in reply["tasks"]:
                self.oracle.check(item["sessionId"] == scope and item["state"] in expected[item["title"]],
                                  "Status must preserve session ownership and actual terminal state")
                if item["state"] == "completed":
                    expected_proof = {"work": item["title"], "proof": self.oracle.receipts[item["title"]]["digest"]}
                    self.oracle.check(json.loads(item["result"]) == expected_proof, "A dependency must carry real completed tool evidence")
        return Action("teams.status", {}, checked)

    async def run(self, scope, label, role_config, actions, *, route=("alpha", "lead-v1"), failure=None):
        identifier = f"{self.generation}-{label}"
        turn = Turn(identifier, scope, route, actions)
        self.oracle.check(turn.prompt not in self.oracle.turns, "Every foreground turn must have a fresh identity")
        self.oracle.turns[turn.prompt] = turn
        result = await asyncio.wait_for(self.rpc("agent.run.start", {
            "runId": identifier, "sessionId": scope, "prompt": turn.prompt, "delivery": "sync",
            "providerId": route[0], "model": route[1],
            "coordination": {"version": 1, "roles": role_config, "servers": []}}), 8)
        self.oracle.check(result["result"]["state"] == ("failed" if failure else "completed"),
                          f"Unexpected foreground terminal state in {label}: {result['result'].get('error')}; checks={self.oracle.violations[-3:]}")
        if failure:
            self.oracle.check(result["result"].get("error") == failure, "Failure must retain the exact policy/admission code")
        return result

    async def wait_terminal(self, works):
        await self.oracle.wait(lambda: all(self.job(item.identifier)["state"] in TERMINAL for item in works))

    async def drain(self):
        active = list(self.sidecar._coordination.active.values())
        if active:
            await asyncio.wait_for(asyncio.gather(*active, return_exceptions=True), 8)

    async def close(self):
        if not self.closed:
            await self.sidecar.shutdown()
            self.store.close()
            self.closed = True


class LargeCoordinationTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        # Avoid asyncio's slow-callback repr of internal message objects in
        # evidence logs; assertions and bounded waits still diagnose failures.
        asyncio.get_running_loop().slow_callback_duration = 10
        self.temporary = tempfile.TemporaryDirectory(prefix="yukinal-large-qa-")
        self.addCleanup(self.temporary.cleanup)
        self.oracle = Oracle()
        self.fixture = Fixture(Path(self.temporary.name) / "workflow.sqlite", self.oracle, "live")
        self.addAsyncCleanup(self.fixture.close)

    async def test_continuous_dependency_pipeline_config_changes_failure_retry_and_parallel_sessions(self):
        fixture, oracle = self.fixture, self.oracle
        initial_roles = roles(1)
        first = [oracle.add(Work(f"a-{index}", "flow-a", f"chunk-{index}", "reader",
                                ("alpha", "work-v1"), fails=index == 6), held=True) for index in range(8)]
        pending = {item.identifier: {"queued", "running"} for item in first}
        await fixture.run("flow-a", "collect", initial_roles,
                          [fixture.dispatch(first[:4]), fixture.dispatch(first[4:]), fixture.status("flow-a", pending)])
        await oracle.wait(lambda: sum(oracle.started[item.identifier].is_set() for item in first) == 3)
        self.assertEqual(len(fixture.sidecar._coordination.active), 8)
        self.assertEqual(sum(job["state"] == "running" for job in fixture.jobs()), 3)
        # Mutating a later role configuration must not mutate already admitted jobs.
        initial_roles[0]["model"] = "work-v2"
        overflow = oracle.add(Work("a-overflow", "flow-a", "overflow", "reader", ("alpha", "work-v2")))
        await fixture.run("flow-a", "full-session", roles(2), [fixture.dispatch([overflow])],
                          route=("alpha", "lead-v2"), failure="team_queue_full")
        self.assertEqual(len(fixture.jobs("flow-a")), 8)
        self.assertNotIn(overflow.identifier, oracle.receipts)
        second = [oracle.add(Work(f"b-{index}", "flow-b", f"chunk-{index}", "reviewer",
                                 ("alpha", "work-v2")), held=True) for index in range(3)]
        await fixture.run("flow-b", "parallel-chat", roles(2), [fixture.dispatch(second)], route=("beta", "lead-v2"))
        self.assertTrue(all(fixture.job(item.identifier)["state"] == "queued" for item in second))
        await fixture.run("flow-b", "cross-scope-stop", roles(2), [Action("teams.cancel", {"taskId": fixture.job("a-0")["id"]})],
                          route=("beta", "lead-v2"), failure="task_not_found")
        cancelled = fixture.job("a-7")
        def stop_checked(reply):
            oracle.check(reply["stopRequested"] is True and reply["taskId"] == cancelled["id"], "Cancellation needs its real scoped task identity")
        await fixture.run("flow-a", "cancel-queued", roles(2), [Action("teams.cancel", {"taskId": cancelled["id"]}, stop_checked)],
                          route=("alpha", "lead-v2"))
        await oracle.wait(lambda: cancelled["state"] == "cancelled")
        for item in first[:7]:
            oracle.gates[item.identifier].set()
        await fixture.wait_terminal(first)
        self.assertEqual(fixture.job("a-6")["state"], "failed")
        self.assertEqual({item["work"] for item in oracle.executions}, {item.identifier for item in first[:7]})
        self.assertNotIn("a-7", oracle.receipts)
        before = len(oracle.executions)
        await fixture.run("flow-a", "no-implicit-replay", roles(3), [fixture.dispatch([first[6]])], route=("beta", "lead-v3"))
        self.assertEqual(len(oracle.executions), before)
        self.assertEqual(fixture.job("a-6")["state"], "failed")
        retried = oracle.add(Work("a-6-retry", "flow-a", "retry-chunk-6", "reader", ("beta", "beta-default"), ("a-0", "a-1")))
        first_states = {item.identifier: {"cancelled" if index == 7 else "failed" if index == 6 else "completed"}
                        for index, item in enumerate(first)}
        await fixture.run("flow-a", "retry-with-evidence", roles(3),
                          [fixture.status("flow-a", first_states), fixture.dispatch([retried])], route=("beta", "lead-v3"))
        await fixture.run("flow-b", "still-responsive", roles(3),
                          [fixture.status("flow-b", {item.identifier: {"queued", "running"} for item in second})],
                          route=("beta", "lead-v3"))
        for item in second:
            oracle.gates[item.identifier].set()
        await fixture.wait_terminal([*second, retried])
        completed = oracle.add(Work("a-final", "flow-a", "final", "inherited", ("beta", "lead-v3"),
                                   ("a-6-retry", "a-2", "a-3", "a-4", "a-5")))
        before_final = {**first_states, retried.identifier: {"completed"}}
        await fixture.run("flow-a", "assemble", roles(3), [fixture.status("flow-a", before_final), fixture.dispatch([completed])],
                          route=("beta", "lead-v3"))
        await fixture.wait_terminal([completed])
        await fixture.run("flow-a", "verify-final", roles(3),
                          [fixture.status("flow-a", {**before_final, completed.identifier: {"completed"}})], route=("alpha", "lead-v2"))
        await fixture.drain()
        self.assertEqual(len(oracle.executions), 12)
        self.assertEqual(len(oracle.receipts), 11)
        self.assertEqual(oracle.peak_models, 3)
        self.assertEqual(oracle.peak_running, 3)
        self.assertEqual(fixture.sidecar._run_sessions, {})
        self.assertEqual(fixture.sidecar._active_runs, {})
        self.assertEqual(len(fixture.store.team_tasks()), 13)
        self.assertEqual(len({job["workerSessionId"] for job in fixture.jobs()}), 13)
        failed_events = [event for event in oracle.events if event["type"] in {"agent.completed", "agent.failed"}
                         and event["runId"] == fixture.job("a-6")["runId"]]
        self.assertEqual(len(failed_events), 1)
        self.assertEqual(failed_events[0]["type"], "agent.failed")
        self.assertEqual(failed_events[0]["error"], "fixture_failure")
        oracle.evidence("dependency_pipeline", {"durableJobs": 13, "dependencyEdges": 7,
                        "roleConfigurations": 3, "scopes": 2, "rejectedOverflow": 1,
                        "failedThenExplicitNewKey": True, "crossScopeCancelRejected": True})

    async def test_global_capacity_crash_snapshot_restart_and_explicit_evidence_continuation(self):
        fixture, oracle = self.fixture, self.oracle
        original = []
        for scope_index in range(4):
            scope = f"capacity-{scope_index}"
            route = ("alpha", f"lead-v{1 + scope_index % 2}")
            group = [oracle.add(Work(f"c{scope_index}-{index}", scope, f"chunk-{index}", "inherited", route), held=True)
                     for index in range(8)]
            original.extend(group)
            await fixture.run(scope, f"fill-{scope_index}", roles(1 + scope_index % 2),
                              [fixture.dispatch(group[:4]), fixture.dispatch(group[4:])], route=route)
        await oracle.wait(lambda: sum(event.is_set() for event in oracle.started.values()) == 3)
        self.assertEqual(len(fixture.sidecar._coordination.active), 32)
        extra = oracle.add(Work("fifth-work", "capacity-4", "fifth-key", "reader", ("beta", "beta-default")), held=True)
        await fixture.run("capacity-4", "global-full", roles(3), [fixture.dispatch([extra])],
                          route=("beta", "lead-v3"), failure="team_queue_full")
        self.assertEqual(len(fixture.jobs()), 32)
        cancelled = fixture.job("c0-7")
        await fixture.run("capacity-0", "release-one-slot", roles(2), [Action("teams.cancel", {"taskId": cancelled["id"]})],
                          route=("alpha", "lead-v2"))
        await oracle.wait(lambda: cancelled["state"] == "cancelled" and len(fixture.sidecar._coordination.active) == 31)
        await fixture.run("capacity-4", "use-released-slot", roles(3), [fixture.dispatch([extra])], route=("beta", "lead-v3"))
        self.assertEqual(len(fixture.sidecar._coordination.active), 32)
        self.assertEqual(len(fixture.jobs()), 33)
        self.assertEqual(oracle.executions, [])
        snapshot = Path(self.temporary.name) / "committed-crash-snapshot.sqlite"
        with closing(sqlite3.connect(snapshot)) as destination:
            fixture.store._connection.backup(destination)
        # Snapshot actual committed running/queued data before graceful cleanup.
        with closing(sqlite3.connect(snapshot)) as saved:
            persisted = [json.loads(row[0]) for row in saved.execute("SELECT payload_json FROM coordinator_team_tasks")]
        self.assertEqual(sum(item["state"] not in TERMINAL for item in persisted), 32)
        self.assertEqual(sum(item["state"] == "running" for item in persisted), 3)
        await fixture.close()
        self.assertEqual(oracle.active_models, 0)
        created_before = len(oracle.created)
        restored = Fixture(snapshot, oracle, "restored")
        self.addAsyncCleanup(restored.close)
        self.assertEqual(len(oracle.created), created_before)
        self.assertEqual({item["state"] for item in restored.jobs()}, {"interrupted", "cancelled"})
        self.assertEqual(sum(item["state"] == "interrupted" for item in restored.jobs()), 32)
        self.assertEqual(restored.sidecar._coordination.active, {})
        old = original[0]
        old_id = restored.job(old.identifier)["id"]
        old_status = {item.identifier: {"cancelled" if item.identifier == "c0-7" else "interrupted"} for item in original[:8]}
        await restored.run("capacity-0", "inspect-not-replay", roles(3),
                           [restored.status("capacity-0", old_status), restored.dispatch([old])], route=("beta", "lead-v3"))
        self.assertEqual(oracle.executions, [])
        self.assertEqual(restored.job(old.identifier)["id"], old_id)
        resumed = [oracle.add(Work(f"recovered-{index}", "capacity-0", f"explicit-new-key-{index}", "reader",
                                  ("beta", "beta-default"))) for index in range(2)]
        await restored.run("capacity-0", "explicit-new-work", roles(3), [restored.dispatch(resumed)], route=("beta", "lead-v3"))
        await restored.wait_terminal(resumed)
        final = oracle.add(Work("recovered-final", "capacity-0", "verified-final-key", "reviewer",
                               ("alpha", "work-v2"), tuple(item.identifier for item in resumed)))
        await restored.run("capacity-0", "continue-from-real-evidence", roles(2),
                           [restored.status("capacity-0", {**old_status, **{item.identifier: {"completed"} for item in resumed}}),
                            restored.dispatch([final])], route=("alpha", "lead-v2"))
        await restored.wait_terminal([final])
        await restored.drain()
        self.assertEqual(len(oracle.executions), 3)
        self.assertEqual(len(oracle.receipts), 3)
        self.assertEqual(restored.job(old.identifier)["state"], "interrupted")
        self.assertEqual(len(restored.store.team_tasks()), 36)
        self.assertEqual(len({job["workerSessionId"] for job in restored.jobs()}), 36)
        self.assertEqual(restored.sidecar._run_sessions, {})
        self.assertEqual(restored.sidecar._active_runs, {})
        self.assertEqual(oracle.peak_models, 3)
        self.assertEqual(oracle.peak_running, 3)
        await restored.close()
        reopened = ConversationStore(snapshot)
        try:
            rows = reopened.team_tasks()
            self.assertEqual(sum(item["state"] == "completed" for item in rows), 3)
            self.assertEqual(sum(item["state"] == "interrupted" for item in rows), 32)
            self.assertEqual(sum(item["state"] == "cancelled" for item in rows), 1)
        finally:
            reopened.close()
        oracle.evidence("capacity_restart", {"durableJobs": 36, "scopes": 5,
                        "capacityLimit": 32, "runningLimit": 3, "snapshotNonterminal": 32,
                        "recoveredWithoutReplay": 32, "explicitNewKeyExecutions": 3,
                        "dependencyEdges": 2, "queueSlotReusedAfterCancel": True})


if __name__ == "__main__":
    unittest.main()
