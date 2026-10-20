from __future__ import annotations

import asyncio
import json
import unittest
from dataclasses import replace

from langchain_core.messages import AIMessage, SystemMessage, ToolMessage

from test_agent_model_loop import ScriptedModel, _registry
from yukinal_agent import AgentBudget, MemoryAuditSink, ModelDrivenAgent, ToolRegistry
from yukinal_agent.goal_loop import long_goal_requested, parse_review
from yukinal_agent.sidecar import _parse_run_request, _sustained_goal
from yukinal_agent.tools import ToolCallError, approval_fingerprint


def operation(index=0):
    return AIMessage(content="", tool_calls=[{"name": "ops__read", "args": {"target_id": "stage-1", "value": str(index)}, "id": f"read-{index}", "type": "tool_call"}])


def review(state="completed", reason="", remaining=None):
    def respond(messages):
        data = json.loads(messages[-1].content)
        return AIMessage(content=json.dumps({"state": state, "remaining": remaining or [],
            "evidenceIds": [item["id"] for item in data["evidence"] if item["status"] == "succeeded"][-100:], "reason": reason}))
    return respond


def progressing_operations(count):
    responses = []
    for index in range(count):
        responses.append(operation(index))
        if (index + 1) % 20 == 0:
            responses.append(review("continue", remaining=["Complete the remaining work and acceptance checks"]))
    return responses


class GoalModel(ScriptedModel):
    async def ainvoke(self, messages):
        self.seen_messages.append(messages)
        item = self.responses.pop(0)
        return item(messages) if callable(item) else item


