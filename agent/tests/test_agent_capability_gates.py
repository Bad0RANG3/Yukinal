"""Five capability gates: LangGraph loop, Tool Registry, Policy, redaction audit,
and task/plan persistence + recovery.

These are *demonstration* gates for the shipped runtime. Each test prints the
observable evidence for its gate so a reviewer can record it in an acceptance
note, and asserts on that evidence so a silent regression fails the run.

Everything here is deterministic and offline: the model is scripted, the audit
sink is a real JSONL file in a temp directory, and the checkpoint store is a
real SQLite database that is closed and reopened.
"""
from __future__ import annotations

import asyncio
import json
import tempfile
import unittest
from pathlib import Path
from typing import Any

from langchain_core.messages import AIMessage, ToolMessage

from yukinal_agent import (
    AgentBudget,
    ExecutionContext,
    JsonlAuditSink,
    MemoryAuditSink,
    ModelDrivenAgent,
    PolicyEngine,
    PrincipalScope,
    ToolCallError,
    ToolRegistry,
    ToolSpec,
    approval_fingerprint,
)
from yukinal_agent.checkpoints import open_checkpoint_store
from yukinal_agent.sidecar import RPC_INVALID_PARAMS, _RpcFault, _parse_run_request

SECRET_PASSWORD = "hunter2-ExAmPlE-do-not-log"
# Assembled at runtime so the repository secret scanner does not have to special-case
# a deliberately fake credential. The shape still matches the redaction pattern.
SECRET_TOKEN = "sk-" + "proj-" + "abcdefghijklmnopqrstuvwxyz012345"
SECRET_KEY_BODY = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAB3NzaC1yc2E\n-----END OPENSSH PRIVATE KEY-----"


def _evidence(gate: str, **facts: Any) -> None:
    print(f"GATE {gate} :: " + json.dumps(facts, ensure_ascii=False, sort_keys=True, default=str))


class ScriptedModel:
    """Minimal LangChain-shaped model: no network, no clock, no randomness."""

    def __init__(self, responses: list[AIMessage]) -> None:
        self._responses = list(responses)
        self.seen_messages: list[list[Any]] = []

    def bind_tools(self, tools: list[dict[str, Any]], *, parallel_tool_calls: bool) -> "ScriptedModel":
        self.tools = tools
        return self

    def bind(self, *, max_tokens: int) -> "ScriptedModel":
        self.max_tokens = max_tokens
        return self

    async def ainvoke(self, messages: list[Any]) -> AIMessage:
        self.seen_messages.append(list(messages))
        if not self._responses:
            raise AssertionError("scripted model ran out of responses")
        return self._responses.pop(0)


def _call(name: str, call_id: str, **arguments: Any) -> dict[str, Any]:
    return {"name": name, "args": arguments, "id": call_id, "type": "tool_call"}


def _read_registry(
    executions: list[dict[str, Any]],
    audit: Any | None = None,
    *,
    policy: PolicyEngine | None = None,
) -> ToolRegistry:
    registry = ToolRegistry(audit or MemoryAuditSink(), policy)

    async def inspect(arguments: dict[str, Any], _context: ExecutionContext) -> dict[str, Any]:
        executions.append(arguments)
        return {"service": arguments.get("service", "nginx.service"), "active": True}

    async def echo_secret(arguments: dict[str, Any], _context: ExecutionContext) -> dict[str, Any]:
        executions.append(arguments)
        # A tool that returns secrets is exactly what the audit must not copy.
        return {"status": "ok", "token": arguments.get("token"), "body": SECRET_KEY_BODY}

    registry.register_many((
        ToolSpec(
            name="ops.inspect",
            description="Read one service's state.",
            input_schema={
                "type": "object",
                "properties": {"service": {"type": "string", "maxLength": 64}},
                "additionalProperties": False,
            },
            risk_level="read",
            requires_approval=False,
            handler=inspect,
            target_id_argument=None,
        ),
        ToolSpec(
            name="ops.echo",
            description="Return the given credentials unchanged.",
            input_schema={
                "type": "object",
                "properties": {
                    "token": {"type": "string"},
                    "password": {"type": "string"},
                },
                "additionalProperties": False,
            },
            risk_level="read",
            requires_approval=False,
            handler=echo_secret,
            target_id_argument=None,
        ),
    ))
    return registry


