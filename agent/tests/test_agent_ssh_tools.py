from __future__ import annotations

import unittest
from typing import Any

from yukinal_agent import AgentWorkflow, ExecutionContext, MemoryAuditSink, ToolCallError, ToolRegistry
from yukinal_agent.ssh_tools import register_ssh_tools


class FakeSshServiceClient:
    def __init__(self) -> None:
        self.calls: list[tuple[str, str, int]] = []

    def service_status(self, target_id: str, service: str, timeout_seconds: int) -> Any:
        self.calls.append((target_id, service, timeout_seconds))
        return type("Result", (), {
            "target_id": target_id,
            "operation": "service_status",
            "exit_code": 0,
            "stdout": "active\n",
            "stderr": "",
            "duration_ms": 12,
            "output_truncated": False,
        })()


def _context(target_id: str | None, allowed_targets: set[str]) -> ExecutionContext:
    host_target = (
        {"host": "remote", "serverId": target_id, "environment": "staging"}
        if target_id is not None
        else {"host": "local", "environment": "local"}
    )
    return ExecutionContext(
        task_id="task-ssh-1",
        tenant_id="tenant-1",
        user_id="operator-1",
        allowed_tools=frozenset({"ssh.service_status"}),
        allowed_target_ids=frozenset(allowed_targets),
        approved_actions={},
        host_target=host_target,
        context_target_id=target_id,
    )


class SshToolTests(unittest.IsolatedAsyncioTestCase):
    async def test_selected_server_is_bound_by_host_context(self) -> None:
        client = FakeSshServiceClient()
        registry = ToolRegistry(MemoryAuditSink())
        register_ssh_tools(registry, client)  # type: ignore[arg-type]
        spec = registry.get("ssh.service_status")
        self.assertIsNotNone(spec)
        self.assertNotIn("target_id", spec.input_schema["properties"])

        result = await AgentWorkflow(registry).run(
            task_id="task-ssh-1",
            tenant_id="tenant-1",
            user_id="operator-1",
            allowed_tools={"ssh.service_status"},
            allowed_target_ids={"srv_stage1"},
            tool_calls=[{
                "name": "ssh.service_status",
                "arguments": {"service": "nginx.service"},
            }],
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        )

        self.assertEqual(result.status, "succeeded")
        self.assertEqual(client.calls, [("srv_stage1", "nginx.service", 10)])
        self.assertEqual(result.results[0]["result"]["target_id"], "srv_stage1")

    async def test_unselected_server_is_rejected_before_http_call(self) -> None:
        client = FakeSshServiceClient()
        registry = ToolRegistry(MemoryAuditSink())
        register_ssh_tools(registry, client)  # type: ignore[arg-type]

        with self.assertRaises(ToolCallError) as raised:
            await registry.execute(
                "ssh.service_status",
                {"service": "nginx.service"},
                _context("srv_other", {"srv_stage1"}),
            )

        self.assertEqual(raised.exception.code, "target_not_allowed")
        self.assertEqual(client.calls, [])

    async def test_model_cannot_override_the_selected_server(self) -> None:
        client = FakeSshServiceClient()
        registry = ToolRegistry(MemoryAuditSink())
        register_ssh_tools(registry, client)  # type: ignore[arg-type]

        with self.assertRaises(ToolCallError) as raised:
            await registry.execute(
                "ssh.service_status",
                {"target_id": "srv_other", "service": "nginx.service"},
                _context("srv_stage1", {"srv_stage1"}),
            )

        self.assertEqual(raised.exception.code, "invalid_arguments")
        self.assertEqual(client.calls, [])


if __name__ == "__main__":
    unittest.main()
