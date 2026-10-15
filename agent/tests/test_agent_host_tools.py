from __future__ import annotations

import unittest
from typing import Any

from langchain_core.messages import AIMessage

from yukinal_agent import ExecutionContext, MemoryAuditSink, ModelDrivenAgent, ToolCallError, ToolRegistry
from yukinal_agent.host_tools import register_host_tools
from yukinal_agent.tools import approval_fingerprint


class FakeHost:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any], float]] = []

    async def request(self, method: str, params: dict[str, Any], *, timeout_seconds: float) -> dict[str, Any]:
        self.calls.append((method, params, timeout_seconds))
        return {"status": "success", "output": {"state": "exited", "exitCode": 0}}


class ScriptedModel:
    def __init__(self, responses: list[AIMessage]) -> None:
        self.responses = list(responses)

    def bind_tools(self, _tools: list[dict[str, Any]], *, parallel_tool_calls: bool) -> ScriptedModel:
        return self

    def bind(self, *, max_tokens: int) -> ScriptedModel:
        return self

    async def ainvoke(self, _messages: list[Any]) -> AIMessage:
        if not self.responses:
            raise AssertionError("scripted model ran out of responses")
        return self.responses.pop(0)


class HostActionToolTests(unittest.IsolatedAsyncioTestCase):
    async def test_structured_write_tools_forward_exact_host_approval_tickets(self) -> None:
        host = FakeHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        cases = {
            "docker.restart": {"container": "api", "timeoutSeconds": 20},
            "systemd.restart": {"service": "api.service", "timeoutSeconds": 20},
            "package.install": {"manager": "apt", "package": "nginx", "timeoutSeconds": 25},
        }

        for index, (tool_name, arguments) in enumerate(cases.items(), start=1):
            fingerprint = approval_fingerprint(
                tool_name, "srv_stage1", arguments,
                task_id=f"agent-task-{index}", tenant_id="local", user_id="agent-session-1",
            )
            context = ExecutionContext(
                task_id=f"agent-task-{index}",
                tenant_id="local",
                user_id="agent-session-1",
                allowed_tools=frozenset({tool_name}),
                allowed_target_ids=frozenset({"srv_stage1"}),
                approved_actions={fingerprint: f"apr-host-ticket-{index}"},
                host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
                host_task_id=f"task-durable-{index}",
                plan_id="plan-current-1",
                plan_step_id=f"step-action-{index}",
                evidence_ids=("ev-1",),
                run_id=f"run-{index}",
                trace_id=f"trace-{index}",
                context_target_id="srv_stage1",
            )

            await registry.execute(tool_name, arguments, context, call_id=f"call-{index}")

        self.assertEqual(len(host.calls), 3)
        for index, (tool_name, arguments) in enumerate(cases.items(), start=1):
            request = host.calls[index - 1][1]
            self.assertEqual(request["toolName"], tool_name)
            self.assertEqual(request["approvalId"], f"apr-host-ticket-{index}")
            self.assertEqual(request["planId"], "plan-current-1")
            self.assertEqual(request["planStepId"], f"step-action-{index}")
            self.assertEqual(request["evidenceIds"], ["ev-1"])
            self.assertEqual(request["input"], arguments)

    async def test_resumed_model_approval_reaches_the_host_with_its_exact_ticket(self) -> None:
        host = FakeHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        arguments = {
            "command": "systemctl restart api.service",
            "purpose": "Restart the failed API service",
            "timeoutMs": 15000,
            "maxOutputBytes": 4096,
        }
        model = ScriptedModel([
            AIMessage(content="", tool_calls=[{
                "name": "server.exec", "args": arguments, "id": "action-call-1", "type": "tool_call",
            }]),
            AIMessage(content="The approved action completed."),
        ])
        agent = ModelDrivenAgent(registry, model)
        first = await agent.run(
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
            prompt="Restart the failed API service.",
            allowed_tools={"server.exec"},
            allowed_target_ids={"srv_stage1"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-current-1",
            plan_step_id="step-action-1",
            evidence_ids=("ev-1", "ev-2"),
            run_id="run-1",
            trace_id="trace-1",
        )

        self.assertEqual(first.status, "awaiting_approval")
        self.assertEqual(host.calls, [])
        resumed = await agent.resume_after_approval(
            first.checkpoint_id or "",
            first.approval_requests[0].fingerprint,
            "apr-host-ticket",
            approved=True,
        )

        self.assertEqual(resumed.status, "succeeded")
        self.assertEqual(len(host.calls), 1)
        _, request, _ = host.calls[0]
        self.assertEqual(request["approvalId"], "apr-host-ticket")
        self.assertEqual(request["input"], arguments)
        self.assertEqual(request["planId"], "plan-current-1")
        self.assertEqual(request["planStepId"], "step-action-1")
        self.assertEqual(request["evidenceIds"], ["ev-1", "ev-2"])

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


class FakeEvidenceHost:
    def __init__(self, *, include_raw_body: bool = False) -> None:
        self.calls: list[tuple[str, dict[str, Any], float]] = []
        self.include_raw_body = include_raw_body

    async def request(self, method: str, params: dict[str, Any], *, timeout_seconds: float) -> dict[str, Any]:
        self.calls.append((method, params, timeout_seconds))
        left = {
            "id": "ev_1",
            "taskId": "task-durable-1",
            "scope": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            "kind": "snapshot",
            "sourceTool": "server.info",
            "collectedAt": "2026-10-15T00:00:00Z",
            "inputSummary": "{}",
            "contentType": "json",
            "contentHash": "a" * 64,
            "truncated": False,
            "redactionStatus": "clean",
        }
        if self.include_raw_body:
            left["content"] = {"secret": "must not reach the Agent"}
        if method == "host.investigation.evidence.search":
            return {"status": "success", "evidence": [left]}
        return {
            "status": "success",
            "comparison": {
                "status": "identical",
                "shape": "json",
                "left": left,
                "right": {**left, "id": "ev_2", "contentHash": "b" * 64},
                "changedPaths": [],
                "changedPathCount": 0,
                "diffTruncated": False,
                "warnings": [],
            },
        }


class HostEvidenceToolTests(unittest.IsolatedAsyncioTestCase):
    def make_context(self, *, host_task_id: str | None = "task-durable-1") -> ExecutionContext:
        return ExecutionContext(
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
            allowed_tools=frozenset({"investigation.evidence.search", "investigation.evidence.compare"}),
            allowed_target_ids=frozenset(),
            approved_actions={},
            host_task_id=host_task_id,
        )

    async def test_search_and_compare_use_the_host_task_and_return_body_free_results(self) -> None:
        host = FakeEvidenceHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]

        search = await registry.execute(
            "investigation.evidence.search",
            {"sourceTool": "server.info", "kind": "snapshot", "limit": 8},
            self.make_context(),
            call_id="call-search",
        )
        compare = await registry.execute(
            "investigation.evidence.compare",
            {"leftEvidenceId": "ev_1", "rightEvidenceId": "ev_2"},
            self.make_context(),
            call_id="call-compare",
        )

        self.assertEqual(search.value["evidence"][0]["id"], "ev_1")
        self.assertNotIn("content", search.value["evidence"][0])
        self.assertEqual(compare.value["comparison"]["status"], "identical")
        self.assertNotIn("content", compare.value["comparison"]["left"])
        self.assertEqual(
            [method for method, _params, _timeout in host.calls],
            ["host.investigation.evidence.search", "host.investigation.evidence.compare"],
        )
        self.assertEqual(host.calls[0][1], {
            "taskId": "task-durable-1",
            "sourceTool": "server.info",
            "kind": "snapshot",
            "limit": 8,
        })
        self.assertEqual(host.calls[1][1], {
            "taskId": "task-durable-1",
            "leftEvidenceId": "ev_1",
            "rightEvidenceId": "ev_2",
        })
        self.assertEqual([timeout for _method, _params, timeout in host.calls], [15.0, 15.0])

    async def test_evidence_tools_require_a_durable_host_task_and_validate_bounds(self) -> None:
        host = FakeEvidenceHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]

        with self.assertRaises(ToolCallError):
            await registry.execute(
                "investigation.evidence.search", {}, self.make_context(host_task_id=None), call_id="no-task"
            )
        with self.assertRaises(ToolCallError):
            await registry.execute(
                "investigation.evidence.search", {"limit": 65}, self.make_context(), call_id="bad-limit"
            )
        with self.assertRaises(ToolCallError):
            await registry.execute(
                "investigation.evidence.compare", {"leftEvidenceId": "ev_1"}, self.make_context(), call_id="incomplete"
            )
        self.assertEqual(host.calls, [])

    async def test_host_response_cannot_smuggle_raw_evidence_bodies(self) -> None:
        host = FakeEvidenceHost(include_raw_body=True)
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]

        with self.assertRaises(ToolCallError) as raised:
            await registry.execute(
                "investigation.evidence.search", {}, self.make_context(), call_id="raw-body"
            )

        self.assertEqual(raised.exception.code, "invalid_host_response")


if __name__ == "__main__":
    unittest.main()
