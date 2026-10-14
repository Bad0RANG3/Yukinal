from __future__ import annotations

import unittest
from typing import Any

from yukinal_agent import ExecutionContext, MemoryAuditSink, ToolCallError, ToolRegistry
from yukinal_agent.host_tools import register_host_tools
from yukinal_agent.tools import approval_fingerprint


class FakeHost:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any], float]] = []

    async def request(self, method: str, params: dict[str, Any], *, timeout_seconds: float) -> dict[str, Any]:
        self.calls.append((method, params, timeout_seconds))
        return {"status": "success", "output": {"state": "exited", "exitCode": 0}}


class HostActionToolTests(unittest.IsolatedAsyncioTestCase):
    async def test_server_exec_forwards_exact_approval_and_current_plan_binding(self) -> None:
        host = FakeHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        arguments = {
            "command": "systemctl restart api.service",
            "purpose": "Restart the failed API service",
            "timeoutMs": 15000,
            "maxOutputBytes": 4096,
        }
        fingerprint = approval_fingerprint(
            "server.exec", "srv_stage1", arguments,
            task_id="agent-task-1", tenant_id="local", user_id="agent-session-1",
        )
        context = ExecutionContext(
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
            allowed_tools=frozenset({"server.exec"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-host-ticket"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-current-1",
            plan_step_id="step-action-1",
            evidence_ids=("ev-1", "ev-2"),
            run_id="run-1",
            trace_id="trace-1",
            context_target_id="srv_stage1",
        )

        result = await registry.execute("server.exec", arguments, context, call_id="call-1")

        self.assertEqual(result.value["exitCode"], 0)
        self.assertEqual(len(host.calls), 1)
        method, request, timeout = host.calls[0]
        self.assertEqual(method, "host.tool.execute")
        self.assertEqual(request["approvalId"], "apr-host-ticket")
        self.assertEqual(request["planId"], "plan-current-1")
        self.assertEqual(request["planStepId"], "step-action-1")
        self.assertEqual(request["evidenceIds"], ["ev-1", "ev-2"])
        self.assertEqual(request["input"], arguments)
        self.assertEqual(request["callId"], "call-1")
        self.assertEqual(timeout, 45.0)

    async def test_server_exec_fails_closed_without_durable_plan_binding(self) -> None:
        host = FakeHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        arguments = {
            "command": "systemctl restart api.service",
            "purpose": "Restart the failed API service",
            "timeoutMs": 15000,
            "maxOutputBytes": 4096,
        }
        fingerprint = approval_fingerprint(
            "server.exec", "srv_stage1", arguments,
            task_id="agent-task-1", tenant_id="local", user_id="agent-session-1",
        )
        context = ExecutionContext(
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
            allowed_tools=frozenset({"server.exec"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-host-ticket"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            context_target_id="srv_stage1",
        )

        with self.assertRaises(ToolCallError) as raised:
            await registry.execute("server.exec", arguments, context, call_id="call-1")

        self.assertEqual(raised.exception.code, "durable_plan_required")
        self.assertEqual(host.calls, [])


if __name__ == "__main__":
    unittest.main()
