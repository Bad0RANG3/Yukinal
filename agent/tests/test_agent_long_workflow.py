from __future__ import annotations

import asyncio
import hashlib
import json
import os
import re
import tempfile
import time
import unittest
from collections import Counter
from pathlib import Path
from typing import Any

from langchain_core.language_models.chat_models import BaseChatModel
from langchain_core.messages import AIMessage, AIMessageChunk, HumanMessage, ToolMessage
from langchain_core.outputs import ChatGenerationChunk

from yukinal_agent.audit import MemoryAuditSink
from yukinal_agent.conversation_store import ConversationStore, DEFAULT_CONTEXT_BYTES, message_bytes
from yukinal_agent.sidecar import PythonSidecar
from yukinal_agent.tools import ToolRegistry, ToolSpec


MAIN_TURNS = 160
FACT_PATTERN = re.compile(r"(?:FACT|READBACK) ([a-z]+)=([A-Z0-9_]+)")
HEADER_PATTERN = re.compile(r"FLOW scope=(main|other|worker) turn=(\d+) tool=([01]) query=([a-z,]+|-) fresh=([01])\n")
MARKERS = {"main": "ISOLATION_MAIN_ONLY", "other": "ISOLATION_OTHER_ONLY", "worker": "ISOLATION_WORKER_ONLY"}


def _prompt(scope: str, turn: int, *, queries: str | None = None, fresh: bool = False) -> str:
    tool = scope == "main" and turn % 5 == 0
    if queries is None:
        queries = "origin,cutover,milestone" if scope == "main" and turn == MAIN_TURNS else (
            "cutover" if scope == "main" and turn in (40, 120) else (
                "milestone" if scope == "main" and turn == 140 else "-"))
    lines = [f"FLOW scope={scope} turn={turn} tool={int(tool)} query={queries} fresh={int(fresh)}"]
    if turn == 1:
        lines.append(MARKERS[scope])
        if scope == "main":
            lines.extend(["FACT origin=ORIGIN_ALPHA", "FACT cutover=BLUE"])
    if scope == "main" and turn == 96:
        lines.append("FACT cutover=GREEN")
    if scope == "main" and turn == 47:
        lines.append("FACT milestone=MILESTONE_047")
    if scope == "main" and turn == 81:
        lines.append("FACT origin=ORIGIN_BETA")
    # Each turn is a distinct, substantial task fragment, not a repeated call.
    for index in range(32):
        digest = hashlib.sha256(f"{scope}:{turn}:{index}".encode("ascii")).hexdigest()
        lines.append(f"artifact-{turn:03d}-{index:02d}: sha256={digest}; decision=inspect")
    return "\n".join(lines)


def _tool_pairs(messages: list[Any]) -> tuple[int, list[str]]:
    pending: set[str] = set()
    paired = 0
    errors: list[str] = []
    for message in messages:
        if isinstance(message, AIMessage):
            if pending:
                errors.append("assistant interrupted a pending tool exchange")
            pending = {call["id"] for call in message.tool_calls}
        elif isinstance(message, ToolMessage):
            if message.tool_call_id not in pending:
                errors.append("orphan tool result")
            else:
                paired += 1
            pending.discard(message.tool_call_id)
        elif pending:
            errors.append("user message split a tool exchange")
    if pending:
        errors.append("dangling tool calls in actual input")
    return paired, errors


def _visible_facts(messages: list[Any]) -> dict[str, str]:
    return dict(FACT_PATTERN.findall("\n".join(str(message.content) for message in messages
                                             if isinstance(message, (HumanMessage, AIMessage, ToolMessage)))))


