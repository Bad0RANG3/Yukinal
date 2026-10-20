from __future__ import annotations

import json
import unittest

from langchain_core.messages import AIMessage

from test_agent_model_loop import ScriptedModel
from yukinal_agent.tools import approval_fingerprint
from yukinal_agent.shell_policy import prepare_model_tool_arguments
from yukinal_agent import AgentBudget, ExecutionContext, MemoryAuditSink, ModelDrivenAgent, ToolRegistry, ToolSpec


def call(index: int, command: str = "read config") -> AIMessage:
    return AIMessage(content="", tool_calls=[{
        "name": "server__exec", "id": f"call-{index}", "type": "tool_call",
        "args": {"command": command, "purpose": f"read {index}"},
    }])


class RepetitionGuardTests(unittest.IsolatedAsyncioTestCase):
    async def run_model(self, responses: list[AIMessage], outputs: list[str], *, requires_approval: bool = False):
        executed = []

        async def handler(arguments: dict, _context: ExecutionContext) -> dict:
            executed.append(arguments["command"])
            return {"stdout": outputs.pop(0), "exitCode": 0, "durationMs": len(executed)}

        registry = ToolRegistry(MemoryAuditSink())
        registry.register(ToolSpec(
            name="server.exec", description="Run a scoped operation.",
            input_schema={"type": "object", "properties": {
                "command": {"type": "string"}, "purpose": {"type": "string"}},
                "required": ["command", "purpose"], "additionalProperties": False},
            risk_level="destructive", requires_approval=True,
            handler=handler, target_id_argument=None,
        ))
        approvals = {} if requires_approval else {
            approval_fingerprint("server.exec", None, prepare_model_tool_arguments("server.exec", call["args"]),
                task_id="repeat-test", tenant_id="tenant", user_id="user"): "fixture-approval"
            for response in responses for call in response.tool_calls
        }
        model = ScriptedModel(responses)
        agent = ModelDrivenAgent(registry, model, AgentBudget(max_model_rounds=8))
        result = await agent.run(task_id="repeat-test", tenant_id="tenant", user_id="user",
                                 prompt="Complete the task.", allowed_tools={"server.exec"},
                                 allowed_target_ids=set(), approved_actions=approvals)
        return result, executed, model

    async def test_unchanged_operation_is_not_executed_a_third_time_and_can_recover(self):
        result, executed, model = await self.run_model(
            [call(1), call(2), call(3), call(4, "apply fix"), AIMessage(content="Fixed and verified.")],
            ["same file", "same file", "updated"],
        )
        self.assertEqual(result.status, "succeeded")
        self.assertEqual(executed, ["set -e\nread config", "set -e\nread config", "set -e\napply fix"])
        notice = json.loads(model.seen_messages[3][-1].content)
        self.assertEqual(notice["state"], "not_executed")
        self.assertEqual(notice["errorCode"], "tool_repetition_detected")
        self.assertEqual(result.tool_results[2]["status"], "failed")

    async def test_rewording_purpose_does_not_extend_the_loop(self):
        result, executed, model = await self.run_model(
            [call(1), call(2), call(3), call(4), AIMessage(content="Must not reach this.")],
            ["same", "same"],
        )
        self.assertEqual(result.error_code, "tool_repetition_detected")
        self.assertEqual(len(executed), 2)
        self.assertEqual(len(model.responses), 1)

    async def test_changing_results_or_intervening_operations_allow_fresh_checks(self):
        result, executed, _model = await self.run_model(
            [call(1), call(2), call(3), call(4, "write config"), call(5), AIMessage(content="Done.")],
            ["starting", "ready", "ready", "changed", "ready"],
        )
        self.assertEqual(result.status, "succeeded")
        self.assertEqual(len(executed), 5)

    async def test_reporting_a_blocker_after_guard_does_not_claim_success(self):
        result, executed, _model = await self.run_model(
            [call(1), call(2), call(3), AIMessage(content="Blocked; the requested change is unfinished.")],
            ["same", "same"],
        )
        self.assertEqual(result.status, "failed")
        self.assertEqual(result.error_code, "tool_repetition_detected")
        self.assertEqual(len(executed), 2)

    async def test_approval_is_still_required_before_any_execution(self):
        result, executed, _model = await self.run_model([call(1)], [], requires_approval=True)
        self.assertEqual(result.status, "awaiting_approval")
        self.assertEqual(executed, [])


if __name__ == "__main__":
    unittest.main()