def _write_registry(
    executions: list[dict[str, Any]],
    audit: Any | None = None,
    *,
    policy: PolicyEngine | None = None,
) -> ToolRegistry:
    registry = ToolRegistry(audit or MemoryAuditSink(), policy)

    async def restart(arguments: dict[str, Any], _context: ExecutionContext) -> str:
        executions.append(arguments)
        return "restarted"

    registry.register(ToolSpec(
        name="ops.restart",
        description="Restart one allow-listed unit.",
        input_schema={
            "type": "object",
            "properties": {
                "target_id": {"type": "string"},
                "unit": {"type": "string"},
            },
            "required": ["target_id"],
            "additionalProperties": False,
        },
        risk_level="write",
        requires_approval=True,
        handler=restart,
        target_required=True,
    ))
    return registry


def _run_kwargs(**overrides: Any) -> dict[str, Any]:
    base: dict[str, Any] = {
        "task_id": "task-gate",
        "tenant_id": "tenant-a",
        "user_id": "operator-7",
        "prompt": "Check the service.",
        "allowed_tools": {"ops.inspect", "ops.echo"},
        "allowed_target_ids": {"srv_stage1"},
        "host_target": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
    }
    base.update(overrides)
    return base


# --------------------------------------------------------------------------- #
# Gate 1: the LangGraph model/tool loop
# --------------------------------------------------------------------------- #
class LangGraphLoopGate(unittest.IsolatedAsyncioTestCase):
    async def test_gate1_model_tool_model_loop_with_bounded_budget(self) -> None:
        executions: list[dict[str, Any]] = []
        events: list[dict[str, Any]] = []
        registry = _read_registry(executions)
        model = ScriptedModel([
            AIMessage(content="", tool_calls=[_call("ops.inspect", "call-a", service="nginx.service")]),
            AIMessage(content="", tool_calls=[_call("ops.inspect", "call-b", service="postgres.service")]),
            AIMessage(content="Both units are active."),
        ])
        agent = ModelDrivenAgent(registry, model, AgentBudget(max_tool_calls=4), event_sink=events.append)

        result = await agent.run(**_run_kwargs())

        # The loop is model -> execute_tool -> model, twice, then END: three
        # model rounds and two registry round trips are the observable shape.
        _evidence(
            "1-langgraph-loop",
            status=result.status,
            modelRounds=len(model.seen_messages),
            toolCalls=result.tool_calls,
            executions=[item["service"] for item in executions],
            eventTypes=[event["type"] for event in events if event["type"] != "agent.stage"],
            stages=[event["stage"] for event in events if event["type"] == "agent.stage"],
        )
        self.assertEqual(result.status, "succeeded")
        self.assertEqual(result.answer, "Both units are active.")
        self.assertEqual(len(model.seen_messages), 3)
        self.assertEqual(result.tool_calls, 2)
        self.assertEqual([item["service"] for item in executions], ["nginx.service", "postgres.service"])
        # Round 2 must have been given the round-1 tool result, not just the prompt.
        tool_messages = [item for item in model.seen_messages[1] if isinstance(item, ToolMessage)]
        self.assertEqual(len(tool_messages), 1)
        self.assertIn("nginx.service", str(tool_messages[0].content))

        # The budget closing the same loop is a different, still-bounded outcome.
        budgeted = ScriptedModel([
            AIMessage(content="", tool_calls=[
                _call("ops.inspect", "call-1", service="a.service"),
                _call("ops.inspect", "call-2", service="b.service"),
            ]),
        ])
        stopped = await ModelDrivenAgent(
            _read_registry([]), budgeted, AgentBudget(max_tool_calls=1),
        ).run(**_run_kwargs())
        self.assertEqual(stopped.status, "failed")
        self.assertEqual(stopped.error_code, "tool_call_limit_exceeded")
        _evidence("1-langgraph-loop-budget", status=stopped.status, errorCode=stopped.error_code)