class _InputChecker:
    """No fact cache or database access: answers use only current model input."""

    def __init__(self) -> None:
        self.observations: list[dict[str, Any]] = []
        self.violations: list[str] = []

    def check(self, messages: list[Any]) -> tuple[dict[str, Any], dict[str, Any] | None]:
        latest = max(index for index, message in enumerate(messages) if isinstance(message, HumanMessage))
        prompt = messages[latest].content
        match = HEADER_PATTERN.match(prompt) if isinstance(prompt, str) else None
        if match is None:
            raise AssertionError("The real model input has no current workflow task")
        scope, number, wants_tool, query, fresh = match.groups()
        turn = int(number)
        text = "\n".join(str(message.content) for message in messages if isinstance(message, (HumanMessage, AIMessage, ToolMessage)))
        paired, pairing_errors = _tool_pairs(messages)
        self.violations.extend(f"{scope}:{turn}: {error}" for error in pairing_errors)
        for foreign_scope, marker in MARKERS.items():
            if foreign_scope != scope and marker in text:
                self.violations.append(f"{scope}:{turn}: foreign {foreign_scope} scope leaked")
        if turn > 1 and fresh != "1" and f"ACK {scope} {turn - 1:03d}" not in text:
            self.violations.append(f"{scope}:{turn}: previous conversation turn is missing")
        facts = dict(FACT_PATTERN.findall(text))
        digest = hashlib.sha256(prompt.encode("ascii")).hexdigest()
        results = [message for message in messages[latest + 1:] if isinstance(message, ToolMessage)]
        observation = {"scope": scope, "turn": turn, "inputBytes": message_bytes(messages),
                       "messages": len(messages), "pairedTools": paired, "facts": facts,
                       "afterTool": bool(results), "fresh": fresh == "1",
                       "requestedFacts": query.split(",") if query != "-" else []}
        self.observations.append(observation)
        if wants_tool == "1" and not results:
            return observation, {"name": "fixture__inspect", "args": {"scope": scope, "turn": turn, "digest": digest},
                                 "id": f"fixture-{scope}-{turn}", "type": "tool_call"}
        if wants_tool == "1":
            payload = json.loads(results[-1].content)
            if not isinstance(payload, dict) or payload.get("digest") != digest or payload.get("turn") != turn:
                self.violations.append(f"{scope}:{turn}: current tool evidence does not match the task")
        return observation, None


class _CheckedModel(BaseChatModel):
    checker: Any

    @property
    def _llm_type(self) -> str:
        return "bounded-long-workflow-input-checker"

    def bind_tools(self, tools: Any, **kwargs: Any) -> Any:
        return self.bind(tools=tools, **kwargs)

    def _generate(self, *_args: Any, **_kwargs: Any) -> Any:
        raise AssertionError("Only the checked streaming path is allowed")

    async def _astream(self, messages: list[Any], stop: Any = None, run_manager: Any = None, **kwargs: Any) -> Any:
        observation, call = self.checker.check(messages)
        if call is not None:
            yield ChatGenerationChunk(message=AIMessageChunk(content="", tool_calls=[call]))
            return
        lines = [f"ACK {observation['scope']} {observation['turn']:03d}"]
        for key in observation["requestedFacts"]:
            value = observation["facts"].get(key)
            lines.append(f"READBACK {key}={value}" if value is not None else f"MISSING {key}")
        yield ChatGenerationChunk(message=AIMessageChunk(content="\n".join(lines)))


class _WorkflowSidecar(PythonSidecar):
    def __init__(self, store: ConversationStore, checker: _InputChecker, registry: ToolRegistry,
                 events: Counter[str]) -> None:
        self.model = _CheckedModel(checker=checker)
        super().__init__(registry, conversation_store=store,
                         notification_sink=lambda _method, event: events.update([event["type"]]))
        response = self.handle({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "1.0", "clientVersion": "long-workflow-fixture", "dataDir": ""}})
        if response is None or "error" in response:
            raise AssertionError("Sidecar initialization failed")

    async def _run_provider_model(self, _provider: Any, _model: Any) -> tuple[Any, int]:
        return self.model, 3000

    async def turn(self, scope: str, number: int, *, queries: str | None = None,
                   fresh: bool = False) -> dict[str, Any]:
        return await asyncio.wait_for(self.handle_async({"jsonrpc": "2.0", "id": 2, "method": "agent.run.start",
            "params": {"runId": f"workflow-{scope}-{number}", "sessionId": f"workflow-{scope}-session",
                       "prompt": _prompt(scope, number, queries=queries, fresh=fresh), "delivery": "sync", "resume": True,
                       "mode": "readonly", "model": "input-checker"}}), timeout=5.0)


