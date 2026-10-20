import json
import unittest
from unittest.mock import AsyncMock
from langchain_core.messages import AIMessage, HumanMessage
from yukinal_agent.sidecar import PythonSidecar, _run_budget, _parse_run_request
from yukinal_agent.inference_policy import select_inference
from yukinal_agent import MemoryAuditSink, ModelDrivenAgent, ToolRegistry
from yukinal_agent.model_agent import _tool_output_summary
from test_agent_model_loop import ScriptedModel, _registry


class RuntimeOutputBudgetTests(unittest.IsolatedAsyncioTestCase):
    async def test_emitted_command_envelope_keeps_json_valid_after_redaction(self):
        async def handler(*_):
            return {"state": "completed", "exitCode": 0, "durationMs": 1,
                "stdout": 'PASSWORD="private-value"\n<html data-secret="private-markup">\n' + 'output\n' * 500,
                "stderr": "", "stdoutTruncated": False, "stderrTruncated": False}
        events = []
        model = ScriptedModel([AIMessage(content="", tool_calls=[{
            "name": "ops__read", "args": {"target_id": "stage-1"}, "id": "read", "type": "tool_call"}]), AIMessage(content="verified")])
        result = await ModelDrivenAgent(_registry(handler), model, event_sink=events.append).run(
            task_id="summary", tenant_id="tenant-1", user_id="user-1", prompt="inspect",
            allowed_tools={"ops.read"}, allowed_target_ids={"stage-1"})
        self.assertEqual(result.status, "succeeded")
        event = next(item for item in events if item["type"] == "agent.tool_result")
        self.assertEqual(json.loads(event["outputSummary"])["exitCode"], 0)
        self.assertNotIn("private-value", event["outputSummary"])
        self.assertNotIn("private-markup", event["outputSummary"])

    def test_large_command_summary_remains_valid_json_with_visible_excerpt_flags(self):
        value = {"state": "completed", "exitCode": 0, "durationMs": 123,
                 "stdout": 'start\n' + '😀 "quoted"\n' * 2000 + 'end\n',
                 "stderr": 'password=private-test-value\n' + '\t' * 5000,
                 "stdoutTruncated": False, "stderrTruncated": False}
        summary = _tool_output_summary(value)
        parsed = json.loads(summary)
        self.assertLessEqual(len(summary.encode('utf-16-le')) // 2, 4000)
        self.assertEqual(parsed["exitCode"], 0)
        self.assertTrue(parsed["stdoutTruncated"])
        self.assertTrue(parsed["stderrTruncated"])
        self.assertTrue(parsed["stdout"].startswith('start\n'))
        self.assertTrue(parsed["stdout"].endswith('end\n'))
        self.assertNotIn('private-test-value', summary)

    def test_explicit_deployment_budget_is_honored_with_finite_caps(self):
        default = _run_budget(None)
        self.assertEqual((default.max_model_rounds, default.max_total_seconds), (8, 120))
        deployment = _run_budget({'maxSteps': 24, 'maxRunMs': 600000, 'maxAttempts': 1})
        self.assertEqual((deployment.max_tool_calls, deployment.max_model_rounds, deployment.max_total_seconds), (24, 24, 600))
        capped = _run_budget({'maxSteps': 10000, 'maxRunMs': 86400000, 'maxAttempts': 32})
        self.assertEqual((capped.max_tool_calls, capped.max_model_rounds, capped.max_total_seconds), (64, 32, 900))

    def test_goal_request_can_finish_setup_without_enlarging_readonly_budget(self):
        request = {'runId':'budget-run','sessionId':'budget-session','prompt':'deploy'}
        goal = _parse_run_request({**request, 'mode':'goal'})['agentBudget']
        readonly = _parse_run_request({**request, 'mode':'readonly'})['agentBudget']
        self.assertEqual((goal.max_tool_calls, goal.max_model_rounds, goal.max_total_seconds), (None, None, 600))
        self.assertEqual((readonly.max_model_rounds, readonly.max_total_seconds), (8, 120))
        explicit = _parse_run_request({**request, 'mode':'goal', 'taskBudget':{'maxSteps':2, 'maxRunMs':3000, 'maxAttempts':1}})['agentBudget']
        self.assertEqual((explicit.max_model_rounds, explicit.max_total_seconds), (2, 3))

    async def test_legacy_model_run_does_not_inherit_connection_probe_limit(self):
        sidecar = PythonSidecar(ToolRegistry(MemoryAuditSink()))
        sidecar._provider_list = AsyncMock(return_value={'providers': [{
            'id': 'test', 'enabled': True, 'model': 'deepseek-chat',
        }]})
        sidecar._provider_key = AsyncMock(return_value='test-key')
        runtime = {'kind': 'openai-compatible', 'model': 'deepseek-chat',
                   'baseUrl': 'https://api.deepseek.com', 'wireApi': 'chat', 'apiKey': 'test-key'}
        sidecar._provider_runtime_config = lambda *_: dict(runtime)
        model, _ = await sidecar._run_provider_model(None, None)
        plan = select_inference(model, {'prompt': '部署网站并执行检查'})
        payload = plan.model.bind_tools([]).bind(max_tokens=plan.output_tokens)
        request = model._get_request_payload([HumanMessage(content='deploy')], **payload.kwargs)
        self.assertGreaterEqual(request['max_tokens'], 2048)
        probe, _ = sidecar._create_runtime_model(runtime)
        self.assertEqual(probe.max_tokens, 256)
