from __future__ import annotations

import importlib.util
import os
import unittest
from typing import Any
from unittest.mock import patch

from yukinal_agent import (
    ExecutionContext,
    ModelConfig,
    ModelFactory,
    ModelFactoryError,
    PolicyEngine,
    PrincipalScope,
    ToolCallError,
    ToolRegistry,
    ToolSpec,
    MemoryAuditSink,
    ModelDrivenAgent,
)


class ModelFactoryTests(unittest.TestCase):
    def test_config_switches_provider_without_changing_agent_code(self) -> None:
        constructed: list[tuple[str, dict[str, Any]]] = []

        def openai_builder(**kwargs: Any) -> object:
            constructed.append(("openai", kwargs))
            return object()

        def anthropic_builder(**kwargs: Any) -> object:
            constructed.append(("anthropic", kwargs))
            return object()

        factory = ModelFactory({"openai": openai_builder, "anthropic": anthropic_builder})
        with patch.dict(os.environ, {"TEST_LLM_KEY": "placeholder-secret", "TEST_MODEL": "sample-model"}):
            factory.create(ModelConfig("openai", "${TEST_MODEL}", "TEST_LLM_KEY"))
            factory.create(ModelConfig("anthropic", "claude-test", "TEST_LLM_KEY"))

        self.assertEqual([provider for provider, _ in constructed], ["openai", "anthropic"])
        self.assertEqual(constructed[0][1]["model"], "sample-model")
        self.assertEqual(constructed[1][1]["model"], "claude-test")
        self.assertEqual(constructed[0][1]["api_key"], "placeholder-secret")
        self.assertEqual(constructed[0][1]["max_tokens"], 2048)

    def test_missing_secret_reference_is_classified_without_echoing_secret(self) -> None:
        with patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(ModelFactoryError) as missing:
                ModelFactory({"openai": lambda **_kwargs: object()}).create(
                    ModelConfig("openai", "model-x", "MISSING_LLM_KEY")
                )
        self.assertEqual(missing.exception.code, "missing_api_key")
        self.assertNotIn("MISSING_LLM_KEY", str(missing.exception))

    def test_official_adapters_build_and_bind_tools_without_network_access(self) -> None:
        if importlib.util.find_spec("langchain_openai") is None or importlib.util.find_spec("langchain_anthropic") is None:
            self.skipTest("install the optional models extra to check provider adapters")
        with patch.dict(os.environ, {"TEST_LLM_KEY": "not-a-real-key"}):
            for provider, model_name in (("openai", "gpt-test"), ("anthropic", "claude-test")):
                with self.subTest(provider=provider):
                    model = ModelFactory().create(ModelConfig(provider, model_name, "TEST_LLM_KEY"))
                    agent = ModelDrivenAgent(ToolRegistry(MemoryAuditSink()), model)
                    self.assertIsNotNone(agent)


class PolicyEngineTests(unittest.IsolatedAsyncioTestCase):
    async def test_each_call_is_checked_against_tenant_user_and_risk_scope(self) -> None:
        async def handler(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            return "ok"

        registry = ToolRegistry(
            MemoryAuditSink(),
            PolicyEngine({
                ("tenant-1", "user-1"): PrincipalScope(
                    allowed_tools=frozenset({"ops.read", "ops.write", "ops.destroy"}),
                    allowed_target_ids=frozenset({"stage-1"}),
                    allowed_risk_levels=frozenset({"read", "write"}),
                )
            }),
        )
        for name, risk in (("ops.read", "read"), ("ops.write", "write")):
            registry.register(ToolSpec(
                name=name,
                description="Scoped operation.",
                input_schema={
                    "type": "object",
                    "properties": {"target_id": {"type": "string"}},
                    "required": ["target_id"],
                    "additionalProperties": False,
                },
                risk_level=risk,
                requires_approval=risk == "write",
                handler=handler,
                target_required=True,
            ))
        registry.register(ToolSpec(
            name="ops.destroy",
            description="Remove a controlled resource.",
            input_schema={
                "type": "object",
                "properties": {"target_id": {"type": "string"}},
                "required": ["target_id"],
                "additionalProperties": False,
            },
            risk_level="destructive",
            requires_approval=True,
            handler=handler,
            target_required=True,
        ))
        wrong_tenant = ExecutionContext(
            "task-1", "tenant-2", "user-1", frozenset({"ops.read"}), frozenset({"stage-1"}), {}
        )
        with self.assertRaises(ToolCallError) as denied:
            await registry.execute("ops.read", {"target_id": "stage-1"}, wrong_tenant)
        self.assertEqual(denied.exception.code, "tool_not_allowed")

        write_context = ExecutionContext(
            "task-2", "tenant-1", "user-1", frozenset({"ops.write"}), frozenset({"stage-1"}), {}
        )
        with self.assertRaises(ToolCallError) as approval:
            await registry.execute("ops.write", {"target_id": "stage-1"}, write_context)
        self.assertEqual(approval.exception.code, "approval_required")
        self.assertEqual(len(approval.exception.approval_fingerprint or ""), 64)

        destructive_context = ExecutionContext(
            "task-3", "tenant-1", "user-1", frozenset({"ops.destroy"}), frozenset({"stage-1"}), {}
        )
        with self.assertRaises(ToolCallError) as risky:
            await registry.execute("ops.destroy", {"target_id": "stage-1"}, destructive_context)
        self.assertEqual(risky.exception.code, "risk_not_allowed")


if __name__ == "__main__":
    unittest.main()
