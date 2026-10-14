from __future__ import annotations

import asyncio
import json
import tempfile
import unittest
from pathlib import Path
from typing import Any

from yukinal_agent import (
    AllowlistAuthorization,
    ExecutionContext,
    JsonlAuditSink,
    MemoryAuditSink,
    ToolCallError,
    ToolRegistry,
    ToolSpec,
    approval_fingerprint,
)


def context(
    *,
    allowed_tools: frozenset[str] = frozenset({"ops.read"}),
    targets: frozenset[str] = frozenset({"staging-01"}),
    approved_actions: dict[str, str] | None = None,
) -> ExecutionContext:
    return ExecutionContext(
        task_id="task-1",
        tenant_id="tenant-1",
        user_id="user-1",
        allowed_tools=allowed_tools,
        allowed_target_ids=targets,
        approved_actions=approved_actions or {},
    )


def read_spec(handler: Any, *, name: str = "ops.read") -> ToolSpec:
    return ToolSpec(
        name=name,
        description="Read one staging value.",
        input_schema={
            "type": "object",
            "properties": {
                "target_id": {"type": "string"},
                "value": {"type": "string"},
                "token": {"type": "string"},
            },
            "required": ["target_id"],
            "additionalProperties": False,
        },
        risk_level="read",
        requires_approval=False,
        handler=handler,
        target_required=True,
    )


class RegistryTests(unittest.IsolatedAsyncioTestCase):
    async def test_schema_and_target_allowlist_are_checked_before_handler(self) -> None:
        audit = MemoryAuditSink()
        called = False

        async def handler(_arguments: dict[str, Any], _context: ExecutionContext) -> dict[str, bool]:
            nonlocal called
            called = True
            return {"ok": True}

        registry = ToolRegistry(audit, AllowlistAuthorization())
        registry.register(read_spec(handler))
        with self.assertRaises(ToolCallError) as invalid:
            await registry.execute("ops.read", {"target_id": "staging-01", "extra": True}, context())
        self.assertEqual(invalid.exception.code, "invalid_arguments")
        with self.assertRaises(ToolCallError) as denied:
            await registry.execute("ops.read", {"target_id": "prod-01"}, context())
        self.assertEqual(denied.exception.code, "target_not_allowed")
        self.assertFalse(called)
        self.assertEqual([event.status for event in audit.events], ["started", "failed", "started", "denied"])

    async def test_audit_never_records_raw_parameters_or_result_values(self) -> None:
        with tempfile.TemporaryDirectory() as folder:
            audit_path = Path(folder) / "audit.jsonl"
            registry = ToolRegistry(JsonlAuditSink(audit_path))

            async def handler(_arguments: dict[str, Any], _context: ExecutionContext) -> dict[str, str]:
                return {"secret_result": "another-secret"}

            registry.register(read_spec(handler))
            await registry.execute(
                "ops.read",
                {"target_id": "staging-01", "token": "do-not-log-this"},
                context(),
            )
            contents = audit_path.read_text(encoding="utf-8")
            self.assertNotIn("do-not-log-this", contents)
            self.assertNotIn("another-secret", contents)
            self.assertIn('"argument_keys":["target_id","token"]', contents)
            parsed = [json.loads(line) for line in contents.splitlines()]
            self.assertEqual([event["status"] for event in parsed], ["started", "succeeded"])
            self.assertEqual(parsed[-1]["result_summary"]["kind"], "object")

    async def test_approval_fingerprint_binds_exact_target_and_arguments(self) -> None:
        audit = MemoryAuditSink()

        async def handler(arguments: dict[str, Any], _context: ExecutionContext) -> dict[str, str]:
            return {"changed": arguments["value"]}

        spec = ToolSpec(
            name="ops.write",
            description="Write a value.",
            input_schema={
                "type": "object",
                "properties": {"target_id": {"type": "string"}, "value": {"type": "string"}},
                "required": ["target_id", "value"],
                "additionalProperties": False,
            },
            risk_level="write",
            requires_approval=True,
            handler=handler,
            target_required=True,
        )
        registry = ToolRegistry(audit)
        registry.register(spec)
        arguments = {"target_id": "staging-01", "value": "green"}
        with self.assertRaises(ToolCallError) as missing:
            await registry.execute("ops.write", arguments, context(allowed_tools=frozenset({"ops.write"})))
        self.assertEqual(missing.exception.code, "approval_required")

        fingerprint = approval_fingerprint(
            "ops.write", "staging-01", arguments,
            task_id="task-1", tenant_id="tenant-1", user_id="user-1",
        )
        approved = context(
            allowed_tools=frozenset({"ops.write"}),
            approved_actions={fingerprint: "approval-123"},
        )
        result = await registry.execute("ops.write", arguments, approved)
        self.assertEqual(result.value, {"changed": "green"})
        self.assertEqual(audit.events[-1].approval_status, "approved")
        other_task_fingerprint = approval_fingerprint(
            "ops.write", "staging-01", arguments,
            task_id="task-2", tenant_id="tenant-1", user_id="user-1",
        )
        self.assertNotEqual(fingerprint, other_task_fingerprint)

        changed = {**arguments, "value": "red"}
        with self.assertRaises(ToolCallError) as stale:
            await registry.execute("ops.write", changed, approved)
        self.assertEqual(stale.exception.code, "approval_required")

    async def test_timeout_and_output_limit_are_audited_as_failures(self) -> None:
        audit = MemoryAuditSink()

        async def slow(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            await asyncio.sleep(1)
            return "late"

        registry = ToolRegistry(audit)
        registry.register(read_spec(slow))
        with self.assertRaises(ToolCallError) as timed_out:
            await registry.execute("ops.read", {"target_id": "staging-01"}, context(), timeout_seconds=0.01)
        self.assertEqual(timed_out.exception.code, "execution_timeout")

        async def large(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            return "too much output"

        registry.register(read_spec(large, name="ops.large"))
        with self.assertRaises(ToolCallError) as oversized:
            await registry.execute(
                "ops.large",
                {"target_id": "staging-01"},
                context(allowed_tools=frozenset({"ops.large"})),
                max_output_bytes=4,
            )
        self.assertEqual(oversized.exception.code, "output_too_large")
        self.assertEqual([event.error_code for event in audit.events if event.status == "failed"], [
            "execution_timeout", "output_too_large"
        ])

    async def test_duplicate_tool_names_and_remote_schema_references_are_rejected(self) -> None:
        registry = ToolRegistry(MemoryAuditSink())

        async def handler(_arguments: dict[str, Any], _context: ExecutionContext) -> None:
            return None

        spec = read_spec(handler)
        registry.register(spec)
        with self.assertRaises(ValueError):
            registry.register(spec)
        with self.assertRaises(ValueError):
            ToolSpec(
                name="ops.remote",
                description="Remote schema.",
                input_schema={"type": "object", "properties": {"x": {"$ref": "https://example.invalid/schema"}}},
                risk_level="read",
                requires_approval=False,
                handler=handler,
            )


if __name__ == "__main__":
    unittest.main()
