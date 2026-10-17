import unittest
from langchain_core.messages import HumanMessage
from yukinal_agent.models import ModelConfig, ModelFactory, ModelFactoryError, with_reasoning_effort
from yukinal_agent.sidecar import _RpcFault, _parse_run_request, _run_fingerprint, _run_checkpoint_binding


class ReasoningTests(unittest.TestCase):
    def test_effort_survives_tool_and_output_binding(self):
        model = ModelFactory().create_with_api_key(ModelConfig("openai-compatible", "deepseek-flash", base_url="https://api.deepseek.com", wire_api="chat"), "test-key")
        for effort in ("none", "low", "high", "max"):
            selected = with_reasoning_effort(model, effort)
            bound = selected.bind_tools([]).bind(max_tokens=8192)
            payload = selected._get_request_payload([HumanMessage(content="Hello")], **bound.kwargs)
            self.assertEqual(payload["extra_body"]["thinking"]["type"], "disabled" if effort == "none" else "enabled")
            self.assertEqual(payload.get("reasoning_effort"), None if effort == "none" else effort)
            self.assertEqual(payload["max_tokens"], 8192)
        self.assertIsNone(model.reasoning_effort)
        self.assertIsNone(model.extra_body)

    def test_unsupported_adapter_rejects_explicit_effort(self):
        model = ModelFactory().create_with_api_key(ModelConfig("openai-compatible", "local-model", base_url="http://127.0.0.1:1234/v1"), None)
        with self.assertRaises(ModelFactoryError):
            with_reasoning_effort(model, "max")

    def test_effort_is_validated_and_bound_to_admission_and_checkpoint(self):
        request = {"runId": "run-1", "sessionId": "session-1", "prompt": "hello"}
        original = _parse_run_request(request)
        selected = _parse_run_request({**request, "reasoningEffort": "max"})
        self.assertNotEqual(_run_fingerprint(original), _run_fingerprint(selected))
        self.assertNotEqual(_run_checkpoint_binding(original), _run_checkpoint_binding(selected))
        for invalid in (None, "ultra", 5, {}, []):
            with self.assertRaises(_RpcFault):
                _parse_run_request({**request, "reasoningEffort": invalid})
