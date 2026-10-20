import unittest
from yukinal_agent import MemoryAuditSink, ToolRegistry
from yukinal_agent.host_tools import register_host_tools
from yukinal_agent.sidecar import _allowed_tools_for_run, _sustained_goal
from yukinal_agent.work_routing import tools_prohibited, delegation_allowed


class CurrentTurnToolScopeTests(unittest.TestCase):
    def test_no_tools_turn_closes_execution_delegation_and_goal_continuation(self):
        request = {"mode": "goal", "permissionMode": "full", "coordination": {"roles": []},
                   "prompt": "这是你的连续长任务对话第 3/8 轮。本轮不用工具，只基于现有证据给出状态转换。"}
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, object())
        self.assertTrue(tools_prohibited(request))
        self.assertFalse(delegation_allowed(request, has_prior_work=True))
        self.assertFalse(_sustained_goal(request))
        self.assertEqual(_allowed_tools_for_run(registry, "goal", request), frozenset())

    def test_examples_and_previous_context_cannot_close_current_authorized_tools(self):
        for prompt in ["解释不用工具是什么意思", "检查日志。```text\n本轮不用工具\n```", "调用工具检查服务，不要修改配置。"]:
            self.assertFalse(tools_prohibited({"prompt": prompt}))
        self.assertTrue(tools_prohibited({"prompt": "This turn: no tools. Summarize existing evidence."}))