def _fixture_registry(executions: list[dict[str, Any]]) -> ToolRegistry:
    registry = ToolRegistry(MemoryAuditSink())

    async def inspect(arguments: dict[str, Any], _context: Any) -> dict[str, Any]:
        executions.append(arguments)
        return dict(arguments)

    registry.register(ToolSpec(name="fixture.inspect", description="In-memory task checksum evidence.",
        risk_level="read", requires_approval=False, target_id_argument=None, handler=inspect,
        input_schema={"type": "object", "properties": {"scope": {"type": "string"},
            "turn": {"type": "integer"}, "digest": {"type": "string"}},
            "required": ["scope", "turn", "digest"], "additionalProperties": False}))
    return registry


class AgentLongWorkflowTests(unittest.IsolatedAsyncioTestCase):
    async def test_160_turn_product_context_workflow_isolated_durable_and_fact_checked(self) -> None:
        checker = _InputChecker()
        executions: list[dict[str, Any]] = []
        registry = _fixture_registry(executions)
        events: Counter[str] = Counter()
        contexts: list[dict[str, Any]] = []
        quality_failures: list[str] = []
        durations: list[float] = []
        reopens: list[int] = []
        started = time.perf_counter()
        summary: dict[str, Any] = {"kind": "local-strict-input-checker", "realProviderRequests": 0,
                                  "mainTurns": MAIN_TURNS, "defaultAgentBudgetUnchanged": True}
        with tempfile.TemporaryDirectory(prefix="yukinal-long-workflow-") as directory:
            database = Path(directory) / "conversation.sqlite"
            store = ConversationStore(database)
            sidecar = _WorkflowSidecar(store, checker, registry, events)
            try:
                other_turn = 0
                worker_turn = 0
                for number in range(1, MAIN_TURNS + 1):
                    before = time.perf_counter()
                    response = await sidecar.turn("main", number)
                    durations.append((time.perf_counter() - before) * 1000)
                    self.assertNotIn("error", response, f"Main turn {number} RPC failed")
                    result = response["result"]["result"]
                    self.assertEqual(result["state"], "completed", f"Main turn {number} failed")
                    self.assertIn(f"ACK main {number:03d}", result["text"])
                    contexts.append(result["context"])
                    self.assertEqual(result["context"]["totalTurns"], number - 1)
                    self.assertEqual(result["context"]["includedTurns"] + result["context"]["compactedTurns"], number - 1)
                    self.assertLessEqual(result["context"]["inputBytes"], result["context"]["maxInputBytes"])
                    if number in (40, 120, MAIN_TURNS):
                        expected = "BLUE" if number < 96 else "GREEN"
                        if f"READBACK cutover={expected}" not in result["text"]:
                            quality_failures.append(f"turn {number}: current cutover was absent or stale; received {result['text']}")
                    if number == 140 and "READBACK milestone=MILESTONE_047" not in result["text"]:
                        quality_failures.append(f"turn {number}: first recall of the middle-turn fact failed; received {result['text']}")
                    if number == MAIN_TURNS:
                        for key, value in {"origin": "ORIGIN_BETA", "milestone": "MILESTONE_047"}.items():
                            if f"READBACK {key}={value}" not in result["text"]:
                                quality_failures.append(f"turn {number}: {key} was absent or stale; received {result['text']}")
                    if number % 20 == 0:
                        other_turn += 1
                        other = await sidecar.turn("other", other_turn)
                        self.assertEqual(other["result"]["result"]["state"], "completed")
                        self.assertEqual(other["result"]["result"]["context"]["totalTurns"], other_turn - 1)
                    if number % 40 == 0:
                        worker_turn += 1
                        worker = await sidecar._launch_team({
                            "runId": f"workflow-worker-{worker_turn}", "workerSessionId": "workflow-worker-session",
                            "goal": _prompt("worker", worker_turn), "access": "read"},
                            {"mode": "readonly", "model": "input-checker", "permissionMode": "ask", "toolAccess": "none"},
                            {"id": "fixture-worker", "label": "Fixture Worker", "systemPrompt": "Inspect only the local task brief."})
                        self.assertEqual(worker["state"], "completed")
                        self.assertEqual(worker["context"]["totalTurns"], worker_turn - 1)
                    self.assertFalse(sidecar._active_runs)
                    self.assertFalse(sidecar._run_sessions)
                    self.assertFalse(sidecar._pending_approvals)
                    self.assertFalse(sidecar._session_locks)
                    self.assertFalse(sidecar._coordination.active)
                    if number in (80, 120):
                        await sidecar.shutdown()
                        store.close()
                        store = ConversationStore(database)
                        self.assertEqual(store._connection.execute(
                            "SELECT count(*) FROM conversation_turns WHERE session_id = 'workflow-main-session'").fetchone()[0], number)
                        sidecar = _WorkflowSidecar(store, checker, registry, events)
                        reopens.append(number)
                rows = store._connection.execute(
                    "SELECT status, messages_json FROM conversation_turns WHERE session_id = 'workflow-main-session' ORDER BY ordinal").fetchall()
                raw_bytes = sum(len(row["messages_json"].encode("utf-8")) for row in rows)
                self.assertEqual(len(rows), MAIN_TURNS)
                self.assertTrue(all(row["status"] == "succeeded" for row in rows))
                self.assertIn("FACT origin=ORIGIN_ALPHA", rows[0]["messages_json"])
                self.assertGreater(raw_bytes, DEFAULT_CONTEXT_BYTES * 2)
                self.assertGreater(max(context["compactedTurns"] for context in contexts), 0,
                                   "The actual default sidecar projection must compact")
                # These are product projections, not raw-DB answers supplied to
                # the checker. Their real serialized envelopes must fit, and
                # smaller budgets may omit turns only as whole tool exchanges.
                budget_probes = []
                for budget in (4096, 8192, 16384, 65536, DEFAULT_CONTEXT_BYTES):
                    projected = store.context("workflow-main-session", None, max_bytes=budget,
                                              query_text="origin milestone cutover")
                    actual_bytes = message_bytes(projected.messages)
                    self.assertEqual(projected.usage["inputBytes"], actual_bytes)
                    self.assertLessEqual(actual_bytes, budget)
                    self.assertEqual(projected.usage["includedTurns"] + projected.usage["compactedTurns"], MAIN_TURNS)
                    pairs, errors = _tool_pairs(projected.messages)
                    self.assertEqual(errors, [], f"A {budget}-byte projection split a tool exchange")
                    budget_probes.append({"budgetBytes": budget, "actualJsonBytes": actual_bytes,
                                          "pairedTools": pairs, **projected.usage})
                cutoff = store.context("workflow-main-session", None, exclude_run="workflow-main-60",
                                       max_bytes=32768, query_text="origin cutover")
                cutoff_facts = _visible_facts(cutoff.messages)
                if cutoff_facts.get("origin") != "ORIGIN_ALPHA" or cutoff_facts.get("cutover") != "BLUE":
                    quality_failures.append(f"A historical run projection lost its earlier facts: {cutoff_facts}")
                self.assertNotIn("ORIGIN_BETA", str(cutoff.messages), "A future correction leaked into an earlier run")
                self.assertNotIn("cutover=GREEN", str(cutoff.messages), "A future correction leaked into an earlier run")
                # With no explicit query, context must use the saved current
                # Human question. Turn 47 was not an opening anchor and has not
                # been read back in any earlier answer before turn 140.
                fallback = store.context("workflow-main-session", None, exclude_run="workflow-main-140",
                                         max_bytes=32768)
                fallback_facts = _visible_facts(fallback.messages)
                if fallback_facts.get("milestone") != "MILESTONE_047":
                    quality_failures.append("The saved Human query fallback did not recall the middle-turn fact")
                self.assertEqual(_tool_pairs(fallback.messages)[1], [])
                # Exercise the actual reset RPC rather than deleting rows or
                # opening a new session. Old raw turns remain persisted while
                # both FTS retrieval and recent history obey the new epoch.
                reset = await sidecar.handle_async({"jsonrpc": "2.0", "id": 3,
                    "method": "agent.context.reset", "params": {"sessionId": "workflow-main-session"}})
                self.assertEqual(reset["result"], {"reset": True})
                reset_context = store.context("workflow-main-session", None, query_text="origin milestone cutover")
                self.assertEqual(reset_context.usage["totalTurns"], 0)
                self.assertEqual(reset_context.messages, [])
                self.assertEqual(store._connection.execute(
                    "SELECT count(*) FROM conversation_turns WHERE session_id = 'workflow-main-session'").fetchone()[0], MAIN_TURNS)
                reset_result = (await sidecar.turn("main", MAIN_TURNS + 1,
                    queries="origin,milestone,cutover", fresh=True))["result"]["result"]
                self.assertEqual(reset_result["state"], "completed")
                self.assertEqual(reset_result["context"]["totalTurns"], 0)
                other_turn += 1
                other_result = (await sidecar.turn("other", other_turn,
                    queries="origin,milestone,cutover"))["result"]["result"]
                worker_turn += 1
                worker_result = await sidecar._launch_team({
                    "runId": f"workflow-worker-{worker_turn}", "workerSessionId": "workflow-worker-session",
                    "goal": _prompt("worker", worker_turn, queries="origin,milestone,cutover"), "access": "read"},
                    {"mode": "readonly", "model": "input-checker", "permissionMode": "ask", "toolAccess": "none"},
                    {"id": "fixture-worker", "label": "Fixture Worker", "systemPrompt": "Inspect only the local task brief."})
                isolation_results = {"reset": reset_result, "otherSession": other_result, "worker": worker_result}
                for label, isolated in isolation_results.items():
                    self.assertEqual(isolated["state"], "completed")
                    for key in ("origin", "milestone", "cutover"):
                        if f"MISSING {key}" not in isolated["text"]:
                            quality_failures.append(f"{label} retrieved a foreign or reset {key}: {isolated['text']}")
                self.assertEqual(len(executions), MAIN_TURNS // 5)
                self.assertEqual(events["agent.completed"], MAIN_TURNS + 1 + other_turn + worker_turn)
                self.assertEqual(events["agent.failed"], 0)
                self.assertEqual(events["agent.tool_result"], len(executions))
                self.assertLessEqual(len(sidecar._completed_runs), 256)
                self.assertFalse(sidecar._active_runs)
                self.assertFalse(sidecar._run_sessions)
                self.assertFalse(sidecar._pending_approvals)
                self.assertFalse(sidecar._session_locks)
                self.assertFalse(sidecar._coordination.active)
                summary.update({"rawTranscriptBytes": raw_bytes, "mainModelRequests": sum(
                    item["scope"] == "main" for item in checker.observations), "totalModelRequests": len(checker.observations),
                    "toolExecutions": len(executions), "otherSessionTurns": other_turn, "workerTurns": worker_turn,
                    "reopenedAfterTurns": reopens, "compactedRuns": sum(context["compactedTurns"] > 0 for context in contexts),
                    "maxCompactedTurns": max(context["compactedTurns"] for context in contexts),
                    "maxContextBytes": max(context["inputBytes"] for context in contexts),
                    "contextBudgetBytes": sorted({context["maxInputBytes"] for context in contexts}),
                    "maxModelInputBytes": max(item["inputBytes"] for item in checker.observations),
                    "middleFactIntroducedAt": 47, "middleFactFirstRequestedAt": 140,
                    "originUpdatedAt": 81, "expectedFinalOrigin": "ORIGIN_BETA",
                    "budgetProbes": budget_probes, "historicalCutoffFacts": cutoff_facts,
                    "savedHumanFallbackFacts": fallback_facts,
                    "isolationReadbacks": {label: item["text"] for label, item in isolation_results.items()},
                    "rawHistoryPreservedAfterReset": True, "resetProbeTurns": 1,
                    "pairedToolExchangesObserved": sum(item["pairedTools"] for item in checker.observations),
                    "checkerViolations": checker.violations, "qualityFailures": quality_failures,
                    "terminalEvents": dict(events), "maxTurnMs": max(durations),
                    "p95TurnMs": sorted(durations)[int(len(durations) * .95) - 1],
                    "elapsedSeconds": time.perf_counter() - started,
                    "activeRunsAtEnd": len(sidecar._active_runs), "pendingApprovalsAtEnd": len(sidecar._pending_approvals),
                    "activeSessionsAtEnd": len(sidecar._run_sessions), "sessionLocksAtEnd": len(sidecar._session_locks)})
            finally:
                await sidecar.shutdown()
                store.close()
        self.assertFalse(Path(directory).exists())
        summary["temporaryDirectoryRemoved"] = True
        summary["observations"] = checker.observations
        destination = os.environ.get("YUKINAL_LONG_WORKFLOW_EVIDENCE")
        if destination:
            Path(destination).write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
        self.assertEqual(checker.violations, [], "Actual model inputs violated workflow scope or tool pairing")
        self.assertEqual(quality_failures, [], "Actual model input lost facts required by the continuing workflow")


if __name__ == "__main__":
    unittest.main()