class SustainedGoalTests(unittest.IsolatedAsyncioTestCase):
    async def run_goal(self, responses, *, budget=None, handler=None, parallel=False, **kwargs):
        async def default_handler(args, _context):
            return {"verified": args.get("value", "ok")}
        model = GoalModel(responses)
        registry = _registry(handler or default_handler)
        if parallel:
            spec = registry.get("ops.read")
            registry = ToolRegistry(MemoryAuditSink())
            registry.register(replace(spec, parallel_safe=True))
        agent = ModelDrivenAgent(registry, model,
            budget or AgentBudget(max_model_rounds=20), goal_loop=True, **kwargs)
        result = await agent.run(task_id="task-goal", tenant_id="tenant-1", user_id="user-1",
            prompt="完整实现部署并测试网站，持续执行直到验收完成", allowed_tools={"ops.read"}, allowed_target_ids={"stage-1"})
        return result, model

    async def test_intermediate_text_returns_to_same_loop_without_human_turn_or_replaying_actions(self):
        events, transcripts = [], []
        result, model = await self.run_goal([
            AIMessage(content="接下来检查并部署。"), review("continue", remaining=["检查真实部署结果"]),
            operation(), AIMessage(content="验收通过。"), review(),
        ], event_sink=events.append, transcript_sink=transcripts.append)
        self.assertEqual((result.status, result.tool_calls), ("succeeded", 1))
        self.assertEqual(len(model.seen_messages), 5)
        self.assertTrue(any("Completion review found" in str(message.content) for message in model.seen_messages[2]))
        self.assertTrue(all(not isinstance(message, SystemMessage) for turn in transcripts for message in turn))
        public = "".join(event.get("textDelta", "") for event in events)
        self.assertIn("接下来：检查真实部署结果", public)
        self.assertNotIn('"evidenceIds"', public)
        self.assertNotIn('"state"', public)
        self.assertEqual(result.answer, "验收通过。")

    async def test_more_than_25_graph_transitions_and_24_tools_finish_in_one_run(self):
        result, model = await self.run_goal([*progressing_operations(30), AIMessage(content="verified"), review()],
            budget=AgentBudget(max_tool_calls=40, max_model_rounds=40))
        self.assertEqual((result.status, result.tool_calls), ("succeeded", 30))
        self.assertEqual(len(model.seen_messages), 33)

    async def test_sustained_run_has_no_24_64_or_100_call_cutoff(self):
        result, _ = await self.run_goal([*progressing_operations(105), AIMessage(content="verified"), review()],
            budget=AgentBudget())
        # The reviewer may cite up to 100 pieces of evidence, independent of
        # the action count. Its response is bounded, execution is not count-cut.
        self.assertEqual(result.tool_calls, 105)
        self.assertEqual((result.status, result.error_code), ("succeeded", None))

    async def test_mid_run_review_redirects_empty_probes_without_counting_them_as_new_progress(self):
        async def handler(*_):
            return {"state": "completed", "exitCode": 0, "stdout": "", "stderr": ""}
        responses = []
        for batch in range(3):
            responses.extend(operation(batch * 20 + i) for i in range(20))
            responses.append(review("continue", remaining=["Stop empty log reads and implement the site"]))
        result, model = await self.run_goal(responses, handler=handler,
            budget=AgentBudget())
        self.assertEqual((result.error_code, result.tool_calls), ("goal_no_progress", 60))
        self.assertEqual(len(model.seen_messages), 63)

    async def test_review_cannot_forge_evidence_or_execute_a_tool(self):
        forged = AIMessage(content=json.dumps({"state": "completed", "remaining": [], "evidenceIds": ["invented"], "reason": ""}))
        for bad in [forged, operation(5), AIMessage(content="not JSON")]:
            with self.subTest(bad=bad.content):
                result, _ = await self.run_goal([operation(), AIMessage(content="done"), bad])
                self.assertEqual(result.error_code, "goal_review_failed")
                self.assertEqual(result.tool_calls, 1)

    async def test_shared_budget_is_not_reset_by_a_continuation(self):
        result, _ = await self.run_goal([operation(), AIMessage(content="next"), review("continue", remaining=["remaining test"]), operation(1)],
            budget=AgentBudget(max_tool_calls=1, max_model_rounds=8))
        self.assertEqual((result.error_code, result.tool_calls), ("tool_call_limit_exceeded", 1))

    async def test_timeout_returns_unknown_outcome_to_goal_without_replaying_handler(self):
        calls = []
        async def handler(args, _context):
            calls.append(args["value"])
            if args["value"] == "0":
                raise ToolCallError("timeout", "timeout")
            return {"verified": True}
        result, model = await self.run_goal([
            operation(0), operation(0), review("continue", remaining=["Inspect actual state with a different read"]),
            operation(1), AIMessage(content="verified"), review(),
        ], handler=handler)
        self.assertEqual((result.status, result.tool_calls), ("succeeded", 3))
        self.assertEqual(calls, ["0", "1"])
        self.assertEqual(result.tool_results[0]["phase"], "outcome_unknown")
        self.assertIn("outcome_unknown", str(model.seen_messages[1][-1].content))

    async def test_authorization_denial_never_becomes_recoverable_timeout(self):
        async def handler(*_):
            raise ToolCallError("permission_denied", "denied")
        result, model = await self.run_goal([operation(), operation(1)], handler=handler)
        self.assertEqual(result.error_code, "permission_denied")
        self.assertEqual(len(model.seen_messages), 1)

    async def test_batch_timeouts_preserve_siblings_and_return_all_outcomes_without_replay(self):
        for parallel in (False, True):
            with self.subTest(parallel=parallel):
                calls = []
                async def handler(args, _context):
                    calls.append(args["value"])
                    if args["value"] in {"0", "2"}:
                        raise ToolCallError("timeout", "timeout")
                    return {"verified": args["value"]}
                batch = AIMessage(content="", tool_calls=[operation(i).tool_calls[0] for i in range(3)])
                result, model = await self.run_goal([
                    batch, operation(3), AIMessage(content="verified"), review(),
                ], handler=handler, parallel=parallel)
                self.assertEqual((result.status, result.tool_calls), ("succeeded", 4))
                self.assertCountEqual(calls, ["0", "1", "2", "3"])
                outcomes = [m for m in model.seen_messages[1] if isinstance(m, ToolMessage)]
                self.assertEqual([m.tool_call_id for m in outcomes], ["read-0", "read-1", "read-2"])
                self.assertEqual([json.loads(m.content).get("state") for m in outcomes],
                                 ["outcome_unknown", None, "outcome_unknown"])
                self.assertEqual([r["status"] for r in result.tool_results],
                                 ["failed", "succeeded", "failed", "succeeded"])

    async def test_timeout_in_write_batch_never_runs_a_dependent_mutation(self):
        calls = []
        async def handler(args, _context):
            calls.append(args["value"])
            raise ToolCallError("timeout", "timeout")
        batch = AIMessage(content="", tool_calls=[
            {**operation(i).tool_calls[0], "name": "ops__write"} for i in range(2)])
        model = GoalModel([batch])
        registry = _registry(handler, write=True)
        agent = ModelDrivenAgent(registry, model, goal_loop=True)
        result = await agent.run(task_id="write-goal", tenant_id="tenant-1", user_id="user-1",
            prompt="持续执行部署验收", allowed_tools={"ops.write"},
            allowed_target_ids={"stage-1"}, approved_actions={
                approval_fingerprint("ops.write", "stage-1", call["args"], task_id="write-goal",
                    tenant_id="tenant-1", user_id="user-1"): "fixture-approval" for call in batch.tool_calls})
        self.assertEqual((result.status, result.error_code), ("failed", "timeout"))
        self.assertEqual(calls, ["0"])

    async def test_repeated_diagnostics_return_to_unfinished_goal_without_executing_repeat(self):
        calls = []
        async def handler(args, _context):
            calls.append(args["value"])
            return {"verified": args["value"]}
        result, _ = await self.run_goal([
            operation(0), operation(0), operation(0),
            review("continue", remaining=["Implement and verify the independent new site"]),
            operation(1), AIMessage(content="verified"), review(),
        ], handler=handler)
        self.assertEqual((result.status, result.tool_calls), ("succeeded", 4))
        self.assertEqual(calls, ["0", "0", "1"])

    async def test_schema_repairs_are_not_a_lifetime_quota_and_never_execute_bad_input(self):
        def invalid(index):
            call = operation(index)
            call.tool_calls[0]["args"]["unregistered"] = True
            return call
        executed = []
        async def handler(args, _context):
            executed.append(args["value"])
            return {"verified": True}
        result, _ = await self.run_goal([
            invalid(0), operation(0), invalid(1), operation(1),
            invalid(2), invalid(3), invalid(4),
            review("continue", remaining=["Repair schema and verify deployment"]),
            operation(2), AIMessage(content="verified"), review(),
        ], handler=handler)
        self.assertEqual((result.status, result.tool_calls), ("succeeded", 8))
        self.assertEqual(executed, ["0", "1", "2"])

    async def test_plain_answer_after_invalid_input_still_requires_goal_review(self):
        invalid = operation(0)
        invalid.tool_calls[0]["args"]["unregistered"] = True
        result, _ = await self.run_goal([
            invalid, AIMessage(content="done"),
            review("continue", remaining=["No action ran; implement and verify"]),
            operation(1), AIMessage(content="verified"), review(),
        ])
        self.assertEqual((result.status, result.tool_calls), ("succeeded", 2))

    async def test_no_progress_stops_and_real_blocker_is_not_success(self):
        responses = []
        for _ in range(3):
            responses.extend([AIMessage(content="next"), review("continue", remaining=["implement site"])])
        result, _ = await self.run_goal(responses)
        self.assertEqual(result.error_code, "goal_no_progress")
        events = []
        blocked, _ = await self.run_goal([AIMessage(content="需要提供目标地址"), review("blocked", reason="缺少目标地址")], event_sink=events.append)
        self.assertEqual((blocked.status, blocked.error_code), ("failed", "goal_blocked"))
        self.assertTrue(any("缺少目标地址" in event.get("textDelta", "") for event in events))

    async def test_cancellation_during_review_does_not_start_more_tools(self):
        started = asyncio.Event()
        model = GoalModel([AIMessage(content="done")])
        original = model.ainvoke
        async def invoke(messages):
            if str(messages[0].content).startswith("You are the completion gate"):
                started.set()
                await asyncio.Event().wait()
            return await original(messages)
        model.ainvoke = invoke
        async def handler(*_):
            self.fail("review executed a tool")
        agent = ModelDrivenAgent(_registry(handler), model, goal_loop=True)
        task = asyncio.create_task(agent.run(task_id="cancel-goal", tenant_id="tenant-1", user_id="user-1", prompt="long goal", allowed_tools={"ops.read"}, allowed_target_ids={"stage-1"}))
        await asyncio.wait_for(started.wait(), 2)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task

    def test_scope_admission_and_explicit_budgets(self):
        for prompt in ["检查服务器", "什么是loop-agent？", "请解释长任务", "```text\n持续执行直到完成\n```"]:
            self.assertFalse(long_goal_requested(prompt))
        request = {"runId": "long-run", "sessionId": "long-session", "prompt": "长任务：完整实现、部署、测试并修复网站"}
        self.assertTrue(_sustained_goal(request))
        for extra in [{"mode": "readonly"}, {"mode": "plan"}, {"toolAccess": "none"}, {"taskId": "durable"}, {"planBinding": {"planId": "p"}}]:
            self.assertFalse(_sustained_goal(request | extra))
        budget = _parse_run_request(request)["agentBudget"]
        self.assertEqual((budget.max_tool_calls, budget.max_model_rounds, budget.max_total_seconds), (None, None, 1800))
        ordinary = _parse_run_request(request | {"prompt": "检查并修复这个服务"})["agentBudget"]
        self.assertEqual((ordinary.max_tool_calls, ordinary.max_model_rounds), (None, None))
        self.assertFalse(long_goal_requested("不要持续执行，只检查部署配置"))
        explicit = _parse_run_request(request | {"taskBudget": {"maxSteps": 2, "maxRunMs": 3000, "maxAttempts": 1}})["agentBudget"]
        self.assertEqual((explicit.max_tool_calls, explicit.max_model_rounds, explicit.max_total_seconds), (2, 2, 3))

    def test_nonzero_command_cannot_be_completion_evidence(self):
        payload = json.dumps({"state": "completed", "remaining": [], "evidenceIds": ["bad"], "reason": ""})
        self.assertIsNone(parse_review(payload, [{"call_id": "bad", "status": "succeeded", "result": {"state": "completed", "exitCode": 2}}]))
