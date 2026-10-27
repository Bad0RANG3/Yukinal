"""Regression tests for the four defects the live acceptance run exposed.

Each test names the observation it pins, so a future change that reintroduces the
behaviour fails with the operator-visible symptom instead of a bare assertion.
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
    ToolCallError,
    ToolRegistry,
    ToolSpec,
)
from yukinal_agent.command_effect import is_read_only_command
from yukinal_agent.read_commands import (
    exists_command,
    validate_exists_path,
)
from yukinal_agent.agent_stage import derive_stage


class CommandEffectTests(unittest.TestCase):
    def test_pure_readers_are_recognised(self) -> None:
        for command in (
            "set -e\ndate -Is",
            "set -e\ndf -i -x tmpfs -x devtmpfs",
            "test -e /tmp/x && echo PRESENT || echo ABSENT",
            "cat /etc/os-release",
            "systemctl",  # a bare reader is still a reader? no: excluded below
        ):
            with self.subTest(command=command):
                expected = command != "systemctl"
                self.assertEqual(is_read_only_command(command), expected)

    def test_effects_and_ambiguity_fail_closed(self) -> None:
        for command in (
            "set -e\nrm -f /tmp/yukinal-probe.txt",
            "set -e\nsystemctl restart nginx",
            "set -e\nfind / -delete",
            "set -e\njournalctl --vacuum-size=10M",
            "df -hT | tee /tmp/out",
            "df -hT > /tmp/out",
            "cat $(echo /etc/passwd)",
            # The classifier is conservative by design: a quoted `|` is not
            # resolved, so this real-world read stays effectful rather than
            # something less safe being reported as a read.
            "grep -E '^(SwapTotal|SwapFree):' /proc/meminfo",
            "X=1 df -hT",
            "",
            None,
        ):
            with self.subTest(command=command):
                self.assertFalse(is_read_only_command(command))

    def test_a_recognised_read_never_reaches_change_stage(self) -> None:
        # The live run reported `change` because `date -Is` executed before any
        # write. Deriving from a read-only classification keeps evidence/evidence.
        stage = derive_stage("evidence", tool="server.exec", risk="read", succeeded=True)
        self.assertEqual(stage, "evidence")

    def test_a_real_effect_still_reaches_change_stage(self) -> None:
        self.assertEqual(derive_stage("evidence", tool="server.exec", risk="destructive", succeeded=True), "change")


class FixedReadCommandTests(unittest.TestCase):
    def test_exists_rejects_injection_and_relative_paths(self) -> None:
        for path in (
            "/tmp/'; rm -rf / #",
            "/tmp/x`id`",
            "/tmp/../etc/shadow",
            "/relative/path",
            "",
            None,
            "/tmp/x y",
        ):
            with self.subTest(path=path):
                with self.assertRaises(ValueError):
                    validate_exists_path(path)

    def test_exists_accepts_one_allowlisted_absolute_path(self) -> None:
        self.assertEqual(validate_exists_path("/tmp/yukinal-probe.txt"), "/tmp/yukinal-probe.txt")
        self.assertEqual(
            exists_command("/tmp/yukinal-probe.txt"),
            "if [ -e '/tmp/yukinal-probe.txt' ]; then echo PRESENT; else echo ABSENT; fi",
        )

    def test_fixed_reads_are_registered_as_approval_free_reads(self) -> None:
        from yukinal_agent.host_tools import register_host_tools

        class Host:
            async def request(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
                return {"status": "success", "output": {
                    "state": "completed", "exitCode": 0, "stdout": "SwapTotal: 0 kB\n",
                    "stderr": "", "stdoutTruncated": False, "stderrTruncated": False,
                }}

        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, Host())  # type: ignore[arg-type]
        specs = {spec.name: spec for spec in registry.list()}
        for name in ("system.swap", "filesystem.inodes", "filesystem.mounts", "system.time", "filesystem.exists"):
            with self.subTest(name=name):
                self.assertIn(name, specs)
                self.assertEqual(specs[name].risk_level, "read")
                self.assertFalse(specs[name].requires_approval)
        # `server.exec` keeps its destructive declaration: authorization unchanged.
        self.assertEqual(specs["server.exec"].risk_level, "destructive")
        self.assertTrue(specs["server.exec"].requires_approval)


def _context(**overrides: Any) -> ExecutionContext:
    base: dict[str, Any] = {
        "task_id": "task-1", "tenant_id": "local", "user_id": "session-1",
        "allowed_tools": frozenset({"ops.read", "ops.write"}),
        "allowed_target_ids": frozenset({"srv_1"}),
        "approved_actions": {}, "context_target_id": "srv_1",
    }
    base.update(overrides)
    return ExecutionContext(**base)


class AuditDecisionRowTests(unittest.IsolatedAsyncioTestCase):
    async def test_a_rejected_approval_leaves_a_rejected_audit_row(self) -> None:
        audit = MemoryAuditSink()
        registry = ToolRegistry(audit)

        async def handler(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            return "written"

        registry.register(ToolSpec(
            name="ops.write", description="Write one value.",
            input_schema={"type": "object", "properties": {"value": {"type": "string"}},
                          "additionalProperties": False},
            risk_level="write", requires_approval=True, handler=handler, target_id_argument=None,
        ))
        with self.assertRaises(ToolCallError) as raised:
            await registry.execute("ops.write", {"value": "x"}, _context(), call_id="call-abc")
        self.assertEqual(raised.exception.code, "approval_required")

        await registry.audit_decision(
            run_id="run-1", call_id="call-abc", tool_name="ops.write",
            target_id="srv_1", status="rejected",
        )
        rows = [(event.status, event.call_id, event.approval_status) for event in audit.events]
        # Without the verdict row the trail stopped at awaiting_approval and the
        # decision was only recoverable from the conversation transcript.
        self.assertEqual(rows, [
            ("started", "call-abc", "not_required"),
            ("awaiting_approval", "call-abc", "awaiting_approval"),
            ("rejected", "call-abc", "rejected"),
        ])


class _ScriptedModel:
    def __init__(self, responses: list[AIMessage]) -> None:
        self._responses = list(responses)

    def bind_tools(self, *_args: Any, **_kwargs: Any) -> "_ScriptedModel":
        return self

    def bind(self, **_kwargs: Any) -> "_ScriptedModel":
        return self

    async def ainvoke(self, _messages: list[Any]) -> AIMessage:
        return self._responses.pop(0)


class StageReportsReadsAsReads(unittest.IsolatedAsyncioTestCase):
    async def test_a_read_only_server_exec_does_not_report_a_change(self) -> None:
        events: list[dict[str, Any]] = []
        registry = ToolRegistry(MemoryAuditSink(),
                                __import__("yukinal_agent").AllowlistAuthorization())

        async def read(_arguments: dict[str, Any], _context: ExecutionContext) -> dict[str, Any]:
            return {"state": "completed", "exitCode": 0, "stdout": "SwapTotal: 0 kB\n", "stderr": ""}

        registry.register(ToolSpec(
            name="server.exec", description="Run one approved command.",
            input_schema={"type": "object", "properties": {"command": {"type": "string"}},
                          "additionalProperties": False},
            # Approval is already granted through `approved_actions` in this test;
            # the tool itself keeps `destructive` + `requires_approval`, which is
            # the declaration an operator-facing registry must keep.
            risk_level="destructive", requires_approval=True, handler=read,
            target_id_argument=None,
        ))
        model = _ScriptedModel([
            AIMessage(content="", tool_calls=[{
                "name": "server.exec", "args": {"command": "set -e\ngrep SwapTotal /proc/meminfo"},
                "id": "call-1", "type": "tool_call",
            }]),
            AIMessage(content="No swap is configured."),
        ])
        agent = ModelDrivenAgent(registry, model, AgentBudget(max_tool_calls=2), event_sink=events.append)
        result = await agent.run(
            task_id="task-1", tenant_id="local", user_id="session-1",
            prompt="Is swap configured?", allowed_tools={"server.exec"},
            allowed_target_ids={"srv_1"},
            approved_actions={_exec_fingerprint("set -e\ngrep SwapTotal /proc/meminfo"): "ref-1"},
            host_target={"host": "remote", "serverId": "srv_1", "environment": "development"},
        )
        self.assertEqual(result.status, "succeeded")
        stages = [(event["previous"], event["stage"]) for event in events if event["type"] == "agent.stage"]
        # Before the fix this was [("intake", "change"), ("change", "report")].
        self.assertEqual(stages, [("intake", "evidence"), ("evidence", "report")])


class PendingApprovalSurvivesARestartInTheSameWindow(unittest.IsolatedAsyncioTestCase):
    """The required rule: same window continues, a different window starts fresh.

    Before this change a resent message got a new `runId`, the checkpoint thread id
    was derived from it, so the stored pending approval was never consulted and the
    run silently re-executed. The locator is now a per-turn key that survives a
    restart; the *binding* still verifies scope and authorization.
    """

    async def _pause(self, data_dir: str, run_id: str, executions: list[dict[str, Any]]):
        from yukinal_agent.checkpoints import open_checkpoint_store

        registry = self._registry(executions)
        async with open_checkpoint_store(data_dir) as checkpointer:
            agent = ModelDrivenAgent(registry, self._proposing_model(), checkpointer=checkpointer)
            return await agent.run(**self._kwargs(run_id))

    async def test_a_pending_approval_is_handed_back_and_runs_exactly_once(self) -> None:
        from yukinal_agent.checkpoints import open_checkpoint_store

        executions: list[dict[str, Any]] = []
        with tempfile.TemporaryDirectory(prefix="yukinal-restart-") as data_dir:
            paused = await self._pause(data_dir, "run-first", executions)
            self.assertEqual(paused.status, "awaiting_approval")
            self.assertEqual(executions, [])

            async with open_checkpoint_store(data_dir) as checkpointer:
                resumed_agent = ModelDrivenAgent(
                    self._registry(executions), self._reporting_model(), checkpointer=checkpointer,
                )
                restored = await resumed_agent.run(**self._kwargs("run-after-restart"))
                self.assertEqual(restored.status, "awaiting_approval")
                # Same paused graph: identical thread and identical approval binding.
                self.assertEqual(restored.checkpoint_id, paused.checkpoint_id)
                self.assertEqual(
                    restored.approval_requests[0].fingerprint,
                    paused.approval_requests[0].fingerprint,
                )
                approved = await resumed_agent.resume_after_approval(
                    restored.checkpoint_id or "",
                    restored.approval_requests[0].fingerprint,
                    "approval-ref-restored",
                    approved=True,
                )
            self.assertEqual(approved.status, "succeeded")
            self.assertEqual(executions, [{"value": "x"}], "the change must run exactly once")

    async def test_a_finished_turn_is_not_replayed(self) -> None:
        from yukinal_agent.checkpoints import open_checkpoint_store

        executions: list[dict[str, Any]] = []
        with tempfile.TemporaryDirectory(prefix="yukinal-finished-") as data_dir:
            async with open_checkpoint_store(data_dir) as checkpointer:
                first = ModelDrivenAgent(
                    self._registry(executions),
                    # One round proposes the write, the next answers after it runs.
                    _ScriptedModel([
                        AIMessage(content="", tool_calls=[{
                            "name": "ops.write", "args": {"value": "x"}, "id": "call-1", "type": "tool_call",
                        }]),
                        AIMessage(content="The value was written."),
                    ]),
                    checkpointer=checkpointer,
                )
                paused = await first.run(**self._kwargs("run-a"))
                done = await first.resume_after_approval(
                    paused.checkpoint_id or "", paused.approval_requests[0].fingerprint,
                    "ref", approved=True,
                )
                self.assertEqual(done.status, "succeeded")
            async with open_checkpoint_store(data_dir) as checkpointer:
                # The repeat must be a real new request, so its model proposes the
                # write again instead of replaying the finished answer.
                again = ModelDrivenAgent(
                    self._registry(executions),
                    _ScriptedModel([
                        AIMessage(content="", tool_calls=[{
                            "name": "ops.write", "args": {"value": "x"}, "id": "call-2", "type": "tool_call",
                        }]),
                        AIMessage(content="The value was written."),
                    ]),
                    checkpointer=checkpointer,
                )
                second = await again.run(**self._kwargs("run-b"))
                self.assertEqual(second.status, "awaiting_approval")
                self.assertNotEqual(second.checkpoint_id, paused.checkpoint_id)
            self.assertEqual(executions, [{"value": "x"}])

    async def test_a_different_window_starts_fresh(self) -> None:
        from yukinal_agent.checkpoints import open_checkpoint_store

        executions: list[dict[str, Any]] = []
        with tempfile.TemporaryDirectory(prefix="yukinal-window-") as data_dir:
            paused = await self._pause(data_dir, "run-window-1", executions)
            async with open_checkpoint_store(data_dir) as checkpointer:
                # A different window is a different session, hence a different turn key.
                other = ModelDrivenAgent(
                    self._registry(executions), self._proposing_model(), checkpointer=checkpointer,
                )
                fresh = await other.run(**{**self._kwargs("run-window-2"), "checkpoint_key": "turn:otherwindow"})
            self.assertEqual(fresh.status, "awaiting_approval")
            self.assertNotEqual(fresh.checkpoint_id, paused.checkpoint_id)

    # -- fixtures ---------------------------------------------------------- #
    @staticmethod
    def _proposing_model() -> _ScriptedModel:
        return _ScriptedModel([AIMessage(content="", tool_calls=[{
            "name": "ops.write", "args": {"value": "x"}, "id": "call-1", "type": "tool_call",
        }])])

    @staticmethod
    def _reporting_model() -> _ScriptedModel:
        # The restore itself must not call the model; only the continuation after the
        # approved call does, and it has exactly one scripted answer.
        return _ScriptedModel([AIMessage(content="The value was written.")])

    @staticmethod
    def _registry(executions: list[dict[str, Any]]) -> ToolRegistry:
        async def handler(arguments: dict[str, Any], _context: ExecutionContext) -> str:
            executions.append(arguments)
            return "written"

        built = ToolRegistry(MemoryAuditSink())
        built.register(ToolSpec(
            name="ops.write", description="Write one value.",
            input_schema={"type": "object", "properties": {"value": {"type": "string"}},
                          "additionalProperties": False},
            risk_level="write", requires_approval=True, handler=handler, target_id_argument=None,
        ))
        return built

    @staticmethod
    def _kwargs(run_id: str) -> dict[str, Any]:
        import hashlib

        return {
            "task_id": "task-" + hashlib.sha256(run_id.encode()).hexdigest()[:32],
            "tenant_id": "local", "user_id": "session-1",
            "prompt": "Write x.", "allowed_tools": {"ops.write"},
            "allowed_target_ids": {"srv_1"},
            "host_target": {"host": "remote", "serverId": "srv_1", "environment": "development"},
            "run_id": run_id,
            # What the sidecar now supplies: one stable key per conversation turn.
            "checkpoint_key": "turn:stablekey",
            "checkpoint_binding": "binding-stable",
        }


def _exec_fingerprint(command: str) -> str:
    from yukinal_agent.tools import approval_fingerprint

    return approval_fingerprint(
        "server.exec", "srv_1", {"command": command},
        task_id="task-1", tenant_id="local", user_id="session-1",
    )


def _run_args(run_id: str) -> dict[str, Any]:
    # The desktop derives task_id from run_id when the run has no durable task;
    # reproducing that here is what makes the fingerprints differ per run.
    import hashlib

    task_id = "task-" + hashlib.sha256(run_id.encode()).hexdigest()[:32]
    return {
        "task_id": task_id, "tenant_id": "local", "user_id": "session-1",
        "prompt": "Write x.", "allowed_tools": {"ops.write"},
        "allowed_target_ids": {"srv_1"},
        "host_target": {"host": "remote", "serverId": "srv_1", "environment": "development"},
        "run_id": run_id,
    }


if __name__ == "__main__":
    unittest.main(verbosity=2)


class AutoApprovalConfirmationTurn(unittest.IsolatedAsyncioTestCase):
    """An auto-approved batch may not chain a second change unseen.

    In 自动批准 mode nobody sees the first result before the next call is admitted.
    The turn after a change that actually ran is therefore limited to reads, so a
    model cannot turn "the user stopped watching" into a multi-step mutation.
    """

    async def test_a_change_forces_a_read_only_confirmation_turn(self) -> None:
        executions: list[dict[str, Any]] = []
        events: list[dict[str, Any]] = []

        registry = ToolRegistry(MemoryAuditSink())

        async def write(arguments: dict[str, Any], _context: ExecutionContext) -> str:
            executions.append(arguments)
            return "written"

        async def read(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            executions.append({"read": True})
            return "value is x"

        registry.register_many((
            ToolSpec(
                name="ops.write", description="Write one value.",
                input_schema={"type": "object", "properties": {"value": {"type": "string"}},
                              "additionalProperties": False},
                risk_level="write", requires_approval=True, handler=write, target_id_argument=None,
            ),
            ToolSpec(
                name="ops.read", description="Read the value.",
                input_schema={"type": "object", "properties": {}, "additionalProperties": False},
                risk_level="read", requires_approval=False, handler=read, target_id_argument=None,
            ),
        ))
        model = _ScriptedModel([
            # Round 1: one approved write.
            AIMessage(content="", tool_calls=[{
                "name": "ops.write", "args": {"value": "x"}, "id": "call-w", "type": "tool_call"}]),
            # Round 2: tries a second change in the confirmation turn; it is refused
            # without executing, and the loop asks again.
            AIMessage(content="", tool_calls=[{
                "name": "ops.write", "args": {"value": "y"}, "id": "call-w2", "type": "tool_call"}]),
            # Round 3: complies with a read instead.
            AIMessage(content="", tool_calls=[{"name": "ops.read", "args": {}, "id": "call-r", "type": "tool_call"}]),
            AIMessage(content="The value is x."),
        ])
        agent = ModelDrivenAgent(
            registry, model, AgentBudget(max_tool_calls=6), event_sink=events.append,
            # What the sidecar passes when the run's permissionMode is "auto".
            auto_approval=True,
        )
        first = await agent.run(
            task_id="task-confirm", tenant_id="local", user_id="session-1",
            prompt="Set the value.", allowed_tools={"ops.write", "ops.read"},
            allowed_target_ids={"srv_1"},
            host_target={"host": "remote", "serverId": "srv_1", "environment": "development"},
        )
        self.assertEqual(first.status, "awaiting_approval")

        done = await agent.resume_after_approval(
            first.checkpoint_id or "", first.approval_requests[0].fingerprint, "ref", approved=True,
        )

        print("GATE 5c-auto-approval-confirmation :: " + json.dumps({
            "status": done.status,
            "executions": executions,
            "toolResults": [item.get("error_code") if item.get("status") != "succeeded" else "ok"
                            for item in done.tool_results],
        }, ensure_ascii=False, sort_keys=True, default=str))
        self.assertEqual(done.status, "succeeded")
        # The second write was never dispatched — it does not appear in executions and
        # produced no tool_call event — and the read ran and answered.
        self.assertEqual(executions, [{"value": "x"}, {"read": True}])
        self.assertEqual(
            [event["toolName"] for event in events if event["type"] == "agent.tool_call"],
            ["ops.write", "ops.read"],
        )
        self.assertEqual(done.answer, "The value is x.")
