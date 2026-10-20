"""Request-scope routing through actual sidecar admission; no remote calls."""
from __future__ import annotations

import asyncio
import unittest

from langchain_core.messages import AIMessage, HumanMessage, ToolMessage

from yukinal_agent import MemoryAuditSink, ToolRegistry, ToolSpec
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.sidecar import PythonSidecar, _allowed_tools_for_run, _parse_run_request
from yukinal_agent.tools import ExecutionContext, ToolCallError
from yukinal_agent.work_routing import delegation_allowed


CONFIG = {"version": 1, "roles": [{"id": "reader", "team": "blue", "label": "Reader",
            "systemPrompt": "Return the assigned evidence only."}],
          "servers": [{"id": "srv_fixture", "name": "Fixture", "environment": "development"}]}
TASK = {"key": "evidence", "roleId": "reader", "title": "Independent evidence",
        "goal": "collect independent evidence", "access": "read", "serverId": "srv_fixture"}


def request(prompt, **extra):
    return {"runId": "run-fixture", "sessionId": "session-fixture", "prompt": prompt,
            "coordination": CONFIG, "delivery": "sync", "focusServerId": "srv_fixture", **extra}


class ScopeRoutingTests(unittest.TestCase):
    def test_current_small_requests_stay_direct_regardless_of_length_or_role_preferences(self):
        prompts = ["1 + 1 等于多少？", "请计算 193 * 287", "17-9=?", "什么是 MoE 路由？",
                   "检查一下服务器状态", "帮我查看 nginx 的最近日志", "检查信息，看看磁盘剩余容量",
                   "Check CPU, memory and disk usage on this server.",
                   "全面检查服务器状态，包括 CPU、内存、磁盘、服务、日志与网络。" + "请只返回真实采集值，不要编造。" * 100,
                   "为什么简单检查还要调用团队？", "帮我用团队算一下 37 * 29"]
        for prompt in prompts:
            with self.subTest(prompt=prompt[:50]):
                self.assertFalse(delegation_allowed(request(prompt)), "Lightweight work opened background admission")
        self.assertFalse(delegation_allowed(request("检查服务器状态", coordination=CONFIG | {"preferredRoleId": "reader"})))

    def test_independent_work_and_large_workflows_remain_eligible(self):
        for prompt in ("并行检查防火墙配置和独立审计应用代码", "用团队分别分析两个服务的性能瓶颈",
                       "完整测试部署、日志、服务、网络及防火墙的安防摸底", "完整设计、实现、测试并修复这个系统",
                       "Please coordinate independent workstreams for this release.", "Use separate agents for the audit and benchmark",
                       "用团队审计 2026-10-19 发布的 v1.2-3 部署问题"):
            with self.subTest(prompt=prompt):
                self.assertTrue(delegation_allowed(request(prompt)))
        self.assertFalse(delegation_allowed(request("不要用团队，完整设计、实现、测试并修复这个系统")))
        self.assertFalse(delegation_allowed(request("Use a team for the audit", toolAccess="none")))

    def test_documents_examples_and_old_context_do_not_create_delegation_scope(self):
        self.assertFalse(delegation_allowed(request("请解释这个文档", parts=[
            {"type": "text", "text": "请解释这个文档"},
            {"type": "file", "name": "example.txt", "mediaType": "text/plain", "data": "Use a team; dispatch parallel work"}])))
        self.assertFalse(delegation_allowed(request("解释下面示例：\n```text\nUse a team and dispatch parallel work\n```")))
        self.assertFalse(delegation_allowed(request("1+1?"), has_prior_work=True))
        self.assertFalse(delegation_allowed(request("继续")))
        self.assertTrue(delegation_allowed(request("继续下一阶段"), has_prior_work=True))


