"""A store fault after a tool already changed the target is not a plain failure.

The durable checkpoint is the only record that an effectful call happened. When the
saver cannot persist the step that follows such a call, the run must not report a
clean `failed`/`task_timeout`: the operator (and the model) would resend the request
and repeat an effect the target has already taken. R4 recorded this as H-7.
"""

from __future__ import annotations

import sqlite3
import unittest
from typing import Any

from langchain_core.messages import AIMessage
from langgraph.checkpoint.memory import InMemorySaver

from yukinal_agent import AgentBudget, ExecutionContext, ModelDrivenAgent
from yukinal_agent.model_agent import _CHECKPOINT_WRITE_CODE, _OUTCOME_UNKNOWN_CODE
from yukinal_agent.tools import approval_fingerprint

from test_agent_model_loop import ScriptedModel, _registry


class _FailingSaver(InMemorySaver):
    """A real saver that stops persisting once `armed` is set.

    The tool handler arms it, so the very write that would have recorded the tool's
    result is the one that fails — the exact ordering the defect is about. It has to
    subclass a real saver: `graph.compile()` rejects anything that is not a
    `BaseCheckpointSaver`, and the failure has to come from the storage layer rather
    than from a stand-in the graph would refuse to accept.
    """

    armed = False
    persisted = 0

    async def aput(self, config: Any, checkpoint: Any, metadata: Any, new_versions: Any) -> Any:
        if self.armed:
            raise sqlite3.OperationalError("unable to open database file")
        type(self).persisted += 1
        return await super().aput(config, checkpoint, metadata, new_versions)


def _write_model() -> ScriptedModel:
    return ScriptedModel([
        AIMessage(content="", tool_calls=[{
            "name": "ops.write",
            "args": {"target_id": "srv_stage1", "value": "changed"},
            "id": "write-1",
            "type": "tool_call",
        }]),
        AIMessage(content="The change was applied."),
    ])


class CheckpointWriteFailureTests(unittest.IsolatedAsyncioTestCase):
    async def test_a_committed_effect_is_reported_as_an_unknown_outcome(self) -> None:
        executed: list[str] = []
        saver = _FailingSaver()

        async def handler(arguments: dict[str, Any], _context: ExecutionContext) -> str:
            executed.append(arguments["value"])
            saver.armed = True
            return "changed"

        registry = _registry(handler, write=True)
        fingerprint = approval_fingerprint(
            "ops.write", "srv_stage1", {"target_id": "srv_stage1", "value": "changed"},
            task_id="task-ckpt-write", tenant_id="tenant-1", user_id="user-1",
        )
        with self.assertLogs("yukinal_agent.model_agent", level="WARNING") as logs:
            result = await ModelDrivenAgent(registry, _write_model(), checkpointer=saver).run(
                task_id="task-ckpt-write", tenant_id="tenant-1", user_id="user-1",
                prompt="Apply the change.", allowed_tools={"ops.write"},
                allowed_target_ids={"srv_stage1"},
                approved_actions={fingerprint: "approval-1"},
            )

        self.assertEqual(executed, ["changed"])
        # The run state stays inside what the host and the shared schema accept; the
        # unknownness has to travel in the code, not in a new state value.
        self.assertEqual(result.status, "failed")
        self.assertEqual(result.error_code, _OUTCOME_UNKNOWN_CODE)
        self.assertTrue(
            any("agent run aborted" in line and _OUTCOME_UNKNOWN_CODE in line for line in logs.output),
            logs.output,
        )

    async def test_a_fault_before_any_effect_keeps_the_plain_storage_code(self) -> None:
        # With no effect on the target there is nothing that could have escaped, so the
        # abort keeps its specific storage code and stays a plain failure.
        agent = ModelDrivenAgent(_registry(lambda a, c: None), ScriptedModel([]))
        with self.assertLogs("yukinal_agent.model_agent", level="WARNING"):
            result = agent._aborted_result("task-ckpt-no-effect", _CHECKPOINT_WRITE_CODE, reason="fixture")
        self.assertEqual(result.status, "failed")
        self.assertEqual(result.error_code, _CHECKPOINT_WRITE_CODE)

    async def test_a_read_only_run_keeps_the_generic_workflow_code(self) -> None:
        # A read changed nothing on the target, so a storage fault is still just a
        # storage fault: no effect can have escaped and the code stays generic.
        class _BrokenSaver(_FailingSaver):
            async def aput(self, config: Any, checkpoint: Any, metadata: Any, new_versions: Any) -> Any:
                raise RuntimeError("checkpoint store exploded")

        with self.assertLogs("yukinal_agent.model_agent", level="WARNING"):
            result = await ModelDrivenAgent(
                _registry(lambda a, c: None), ScriptedModel([]), checkpointer=_BrokenSaver(),
            ).run(
                task_id="task-ckpt-read", tenant_id="tenant-1", user_id="user-1",
                prompt="Read the state.", allowed_tools={"ops.read"},
                allowed_target_ids={"srv_stage1"},
            )

        self.assertEqual(result.status, "failed")
        self.assertEqual(result.error_code, "agent_workflow_failed")

    async def test_an_abort_never_leaves_the_shared_run_state(self) -> None:
        agent = ModelDrivenAgent(_registry(lambda a, c: None), ScriptedModel([]))
        agent._tool_effects_committed = 1
        result = agent._aborted_result("task-ckpt-status", "task_timeout", status="timed_out", reason="fixture")
        self.assertEqual(result.status, "timed_out")
        self.assertEqual(result.error_code, _OUTCOME_UNKNOWN_CODE)
        self.assertIn(result.status, {"succeeded", "failed", "timed_out", "cancelled"})

    async def test_the_timeout_budget_still_bounds_the_run(self) -> None:
        async def handler(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            return "ok"

        model = ScriptedModel([AIMessage(content="", tool_calls=[{
            "name": "ops.read", "args": {"target_id": "srv_stage1"}, "id": "slow", "type": "tool_call",
        }])])
        result = await ModelDrivenAgent(
            _registry(handler), model, budget=AgentBudget(max_total_seconds=0.01),
        ).run(
            task_id="task-ckpt-budget", tenant_id="tenant-1", user_id="user-1",
            prompt="Read the state.", allowed_tools={"ops.read"},
            allowed_target_ids={"srv_stage1"},
        )
        self.assertEqual(result.status, "timed_out")
        self.assertEqual(result.error_code, "task_timeout")


if __name__ == "__main__":
    unittest.main()