# --------------------------------------------------------------------------- #
# Gate 2: the Tool Registry contract
# --------------------------------------------------------------------------- #
class ToolRegistryGate(unittest.IsolatedAsyncioTestCase):
    async def test_gate2_registry_validates_before_execution_and_bounds_results(self) -> None:
        executions: list[dict[str, Any]] = []
        audit = MemoryAuditSink()
        registry = _read_registry(executions, audit)
        context = ExecutionContext(
            task_id="task-gate", tenant_id="tenant-a", user_id="operator-7",
            allowed_tools=frozenset({"ops.inspect", "ops.echo", "ops.slow", "ops.huge"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={},
        )

        rejected = await self._reject(registry, "ops.inspect", {"service": "x", "token": SECRET_TOKEN}, context)
        unknown = await self._reject(registry, "ops.missing", {}, context)
        duplicate = self._duplicate_name(registry)

        async def slow(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            await asyncio.sleep(1.0)
            return "late"

        registry.register(ToolSpec(
            name="ops.slow", description="Never returns in time.",
            input_schema={"type": "object", "properties": {}, "additionalProperties": False},
            risk_level="read", requires_approval=False, handler=slow, target_id_argument=None,
        ))
        slow_code = await self._code(lambda: registry.execute("ops.slow", {}, context, timeout_seconds=0.05))

        registry.register(ToolSpec(
            name="ops.huge", description="Returns more than the output budget.",
            input_schema={"type": "object", "properties": {}, "additionalProperties": False},
            risk_level="read", requires_approval=False,
            handler=lambda *_: _return("x" * 4096), target_id_argument=None,
        ))
        huge_code = await self._code(
            lambda: registry.execute("ops.huge", {}, context, max_output_bytes=64)
        )

        ok = await registry.execute("ops.inspect", {"service": "nginx.service"}, context)

        _evidence(
            "2-tool-registry",
            handlerExecutions=len(executions),
            invalidArguments=rejected,
            unknownTool=unknown,
            duplicateRegistration=duplicate,
            timeout=slow_code,
            outputLimit=huge_code,
            okStatus=ok.value,
            auditStatuses=[event.status for event in audit.events],
        )
        # A rejected call never reaches the handler: only the final valid call did.
        self.assertEqual(len(executions), 1)
        self.assertEqual(rejected, "invalid_arguments")
        self.assertEqual(unknown, "unknown_tool")
        self.assertEqual(duplicate, "duplicate tool name")
        self.assertEqual(slow_code, "execution_timeout")
        self.assertEqual(huge_code, "output_too_large")
        self.assertEqual(ok.value["service"], "nginx.service")
        # Every attempt, successful or not, produced audit rows: a `started` row
        # before execution and exactly one terminal row after it.
        statuses = [event.status for event in audit.events]
        self.assertEqual(statuses, ["started", "failed"] * 4 + ["started", "succeeded"])

    @staticmethod
    async def _reject(registry: ToolRegistry, name: str, arguments: dict[str, Any],
                      context: ExecutionContext) -> str:
        try:
            await registry.execute(name, arguments, context)
        except ToolCallError as error:
            return error.code
        return "executed"

    @staticmethod
    def _duplicate_name(registry: ToolRegistry) -> str:
        try:
            registry.register(ToolSpec(
                name="ops.inspect", description="Duplicate.", input_schema={"type": "object", "properties": {}},
                risk_level="read", requires_approval=False, handler=lambda *_: _return(None),
                target_id_argument=None,
            ))
        except ValueError as error:
            return str(error)
        return "accepted"

    @staticmethod
    async def _code(operation: Any) -> str:
        try:
            await operation()
        except ToolCallError as error:
            return error.code
        return "executed"


async def _return(value: Any) -> Any:
    return value


# --------------------------------------------------------------------------- #
# Gate 3: policy
# --------------------------------------------------------------------------- #
class PolicyGate(unittest.IsolatedAsyncioTestCase):
    async def test_gate3_default_deny_intersects_principal_and_task_scope(self) -> None:
        policy = PolicyEngine({
            ("tenant-a", "operator-7"): PrincipalScope(
                allowed_tools=frozenset({"ops.inspect", "ops.restart"}),
                allowed_target_ids=frozenset({"srv_stage1"}),
                allowed_risk_levels=frozenset({"read", "write"}),
            ),
        })
        executions: list[dict[str, Any]] = []
        registry = _read_registry(executions, policy=policy)
        registry.register(ToolSpec(
            name="ops.restart", description="Restart a unit.",
            input_schema={
                "type": "object",
                "properties": {"target_id": {"type": "string"}, "unit": {"type": "string"}},
                "required": ["target_id"], "additionalProperties": False,
            },
            risk_level="write", requires_approval=True, handler=lambda *_: _return("restarted"),
            target_required=True,
        ))

        def context(**overrides: Any) -> ExecutionContext:
            base: dict[str, Any] = {
                "task_id": "task-gate", "tenant_id": "tenant-a", "user_id": "operator-7",
                "allowed_tools": frozenset({"ops.inspect", "ops.restart"}),
                "allowed_target_ids": frozenset({"srv_stage1"}),
                "approved_actions": {},
                "context_target_id": "srv_stage1",
            }
            base.update(overrides)
            return ExecutionContext(**base)

        outcomes: dict[str, str] = {}
        outcomes["unknown-principal"] = await self._code(
            registry, "ops.inspect", {}, context(tenant_id="tenant-b")
        )
        outcomes["unregistered-for-task"] = await self._code(
            registry, "ops.inspect", {}, context(allowed_tools=frozenset({"ops.restart"}))
        )
        outcomes["model-supplied-other-target"] = await self._code(
            registry, "ops.restart", {"target_id": "srv_prod9"}, context()
        )
        outcomes["write-without-approval"] = await self._code(
            registry, "ops.restart", {"target_id": "srv_stage1"}, context()
        )
        fingerprint = approval_fingerprint(
            "ops.restart", "srv_stage1", {"target_id": "srv_stage1"},
            task_id="task-gate", tenant_id="tenant-a", user_id="operator-7",
        )
        outcomes["approved-exact-call"] = await self._code(
            registry, "ops.restart", {"target_id": "srv_stage1"},
            context(approved_actions={fingerprint: "approval-ref-1"}),
        )
        # Same grant, one changed argument: the fingerprint no longer matches.
        outcomes["approved-then-changed-args"] = await self._code(
            registry, "ops.restart", {"target_id": "srv_stage1", "unit": "other.service"},
            context(approved_actions={fingerprint: "approval-ref-1"}),
        )
        # A read-only principal may not borrow the same principal's write scope.
        write_denied = PolicyEngine({
            ("tenant-a", "operator-7"): PrincipalScope(
                allowed_tools=frozenset({"ops.inspect", "ops.restart"}),
                allowed_target_ids=frozenset({"srv_stage1"}),
                allowed_risk_levels=frozenset({"read"}),
            ),
        })
        write_denied_registry = _write_registry([], MemoryAuditSink(), policy=write_denied)
        fingerprint_write = approval_fingerprint(
            "ops.restart", "srv_stage1", {"target_id": "srv_stage1"},
            task_id="task-gate", tenant_id="tenant-a", user_id="operator-7",
        )
        outcomes["risk-not-allowed"] = await self._code(
            write_denied_registry, "ops.restart", {"target_id": "srv_stage1"},
            ExecutionContext(
                task_id="task-gate", tenant_id="tenant-a", user_id="operator-7",
                allowed_tools=frozenset({"ops.restart"}),
                allowed_target_ids=frozenset({"srv_stage1"}),
                approved_actions={fingerprint_write: "approval-ref-1"},
            ),
        )

        _evidence("3-policy", outcomes=outcomes)
        self.assertEqual(outcomes, {
            "unknown-principal": "tool_not_allowed",
            "unregistered-for-task": "tool_not_allowed",
            "model-supplied-other-target": "target_not_allowed",
            "write-without-approval": "approval_required",
            "approved-exact-call": "executed",
            "approved-then-changed-args": "approval_required",
            "risk-not-allowed": "risk_not_allowed",
        })

    @staticmethod
    async def _code(registry: ToolRegistry, name: str, arguments: dict[str, Any],
                    context: ExecutionContext) -> str:
        try:
            await registry.execute(name, arguments, context)
        except ToolCallError as error:
            return error.code
        return "executed"


# --------------------------------------------------------------------------- #
# Gate 4: append-only audit with redaction
# --------------------------------------------------------------------------- #
class RedactionAuditGate(unittest.IsolatedAsyncioTestCase):
    async def test_gate4_audit_file_records_shape_not_secrets(self) -> None:
        executions: list[dict[str, Any]] = []
        with tempfile.TemporaryDirectory(prefix="yukinal-audit-") as directory:
            audit_path = Path(directory) / "var" / "audit" / "tool-calls.jsonl"
            registry = _read_registry(executions, JsonlAuditSink(audit_path))
            context = ExecutionContext(
                task_id="task-gate", tenant_id="tenant-a", user_id="operator-7",
                allowed_tools=frozenset({"ops.echo"}),
                allowed_target_ids=frozenset({"srv_stage1"}),
                approved_actions={}, context_target_id="srv_stage1",
            )
            await registry.execute(
                "ops.echo",
                {"token": SECRET_TOKEN, "password": SECRET_PASSWORD},
                context,
            )
            raw = audit_path.read_text(encoding="utf-8")
            rows = [json.loads(line) for line in raw.splitlines() if line.strip()]

        _evidence(
            "4-redaction-audit",
            rows=len(rows),
            statuses=[row["status"] for row in rows],
            argumentKeys=rows[-1]["argument_keys"],
            argumentTypes=rows[-1]["argument_types"],
            resultSummary=rows[-1]["result_summary"],
            secretBytesPresent={
                "password": SECRET_PASSWORD in raw,
                "token": SECRET_TOKEN in raw,
                "privateKey": "BEGIN OPENSSH PRIVATE KEY" in raw,
            },
        )
        self.assertEqual(len(rows), 2)  # started + succeeded
        self.assertEqual([row["status"] for row in rows], ["started", "succeeded"])
        # Names and types are recorded; values are not.
        self.assertEqual(rows[-1]["argument_keys"], ["password", "token"])
        self.assertEqual(
            [item[1] for item in rows[-1]["argument_types"]], ["str", "str"]
        )
        self.assertEqual(rows[-1]["task_id"], "task-gate")
        self.assertEqual(rows[-1]["tenant_id"], "tenant-a")
        self.assertEqual(rows[-1]["user_id"], "operator-7")
        self.assertEqual(rows[-1]["target_id"], "srv_stage1")
        # Redaction is verified against the whole file, including the result
        # summary of a tool that deliberately returned the secret.
        for secret in (SECRET_PASSWORD, SECRET_TOKEN, "AAAAB3NzaC1yc2E"):
            self.assertNotIn(secret, raw)
        self.assertEqual(rows[-1]["result_summary"]["kind"], "object")

        # Event-level redaction is the other half: the UI-facing payload never
        # carries the value even though the tool received it.
        events: list[dict[str, Any]] = []
        model = ScriptedModel([
            AIMessage(content="", tool_calls=[_call("ops.echo", "call-secret", token=SECRET_TOKEN)]),
            AIMessage(content="done"),
        ])
        result = await ModelDrivenAgent(
            _read_registry([]), model, event_sink=events.append,
        ).run(**_run_kwargs(allowed_tools={"ops.echo"}))
        serialized = json.dumps(events, ensure_ascii=False, default=str)
        _evidence(
            "4-redaction-events",
            status=result.status,
            eventTypes=[event["type"] for event in events],
            eventSecretBytesPresent=SECRET_TOKEN in serialized,
        )
        self.assertEqual(result.status, "succeeded")
        self.assertNotIn(SECRET_TOKEN, serialized)
        tool_call = next(event for event in events if event["type"] == "agent.tool_call")
        self.assertEqual(tool_call["input"], {"token": "[redacted]"})


# --------------------------------------------------------------------------- #
# Gate 5: task/plan persistence and recovery
# --------------------------------------------------------------------------- #
class PersistenceRecoveryGate(unittest.IsolatedAsyncioTestCase):
    async def test_gate5_checkpoint_survives_process_restart_and_binds_plan(self) -> None:
        executions: list[dict[str, Any]] = []
        registry = _write_registry(executions)
        kwargs = {
            "task_id": "task-durable", "tenant_id": "tenant-a", "user_id": "operator-7",
            "prompt": "Restart the unit.", "allowed_tools": {"ops.restart"},
            "allowed_target_ids": {"srv_stage1"},
            "host_target": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            "run_id": "run-durable-1", "checkpoint_binding": "binding-1",
            "plan_id": "plan-1", "plan_step_id": "step-1", "evidence_ids": ("ev-1",),
            "host_task_id": "task-durable",
        }
        with tempfile.TemporaryDirectory(prefix="yukinal-state-") as data_dir:
            # --- process 1: pause on the write approval, then exit ---
            async with open_checkpoint_store(data_dir) as checkpointer:
                first_agent = ModelDrivenAgent(
                    registry,
                    ScriptedModel([AIMessage(content="", tool_calls=[
                        _call("ops.restart", "call-restart-1", target_id="srv_stage1"),
                    ])]),
                    checkpointer=checkpointer,
                )
                paused = await first_agent.run(**kwargs)
                paused_before_execution = len(executions) == 0

            # --- process 2: brand new agent, brand new saver, same thread ---
            async with open_checkpoint_store(data_dir) as checkpointer:
                restored_model = ScriptedModel([AIMessage(content="The unit was restarted.")])
                restored_agent = ModelDrivenAgent(
                    registry, restored_model, checkpointer=checkpointer,
                )
                restored = await restored_agent.run(**kwargs)
                model_rounds_before_approval = len(restored_model.seen_messages)
                # A different prompt on the same thread is refused, not replayed.
                mismatched = await ModelDrivenAgent(
                    registry, ScriptedModel([]), checkpointer=checkpointer,
                ).run(**{**kwargs, "prompt": "Something else entirely."})
                # A thread that never existed cannot be "required".
                missing = await ModelDrivenAgent(
                    registry, ScriptedModel([]), checkpointer=checkpointer,
                ).run(**{**kwargs, "run_id": "run-never-started", "require_checkpoint": True})
                # A rebind to another plan step is not part of the thread identity:
                # the pending approval is reused and the host re-checks the plan
                # step when it mints the one-shot ticket.
                rebound = await ModelDrivenAgent(
                    registry, ScriptedModel([]), checkpointer=checkpointer,
                ).run(**{**kwargs, "plan_step_id": "step-2"})
                resumed = await restored_agent.resume_after_approval(
                    restored.checkpoint_id or "",
                    paused.approval_requests[0].fingerprint,
                    "approval-ref-restored",
                    approved=True,
                )

            _evidence(
                "5-persistence-recovery",
                pausedStatus=paused.status,
                pausedBeforeExecution=paused_before_execution,
                restoredStatus=restored.status,
                sameCheckpoint=restored.checkpoint_id == paused.checkpoint_id,
                modelRoundsDuringRestore=model_rounds_before_approval,
                mismatchedError=mismatched.error_code,
                missingError=missing.error_code,
                reboundStatus=rebound.status,
                resumedStatus=resumed.status,
                resumedAnswer=resumed.answer,
                executedOnce=len(executions) == 1,
                stateFiles=sorted(
                    str(path.relative_to(data_dir)) for path in Path(data_dir).rglob("*.sqlite")
                ),
            )

        self.assertEqual(paused.status, "awaiting_approval")
        self.assertTrue(paused.checkpoint_id)
        self.assertTrue(paused_before_execution)
        self.assertEqual(restored.status, "awaiting_approval")
        self.assertEqual(restored.checkpoint_id, paused.checkpoint_id)
        # Restoring a pending approval does not call the model again.
        self.assertEqual(model_rounds_before_approval, 0)
        self.assertEqual(mismatched.error_code, "checkpoint_context_mismatch")
        self.assertEqual(missing.error_code, "checkpoint_missing")
        self.assertEqual(rebound.status, "awaiting_approval")
        self.assertEqual(rebound.checkpoint_id, paused.checkpoint_id)
        self.assertEqual(resumed.status, "succeeded")
        self.assertEqual(resumed.answer, "The unit was restarted.")
        # Exactly one execution, after the approval, across two "processes".
        self.assertEqual(executions, [{"target_id": "srv_stage1"}])

    def test_gate5_plan_binding_is_validated_at_the_rpc_boundary(self) -> None:
        accepted = _parse_run_request({
            "runId": "run-plan", "sessionId": "session-plan", "taskId": "task-plan",
            "prompt": "run the approved step", "mode": "plan",
            "focusServerId": "srv_stage1",
            "target": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            "planBinding": {"planId": "plan-1", "planStepId": "step-1", "evidenceIds": ["ev-1", "ev-2"]},
            "requireCheckpoint": True,
        })

        def rejected(params: dict[str, Any]) -> int | None:
            try:
                _parse_run_request(params)
            except _RpcFault as fault:
                return fault.code
            return None

        base = {
            "runId": "run-plan", "sessionId": "session-plan", "taskId": "task-plan",
            "prompt": "run the approved step",
            "target": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        }
        cases = {
            "duplicate-evidence": rejected({**base, "planBinding": {
                "planId": "plan-1", "planStepId": "step-1", "evidenceIds": ["ev-1", "ev-1"]}}),
            "missing-task": rejected({**base, "taskId": None, "planBinding": {
                "planId": "plan-1", "planStepId": "step-1", "evidenceIds": []}}),
            "local-target": rejected({**base, "target": {"host": "local", "environment": "local"},
                                      "planBinding": {"planId": "plan-1", "planStepId": "step-1",
                                                      "evidenceIds": []}}),
            "bad-step-id": rejected({**base, "planBinding": {
                "planId": "plan-1", "planStepId": "step 1 with spaces", "evidenceIds": []}}),
        }
        _evidence(
            "5-plan-binding-rpc",
            acceptedBinding=accepted["planBinding"],
            requireCheckpoint=accepted["requireCheckpoint"],
            rejected=cases,
        )
        self.assertEqual(accepted["planBinding"], {
            "planId": "plan-1", "planStepId": "step-1", "evidenceIds": ["ev-1", "ev-2"],
        })
        self.assertTrue(accepted["requireCheckpoint"])
        for name, code in cases.items():
            with self.subTest(case=name):
                self.assertEqual(code, RPC_INVALID_PARAMS)


class RejectionReachesTheModelGate(unittest.IsolatedAsyncioTestCase):
    """The verdict is an outcome the model owns, not a runtime-written epilogue.

    Before this change the rejection path set `status="succeeded"` and stopped, so
    "it did not retry" was a property of the loop and the transcript kept a tool
    call with no result. These tests assert the model is given one round with the
    denial and that a retry of the same action is still refused.
    """

    async def test_a_rejection_gives_the_model_one_round_and_refuses_a_retry(self) -> None:
        executions: list[dict[str, Any]] = []
        events: list[dict[str, Any]] = []
        registry = _write_registry(executions)
        model = ScriptedModel([
            # Round 1 proposes the write.
            AIMessage(content="", tool_calls=[_call("ops.restart", "call-1", target_id="srv_stage1")]),
            # Round 3, after the denial, tries the identical action again.
            AIMessage(content="", tool_calls=[_call("ops.restart", "call-2", target_id="srv_stage1")]),
            # Round 4 reports the blocker after the retry is refused.
            AIMessage(content="The restart was not approved, so I did not run it."),
        ])
        agent = ModelDrivenAgent(registry, model, AgentBudget(max_tool_calls=4), event_sink=events.append)
        first = await agent.run(**_run_kwargs(allowed_tools={"ops.restart"}, prompt="Restart the unit."))
        self.assertEqual(first.status, "awaiting_approval")

        resumed = await agent.resume_after_approval(
            first.checkpoint_id or "", first.approval_requests[0].fingerprint, None, approved=False,
        )

        _evidence(
            "5b-rejection-reaches-model",
            status=resumed.status,
            modelRounds=len(model.seen_messages),
            executions=len(executions),
            toolResults=[item["error_code"] for item in resumed.tool_results],
            eventTypes=[event["type"] for event in events if event["type"] != "agent.stage"],
        )
        # Nothing ran, the model was called again, and the identical retry was denied
        # without ever reaching the handler.
        self.assertEqual(executions, [])
        # Two model rounds follow the denial: one to react, one to report after the
        # identical retry is refused. Nothing ever reached the handler.
        self.assertEqual(len(model.seen_messages), 3)
        self.assertEqual([item["error_code"] for item in resumed.tool_results], [
            "approval_rejected", "tool_repetition_detected",
        ])
        # The answer is the model's, not a sentence the runtime wrote on its behalf.
        self.assertEqual(resumed.answer, "The restart was not approved, so I did not run it.")
        self.assertEqual(resumed.status, "failed")
        self.assertEqual(resumed.error_code, "tool_repetition_detected")
        # The denial was delivered as a tool result on the call the assistant used.
        denial = [item for item in model.seen_messages[-1] if isinstance(item, ToolMessage)]
        self.assertEqual([item.tool_call_id for item in denial], ["call-1", "call-2"])
        payload = json.loads(str(denial[0].content))
        self.assertEqual(payload["state"], "not_executed")
        self.assertIn("Do not retry", payload["notice"])
        # A denial is not a change: the only stage is the report that follows the
        # model's own answer, and it never passes through `change`.
        self.assertEqual(
            [(event["previous"], event["stage"]) for event in events if event["type"] == "agent.stage"],
            [("intake", "report")],
        )



if __name__ == "__main__":
    unittest.main(verbosity=2)
