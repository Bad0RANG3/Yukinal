"""Behavioral acceptance for inference, execution and context boundaries. No API calls."""
from __future__ import annotations

import asyncio
import json
import unittest

from langchain_core.messages import AIMessage, HumanMessage, ToolMessage

from yukinal_agent import AgentBudget, MemoryAuditSink, ModelDrivenAgent, ToolRegistry, ToolSpec
from yukinal_agent.conversation_store import ConversationStore, message_bytes
from yukinal_agent.inference_policy import select_inference
from yukinal_agent.model_context import model_messages
from yukinal_agent.model_metrics import accumulate_usage
from yukinal_agent.models import ModelConfig, ModelFactory
from yukinal_agent.tools import ToolCallError, approval_fingerprint
from test_agent_model_loop import ScriptedModel


def call(name, index):
    return {"name": name.replace(".", "__"), "args": {"value": str(index)}, "id": str(index), "type": "tool_call"}


async def run(agent, **kwargs):
    return await agent.run(task_id="task-test", tenant_id="local", user_id="user-test", prompt="检查并给出结果",
                           allowed_tools={"ops.read", "ops.barrier", "ops.write"}, allowed_target_ids=set(), **kwargs)


def register(registry, name, handler, parallel=False, write=False):
    registry.register(ToolSpec(name, "Read independently" if parallel else "Ordered operation",
        {"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"], "additionalProperties": False},
        "write" if write else "read", write, handler, target_id_argument=None, parallel_safe=parallel))