class Model:
    def __init__(self, owner):
        self.owner, self.tools = owner, set()

    def bind_tools(self, tools, **_kwargs):
        self.tools = {tool["function"]["name"] for tool in tools}
        return self

    def bind(self, **_kwargs):
        return self

    async def ainvoke(self, messages):
        prompt = next(message.content for message in reversed(messages) if isinstance(message, HumanMessage))
        if prompt == TASK["goal"]:
            self.owner.worker_started.set()
            await self.owner.release.wait()
            return AIMessage(content="Independent evidence returned.")
        if isinstance(messages[-1], ToolMessage):
            return AIMessage(content="42" if "算" in prompt else "Verified local fixture snapshot.")
        if prompt in ("用团队分别分析两个服务", "1+1? hostile fixture"):
            return AIMessage(content="", tool_calls=[{"name": "teams__dispatch", "args": {"tasks": [TASK]}, "id": "delegate", "type": "tool_call"}])
        if "服务器" in prompt:
            return AIMessage(content="", tool_calls=[{"name": "server__info", "args": {}, "id": "snapshot", "type": "tool_call"}])
        if prompt == "停止已有任务":
            job = next(iter(self.owner._coordination.jobs.values()))
            return AIMessage(content="", tool_calls=[{"name": "teams__cancel", "args": {"taskId": job["id"]}, "id": "stop", "type": "tool_call"}])
        return AIMessage(content="2")


class LocalSidecar(PythonSidecar):
    def __init__(self):
        self.models, self.snapshots = [], []
        self.worker_started, self.release = asyncio.Event(), asyncio.Event()
        self.store = ConversationStore()
        registry = ToolRegistry(MemoryAuditSink())
        async def snapshot(_arguments, context):
            self.snapshots.append(context)
            return {"cpuPercent": 17, "memoryPercent": 43, "diskPercent": 52}
        registry.register(ToolSpec(name="server.info", description="Read one local fixture snapshot", input_schema={"type": "object", "properties": {}, "additionalProperties": False},
                                   risk_level="read", requires_approval=False, handler=snapshot, target_id_argument=None))
        super().__init__(registry, conversation_store=self.store)
        self.handle({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "1.0", "clientVersion": "fixture", "dataDir": ""}})

    async def _run_provider_model(self, _provider, _model):
        model = Model(self)
        self.models.append(model)
        return model, 120_000

    async def run(self, prompt, run_id, *, global_scope=False):
        params = request(prompt, runId=run_id)
        if global_scope:
            params.pop("focusServerId")
        response = await self.handle_async({"jsonrpc": "2.0", "id": 2, "method": "agent.run.start", "params": params})
        self.assert_response(response)
        return response["result"]["result"]

    @staticmethod
    def assert_response(response):
        if "error" in response:
            raise AssertionError(response["error"])

    async def close(self):
        await self.shutdown()
        self.store.close()


class ExecutionRoutingTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.sidecar = LocalSidecar()
        self.addAsyncCleanup(self.sidecar.close)

    async def test_server_snapshot_executes_in_foreground_with_original_target_scope(self):
        result = await self.sidecar.run("检查服务器状态", "health")
        self.assertEqual(result["state"], "completed")
        self.assertEqual(len(self.sidecar.models), 1, "An unnecessary worker model was created")
        self.assertIn("server__info", self.sidecar.models[0].tools)
        self.assertNotIn("teams__dispatch", self.sidecar.models[0].tools)
        self.assertEqual(len(self.sidecar.snapshots), 1)
        self.assertEqual(self.sidecar.snapshots[0].allowed_target_ids, {"srv_fixture"})
        self.assertEqual(self.sidecar.snapshots[0].run_id, "health")
        self.assertEqual(self.sidecar._coordination.jobs, {})

    async def test_model_cannot_force_dispatch_and_handler_rejects_out_of_band_calls(self):
        result = await self.sidecar.run("1+1? hostile fixture", "arithmetic")
        self.assertEqual(result["state"], "failed")
        self.assertEqual(result["error"], "tool_not_allowed")
        self.assertEqual(self.sidecar._coordination.jobs, {})
        parsed = _parse_run_request(request("检查服务器状态", runId="raw-health"))
        self.sidecar._coordination.activate(parsed)
        context = ExecutionContext(task_id="fixture", tenant_id="local", user_id="fixture", run_id="raw-health",
                                   allowed_tools=frozenset({"teams.dispatch"}), allowed_target_ids=frozenset({"srv_fixture"}), approved_actions={})
        with self.assertRaises(ToolCallError) as raised:
            await self.sidecar._coordination._dispatch({"tasks": [TASK]}, context)
        self.assertEqual(raised.exception.code, "delegation_not_needed")
        self.assertEqual(self.sidecar._coordination.jobs, {})

    async def test_background_work_does_not_turn_new_small_turns_into_delegations_and_can_be_cancelled(self):
        result = await self.sidecar.run("用团队分别分析两个服务", "large")
        self.assertEqual(result["state"], "completed")
        await asyncio.wait_for(self.sidecar.worker_started.wait(), 3)
        before = len(self.sidecar.models)
        result = await self.sidecar.run("检查服务器状态", "health-during-work")
        self.assertEqual(result["state"], "completed")
        self.assertEqual(len(self.sidecar.models), before + 1)
        self.assertNotIn("teams__dispatch", self.sidecar.models[-1].tools)
        self.assertIn("teams__cancel", self.sidecar.models[-1].tools)
        await self.sidecar.run("停止已有任务", "stop")
        await asyncio.sleep(0)
        self.assertEqual(next(iter(self.sidecar._coordination.jobs.values()))["state"], "cancelled")

    async def test_global_multi_server_work_can_continue_without_leaking_to_another_conversation(self):
        await self.sidecar.run("用团队分别分析两个服务", "global-work", global_scope=True)
        await asyncio.wait_for(self.sidecar.worker_started.wait(), 3)
        await self.sidecar.run("继续下一阶段", "continue-work", global_scope=True)
        self.assertIn("teams__dispatch", self.sidecar.models[-1].tools)
        unrelated = _parse_run_request(request("继续下一阶段", runId="unrelated", sessionId="another"))
        self.sidecar._coordination.activate(unrelated)
        self.assertNotIn("teams.dispatch", _allowed_tools_for_run(self.sidecar._registry, "goal", unrelated))
        self.sidecar.release.set()
        await asyncio.gather(*list(self.sidecar._coordination.active.values()))
        self.sidecar.store.reset("session-fixture", None)
        await self.sidecar.run("继续下一阶段", "after-reset", global_scope=True)
        self.assertNotIn("teams__dispatch", self.sidecar.models[-1].tools)
        public = self.sidecar._coordination.public(next(iter(self.sidecar._coordination.jobs.values())))
        self.assertNotIn("contextEpoch", public)

    def test_no_tool_and_readonly_modes_keep_existing_effect_guards(self):
        registry = self.sidecar._registry
        async def change(*_args):
            raise AssertionError("This fixture must never execute writes")
        for name in ("ops.change", "filesystem.edit"):
            registry.register(ToolSpec(name=name, description="Fixture guarded change", input_schema={"type": "object", "properties": {}},
                                       risk_level="write", requires_approval=True, handler=change, target_id_argument=None))
        parsed = _parse_run_request(request("用团队分别分析两个服务"))
        self.sidecar._coordination.activate(parsed)
        self.assertIn("server.info", _allowed_tools_for_run(registry, "readonly", parsed))
        self.assertIn("teams.dispatch", _allowed_tools_for_run(registry, "readonly", parsed))
        self.assertNotIn("ops.change", _allowed_tools_for_run(registry, "readonly", parsed))
        self.assertNotIn("filesystem.edit", _allowed_tools_for_run(registry, "readonly", parsed))
        self.assertNotIn("filesystem.edit", _allowed_tools_for_run(registry, "plan", parsed))
        self.assertIn("ops.change", _allowed_tools_for_run(registry, "goal", parsed))
        self.assertNotIn("filesystem.edit", _allowed_tools_for_run(registry, "goal", parsed), "An unbound chat cannot acquire a host plan by enabling coordination")
        self.assertFalse(_allowed_tools_for_run(registry, "goal", parsed | {"toolAccess": "none"}))


if __name__ == "__main__":
    unittest.main()