class PerformanceTests(unittest.IsolatedAsyncioTestCase):
    async def test_cutoff_empty_and_blocked_responses_never_succeed_or_execute_calls(self):
        executed = []
        async def handler(args, context):
            executed.append(args); return "unexpected"
        registry = ToolRegistry(MemoryAuditSink()); register(registry, "ops.read", handler, True)
        for reason, content, expected in [("length", "", "model_output_truncated"),
            ("max_tokens", "partial", "model_output_truncated"), ("content_filter", "", "model_response_blocked"),
            ("stop", "   ", "empty_model_response")]:
            with self.subTest(reason=reason):
                response = AIMessage(content=content, response_metadata={"finish_reason": reason},
                    tool_calls=[call("ops.read", 1)] if reason == "length" else [])
                result = await run(ModelDrivenAgent(registry, ScriptedModel([response])))
                self.assertEqual((result.status, result.error_code), ("failed", expected))
        self.assertEqual(executed, [])

    async def test_independent_reads_overlap_and_results_keep_provider_order(self):
        active = peak = 0; started = asyncio.Event()
        async def handler(args, context):
            nonlocal active, peak
            active += 1; peak = max(peak, active)
            if active == 3: started.set()
            try:
                await asyncio.wait_for(started.wait(), 1)
                await asyncio.sleep((3 - int(args["value"])) * .003)
                return args["value"]
            finally: active -= 1
        registry = ToolRegistry(MemoryAuditSink()); register(registry, "ops.read", handler, True)
        model = ScriptedModel([AIMessage(content="", tool_calls=[call("ops.read", i) for i in range(3)]), AIMessage(content="已查证。")])
        result = await run(ModelDrivenAgent(registry, model))
        self.assertEqual(result.status, "succeeded"); self.assertEqual(peak, 3); self.assertEqual(result.tool_calls, 3)
        self.assertEqual([m.tool_call_id for m in model.seen_messages[-1] if isinstance(m, ToolMessage)], ["0", "1", "2"])
        self.assertEqual([item["result"] for item in result.tool_results], ["0", "1", "2"])

    async def test_a_failed_read_in_a_batch_returns_to_the_model_instead_of_ending_the_run(self):
        # Regression: the diagnosis branch used to require a single call, so one
        # failed read in a model-proposed batch ended the whole run with the model
        # never asked again. A single task must survive one bad observation.
        executed = []
        async def reads(args, context):
            executed.append(args["value"])
            return {"observed": args["value"]}
        async def fails(args, context):
            executed.append("fail-" + args["value"])
            raise ToolCallError("execution_failed", "the remote probe returned an unexpected shape")
        registry = ToolRegistry(MemoryAuditSink())
        register(registry, "ops.read", reads, True)
        register(registry, "ops.fail", fails, True)
        model = ScriptedModel([
            AIMessage(content="", tool_calls=[call("ops.fail", 0), call("ops.read", 1)]),
            AIMessage(content="The probe failed; the host is otherwise reachable."),
        ])
        result = await ModelDrivenAgent(registry, model, AgentBudget(max_model_rounds=4)).run(
            task_id="task-test", tenant_id="local", user_id="user-test", prompt="检查并给出结果",
            allowed_tools={"ops.read", "ops.fail"}, allowed_target_ids=set())
        self.assertEqual(result.status, "succeeded", result)
        self.assertEqual(result.answer, "The probe failed; the host is otherwise reachable.")
        self.assertEqual(result.tool_calls, 2)
        self.assertEqual([item["status"] for item in result.tool_results], ["failed", "succeeded"])
        self.assertEqual(result.tool_results[0]["phase"], "failure_diagnosis")
        # Both tool calls answered: the batch did not drop a sibling message.
        answered = [message for message in model.seen_messages[1] if isinstance(message, ToolMessage)]
        self.assertEqual([message.tool_call_id for message in answered], ["0", "1"])
        self.assertEqual(executed, ["fail-0", "1"])

    async def test_a_failed_batch_read_requires_verification_before_a_later_effectful_call(self):
        # The diagnosis is only a diagnosis once a read actually succeeded. A
        # failed read in the batch must keep the recovery guard armed so the model
        # cannot follow it with an effectful call in the same run.
        writes = []
        async def writes_handler(args, context):
            writes.append(args["value"]); return "changed"
        async def fails(args, context):
            raise ToolCallError("execution_failed", "read failed")
        async def reads(args, context):
            return {"observed": args["value"]}
        registry = ToolRegistry(MemoryAuditSink())
        register(registry, "ops.read", reads, True)
        register(registry, "ops.fail", fails, True)
        register(registry, "ops.write", writes_handler, write=True)
        model = ScriptedModel([
            AIMessage(content="", tool_calls=[call("ops.fail", 0), call("ops.fail", 1)]),
            AIMessage(content="", tool_calls=[call("ops.write", 2)]),
            AIMessage(content="", tool_calls=[call("ops.read", 3)]),
            AIMessage(content="Inspected the state without repeating the change."),
        ])
        fingerprint = approval_fingerprint("ops.write", None, {"value": "2"},
                                           task_id="task-test", tenant_id="local", user_id="user-test")
        result = await ModelDrivenAgent(registry, model, AgentBudget(max_model_rounds=6)).run(
            task_id="task-test", tenant_id="local", user_id="user-test", prompt="检查并给出结果",
            allowed_tools={"ops.read", "ops.fail", "ops.write"}, allowed_target_ids=set(),
            approved_actions={fingerprint: "fixture-approval"})
        self.assertEqual(result.status, "succeeded", result)
        self.assertEqual(writes, [], "an unverified effectful call must not execute")
        self.assertEqual(result.tool_results[2]["error_code"], "read_only_verification_required")

    async def test_writes_and_unmarked_reads_are_ordering_barriers(self):
        events = []
        async def handler(args, context):
            events.append("start-" + args["value"]); await asyncio.sleep(.005)
            events.append("end-" + args["value"]); return args["value"]
        registry = ToolRegistry(MemoryAuditSink())
        register(registry, "ops.read", handler, True); register(registry, "ops.barrier", handler)
        register(registry, "ops.write", handler, write=True)
        calls = [call("ops.read", 0), call("ops.read", 1), call("ops.write", 2), call("ops.barrier", 3), call("ops.read", 4)]
        result = await run(ModelDrivenAgent(registry, ScriptedModel([AIMessage(content="", tool_calls=calls), AIMessage(content="已完成。")])),
            approved_actions={approval_fingerprint("ops.write", None, {"value": "2"}, task_id="task-test", tenant_id="local", user_id="user-test"): "fixture-approval"})
        self.assertEqual(result.status, "succeeded")
        self.assertLess(events.index("end-0"), events.index("start-2")); self.assertLess(events.index("end-1"), events.index("start-2"))
        self.assertLess(events.index("end-2"), events.index("start-3")); self.assertLess(events.index("end-3"), events.index("start-4"))

    async def test_cancelling_a_batch_cleans_up_every_read(self):
        active = 0; started = asyncio.Event(); cancelled = []
        async def handler(args, context):
            nonlocal active
            active += 1
            if active == 3: started.set()
            try: await asyncio.Event().wait()
            finally: active -= 1; cancelled.append(args["value"])
        registry = ToolRegistry(MemoryAuditSink()); register(registry, "ops.read", handler, True)
        agent = ModelDrivenAgent(registry, ScriptedModel([AIMessage(content="", tool_calls=[call("ops.read", i) for i in range(3)])]))
        task = asyncio.create_task(run(agent)); await asyncio.wait_for(started.wait(), 1); task.cancel()
        with self.assertRaises(asyncio.CancelledError): await task
        self.assertEqual(active, 0); self.assertEqual(set(cancelled), {"0", "1", "2"})

    async def test_custom_read_approvals_resume_without_replaying_completed_siblings(self):
        executed = []
        class CustomPolicy:
            def authorize(self, context, spec, arguments):
                fingerprint = approval_fingerprint(spec.name, None, arguments, task_id=context.task_id,
                    tenant_id=context.tenant_id, user_id=context.user_id)
                if arguments["value"] != "0" and not context.approved_actions.get(fingerprint):
                    raise ToolCallError("approval_required", "Custom read approval", approval_fingerprint=fingerprint)
                return "approved" if arguments["value"] != "0" else "not_required"
        async def handler(args, context): executed.append(args["value"]); return args["value"]
        registry = ToolRegistry(MemoryAuditSink(), CustomPolicy()); register(registry, "ops.read", handler, True)
        model = ScriptedModel([AIMessage(content="", tool_calls=[call("ops.read", i) for i in range(3)]), AIMessage(content="已完成。")])
        agent = ModelDrivenAgent(registry, model)
        paused = await run(agent)
        self.assertEqual(paused.status, "awaiting_approval"); self.assertEqual(executed, ["0"])
        self.assertEqual(len(paused.approval_requests), 1)
        for value in ("1", "2"):
            self.assertEqual(paused.approval_requests[0].arguments["value"], value)
            paused = await agent.resume_after_approval(paused.checkpoint_id, paused.approval_requests[0].fingerprint,
                "approved-" + value, approved=True)
        self.assertEqual(paused.status, "succeeded")
        self.assertEqual(executed, ["0", "1", "2"]); self.assertEqual(paused.tool_calls, 3)
        self.assertEqual({m.tool_call_id for m in model.seen_messages[-1] if isinstance(m, ToolMessage)}, {"0", "1", "2"})

    async def test_batch_output_limit_is_shared(self):
        async def handler(args, context): return "x" * 60
        registry = ToolRegistry(MemoryAuditSink()); register(registry, "ops.read", handler, True)
        agent = ModelDrivenAgent(registry, ScriptedModel([AIMessage(content="", tool_calls=[call("ops.read", i) for i in range(3)])]), AgentBudget(max_output_bytes=100))
        result = await run(agent)
        self.assertEqual(result.status, "failed"); self.assertLessEqual(result.output_bytes, 100)
        self.assertEqual(result.tool_calls, 3)

    def test_large_result_projection_keeps_original_and_private_reasoning(self):
        thought = AIMessage(content="", additional_kwargs={"reasoning_content": "private-state"}, tool_calls=[call("ops.read", 0)])
        for payload in ("开头" + "x" * 100000 + "结尾", "\0" * 100000):
            original = ToolMessage(content=payload, tool_call_id="0")
            projected = model_messages([thought, original])
            self.assertIs(projected[0], thought); self.assertEqual(original.content, payload)
            self.assertLessEqual(len(projected[1].content.encode()), 16 * 1024)
            self.assertTrue(json.loads(projected[1].content)["truncated"])
            self.assertEqual(projected[1].tool_call_id, "0")

    def test_auto_effort_respects_manual_model_and_continuation(self):
        model = ModelFactory().create_with_api_key(ModelConfig("openai-compatible", "deepseek-flash", base_url="https://api.deepseek.com"), "test-key")
        for prompt, effort in [("你好", "none"), ("继续", "low"), ("好的", "low"), ("检查服务状态", "low"), ("深入分析根因", "high")]:
            selected = select_inference(model, {"promptText": prompt})
            self.assertEqual(selected.effort, effort); self.assertEqual(selected.source, "auto")
        manual = select_inference(model, {"promptText": "你好", "reasoningEffort": "max"})
        self.assertEqual((manual.effort, manual.output_tokens, manual.source), ("max", 32768, "manual"))
        self.assertIsNone(model.reasoning_effort)
        fake = ScriptedModel([])
        self.assertIs(select_inference(fake, {"promptText": "深入"}).model, fake)

    def test_usage_does_not_estimate_missing_counts_or_expose_private_text(self):
        response = AIMessage(content="结果", additional_kwargs={"reasoning_content": "private-state"},
            usage_metadata={"input_tokens": 100, "output_tokens": 50, "total_tokens": 150, "output_token_details": {"reasoning": 40}},
            response_metadata={"deepseek_usage": {"prompt_cache_hit_tokens": 80, "prompt_cache_miss_tokens": 20}})
        usage = accumulate_usage({}, response)
        self.assertEqual((usage["reasoningTokens"], usage["cacheReadTokens"], usage["cacheMissTokens"]), (40, 80, 20))
        partial = accumulate_usage(usage, AIMessage(content="未上报"))
        self.assertEqual(partial["unreportedRequests"], 1); self.assertEqual(partial["reasoningReportedRequests"], 1)
        self.assertNotIn("private-state", json.dumps(partial))

    def test_deepseek_nonstream_cache_counts_preserve_zero_and_drop_invalid_values(self):
        model = ModelFactory().create_with_api_key(ModelConfig("openai-compatible", "deepseek-flash", base_url="https://api.deepseek.com"), "test-key")
        for raw, expected in [({"prompt_cache_hit_tokens": 0, "prompt_cache_miss_tokens": 12}, {"prompt_cache_hit_tokens": 0, "prompt_cache_miss_tokens": 12}),
                              ({"prompt_cache_hit_tokens": True, "prompt_cache_miss_tokens": -1}, {})]:
            result = model._create_chat_result({"id":"fixture", "model":"deepseek-flash", "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"结果","reasoning_content":"private-state"}}],
                "usage":{"prompt_tokens":12,"completion_tokens":7,"total_tokens":19,**raw}})
            message = result.generations[0].message
            self.assertEqual(message.response_metadata.get("deepseek_usage", {}), expected)
            usage = accumulate_usage({}, message)
            if expected: self.assertEqual((usage["cacheReadTokens"],usage["cacheReportedRequests"]),(0,1))
            else: self.assertNotIn("cacheReadTokens", usage)
            self.assertNotIn("private-state",json.dumps(usage))

    def test_context_pressure_compacts_whole_turns_and_keeps_late_correction(self):
        store = ConversationStore()
        try:
            for i in range(8):
                prompt = "当前目标：修复服务。" + "过程资料 " * 300
                if i == 7: prompt += "\n更正：保留数据库，不要重装。"
                request = {"runId": f"run-{i}", "sessionId": "session-test", "promptText": prompt}
                store.begin(request, str(i))
                store.update_messages(request["runId"], [HumanMessage(content=prompt), AIMessage(content="已检查；下一步验证数据库连接。")])
                store.finish(request["runId"], "succeeded")
            context = store.context("session-test", None, max_bytes=24 * 1024, query_text="保留数据库")
            self.assertGreater(context.usage["compactedTurns"], 0)
            self.assertLessEqual(message_bytes(context.messages), 24 * 1024)
            self.assertIn("保留数据库", "\n".join(str(m.content) for m in context.messages))
            self.assertEqual(context.usage["totalTurns"], 8)
            self.assertEqual(store._connection.execute("SELECT count(*) FROM conversation_turns").fetchone()[0], 8)
        finally: store.close()
