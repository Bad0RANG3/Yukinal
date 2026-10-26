from __future__ import annotations

import unittest
from typing import Any

from langchain_core.messages import AIMessage

from yukinal_agent import ExecutionContext, MemoryAuditSink, ModelDrivenAgent, ToolCallError, ToolRegistry
from yukinal_agent.host_tools import (
    FILESYSTEM_ACTION_TOOLS,
    register_desktop_backup_metadata_tools,
    register_host_tools,
)
from yukinal_agent.tools import approval_fingerprint


class FakeHost:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any], float]] = []

    async def request(self, method: str, params: dict[str, Any], *, timeout_seconds: float) -> dict[str, Any]:
        self.calls.append((method, params, timeout_seconds))
        return {"status": "success", "output": {"state": "exited", "exitCode": 0}}


class FakeBackupMetadataHost:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any], float]] = []
        self.list_row = {
            "id": "backup_1",
            "serverId": "srv_stage1",
            "taskId": None,
            "path": "/etc/app.conf",
            "backupPath": "/etc/.yukinal-backup-a",
            "revision": "a" * 64,
            "bytesBackedUp": 128,
            "status": "available",
            "createdAt": "2020-01-01T00:00:00Z",
            "updatedAt": "2020-01-01T00:00:00Z",
            "restoredAt": None,
            "deletedAt": None,
        }
        self.retention_rows = [
            {
                "id": "backup_old",
                "path": "/etc/app.conf",
                "backupPath": "/etc/.yukinal-backup-old",
                "revision": "b" * 64,
                "taskId": "task-owner-other",
                "createdAt": "2020-01-01T00:00:00Z",
                "bytesBackedUp": 10,
            },
            {
                "id": "backup_new",
                "path": "/etc/app.conf",
                "backupPath": "/etc/.yukinal-backup-new",
                "revision": "c" * 64,
                "taskId": "task-durable-1",
                "createdAt": "2021-01-01T00:00:00Z",
                "bytesBackedUp": 20,
            },
            {
                "id": "backup_single",
                "path": "/etc/only.conf",
                "backupPath": "/etc/.yukinal-backup-only",
                "revision": "d" * 64,
                "taskId": None,
                "createdAt": "2020-01-01T00:00:00Z",
                "bytesBackedUp": 30,
            },
        ]

    async def request(self, method: str, params: dict[str, Any], *, timeout_seconds: float) -> dict[str, Any]:
        self.calls.append((method, params, timeout_seconds))
        if method == "host.filesystem.backup.list_query":
            return {"status": "success", "output": {"records": [self.list_row], "truncated": False}}
        if method == "host.filesystem.backup.retention_scan":
            return {
                "status": "success",
                "output": {
                    "records": self.retention_rows,
                    "truncated": False,
                    "evaluatedAtEpochSeconds": 1_800_000_000,
                },
            }
        raise AssertionError(f"unexpected host metadata method: {method}")


class FakePlanHost:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any], float]] = []

    async def request(self, method: str, params: dict[str, Any], *, timeout_seconds: float) -> dict[str, Any]:
        self.calls.append((method, params, timeout_seconds))
        if method == "host.investigation.plan.record":
            plan = dict(params["plan"])
            plan["revision"] = 1
            plan["status"] = "active"
            plan["currentStepId"] = plan["steps"][0]["id"]
            return {"recorded": True, "plan": plan}
        if method == "host.tool.execute":
            return {"status": "success", "output": {"status": "active"}}
        raise AssertionError(f"unexpected plan host method: {method}")


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
    async def test_model_records_durable_plan_then_uses_host_binding_for_ssh_read(self) -> None:
        host = FakePlanHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        model = ScriptedModel([
            AIMessage(content="", tool_calls=[{
                "name": "investigation.plan",
                "args": {"steps": [{
                    "id": "inspect-server",
                    "ordinal": 0,
                    "kind": "evidence",
                    "title": "Inspect server health",
                    "purpose": "Collect current server status before deciding what to do.",
                    "allowedTools": ["server__info"],
                    "successCriteria": ["The host returns a structured status snapshot."],
                    "requiresApproval": False,
                }]},
                "id": "plan-call-1",
                "type": "tool_call",
            }]),
            AIMessage(content="", tool_calls=[{
                "name": "server.info", "args": {}, "id": "info-call-1", "type": "tool_call",
            }]),
            AIMessage(content="The current server status is active."),
        ])

        result = await ModelDrivenAgent(registry, model).run(
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
            prompt="Check the selected server.",
            allowed_tools={"investigation.plan", "server.info"},
            allowed_target_ids={"srv_stage1"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            run_id="run-plan-1",
            trace_id="trace-plan-1",
        )

        self.assertEqual(result.status, "succeeded")
        self.assertEqual(result.tool_calls, 2)
        self.assertEqual([method for method, _params, _timeout in host.calls], [
            "host.investigation.plan.record", "host.tool.execute",
        ])
        plan_request = host.calls[0][1]["plan"]
        self.assertEqual(plan_request["taskId"], "task-durable-1")
        self.assertEqual(plan_request["steps"][0]["attempts"], 0)
        self.assertEqual(plan_request["steps"][0]["allowedTools"], ["server.info"])
        self.assertEqual(plan_request["steps"][0]["status"], "pending")
        tool_request = host.calls[1][1]
        self.assertEqual(tool_request["planId"], plan_request["id"])
        self.assertEqual(tool_request["planStepId"], "inspect-server")
        self.assertEqual(tool_request["callId"], result.tool_results[1]["call_id"])
        self.assertTrue(tool_request["callId"])
        self.assertEqual(tool_request["traceId"], "trace-plan-1")

    async def test_plan_rejects_tools_not_registered_by_the_host(self) -> None:
        host = FakePlanHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        context = ExecutionContext(
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
            allowed_tools=frozenset({"investigation.plan"}),
            allowed_target_ids=frozenset(),
            approved_actions={},
            host_task_id="task-durable-1",
        )

        with self.assertRaises(ToolCallError) as raised:
            await registry.execute(
                "investigation.plan",
                {"steps": [{
                    "id": "bad-step",
                    "ordinal": 0,
                    "kind": "evidence",
                    "title": "Unknown tool",
                    "purpose": "Do not allow tools that the host has not registered.",
                    "allowedTools": ["arbitrary.shell"],
                    "successCriteria": ["Rejected before host RPC."],
                    "requiresApproval": False,
                }]},
                context,
                call_id="plan-call-invalid",
            )

        self.assertEqual(raised.exception.code, "unknown_plan_tool")
        self.assertEqual(host.calls, [])

    async def test_regular_host_tools_forward_the_current_plan_and_evidence_binding(self) -> None:
        host = FakeHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        context = ExecutionContext(
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
            allowed_tools=frozenset({"server.info"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-current-1",
            plan_step_id="step-observe-1",
            evidence_ids=("ev-1", "ev-2"),
            run_id="run-1",
            trace_id="trace-1",
            context_target_id="srv_stage1",
        )

        await registry.execute("server.info", {}, context, call_id="observe-1")

        self.assertEqual(len(host.calls), 1)
        method, request, _timeout = host.calls[0]
        self.assertEqual(method, "host.tool.execute")
        self.assertEqual(request["planId"], "plan-current-1")
        self.assertEqual(request["planStepId"], "step-observe-1")
        self.assertEqual(request["evidenceIds"], ["ev-1", "ev-2"])
        self.assertNotIn("approvalId", request)

    async def test_structured_write_tools_forward_exact_host_approval_tickets(self) -> None:
        host = FakeHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        cases = {
            "docker.restart": {"container": "api", "timeoutSeconds": 20},
            "systemd.restart": {"service": "api.service", "timeoutSeconds": 20},
            # The tool schema admits only `auto`: the target's package manager is
            # detected inside the SSH operation, not chosen by the model.
            "package.install": {"manager": "auto", "package": "nginx", "timeoutSeconds": 25},
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
        self.assertEqual(
            request["input"],
            {**arguments, "command": "set -e\n" + arguments["command"]},
        )
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

    async def test_server_exec_runs_from_a_conversation_with_its_approval(self) -> None:
        # A conversation has no task or plan. The exact per-call approval is what
        # authorizes it; the host still re-checks target, ticket and denylist.
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
            context_target_id="srv_stage1",
        )

        await registry.execute("server.exec", arguments, context, call_id="call-1")
        request = host.calls[0][1]
        self.assertEqual(request["toolName"], "server.exec")
        self.assertEqual(request["approvalId"], "apr-host-ticket")
        for field in ("taskId", "planId", "planStepId", "evidenceIds"):
            self.assertNotIn(field, request, "a conversation must not fabricate a task binding")
        self.assertEqual(request["target"]["serverId"], "srv_stage1")


class FakeFilesystemHost:
    def __init__(self, outputs: dict[str, dict[str, Any]] | None = None) -> None:
        self.calls: list[tuple[str, dict[str, Any], float]] = []
        self.outputs = outputs or {}

    async def request(self, method: str, params: dict[str, Any], *, timeout_seconds: float) -> dict[str, Any]:
        self.calls.append((method, params, timeout_seconds))
        tool_name = params.get("toolName")
        output = self.outputs.get(tool_name, {
            "path": "/etc/app.conf",
            "revision": "a" * 64,
            "bytesBefore": 10,
            "bytesAfter": 11,
            "lineDelta": 0,
        })
        return {"status": "success", "output": output}


class HostFilesystemToolTests(unittest.IsolatedAsyncioTestCase):
    def make_context(
        self,
        *,
        allowed_tools: frozenset[str] = FILESYSTEM_ACTION_TOOLS,
        approved_actions: dict[str, str] | None = None,
        with_plan: bool = True,
    ) -> ExecutionContext:
        return ExecutionContext(
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
            allowed_tools=allowed_tools,
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions=approved_actions or {},
            context_target_id="srv_stage1",
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-current-1" if with_plan else None,
            plan_step_id="step-file-action-1" if with_plan else None,
            evidence_ids=("ev-1",) if with_plan else (),
            run_id="run-1",
            trace_id="trace-1",
        )

    async def test_filesystem_actions_require_exact_approval_and_forward_plan_binding(self) -> None:
        host = FakeFilesystemHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        tools = {tool.name: tool for tool in registry.list()}
        for name in FILESYSTEM_ACTION_TOOLS:
            self.assertEqual(tools[name].risk_level, "write")
            self.assertTrue(tools[name].requires_approval)
        self.assertEqual(tools["filesystem.backup.list"].risk_level, "read")
        self.assertFalse(tools["filesystem.backup.list"].requires_approval)
        self.assertEqual(tools["filesystem.backup.retention"].risk_level, "read")
        self.assertFalse(tools["filesystem.backup.retention"].requires_approval)

        arguments = {
            "path": "/etc/app.conf",
            "expectedRevision": "b" * 64,
            "oldString": "listen = 80",
            "newString": "listen = 8080",
        }
        context = self.make_context()
        with self.assertRaises(ToolCallError) as pending:
            await registry.execute("filesystem.edit", arguments, context, call_id="edit-call-1")
        self.assertEqual(pending.exception.code, "approval_required")
        self.assertEqual(host.calls, [])

        fingerprint = approval_fingerprint(
            "filesystem.edit",
            "srv_stage1",
            arguments,
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
        )
        approved = self.make_context(approved_actions={fingerprint: "approved-once"})
        result = await registry.execute("filesystem.edit", arguments, approved, call_id="edit-call-1")

        self.assertEqual(result.value["revision"], "a" * 64)
        self.assertEqual(len(host.calls), 1)
        method, request, _timeout = host.calls[0]
        self.assertEqual(method, "host.tool.execute")
        self.assertEqual(request["toolName"], "filesystem.edit")
        self.assertEqual(request["input"], arguments)
        self.assertEqual(request["taskId"], "task-durable-1")
        self.assertEqual(request["planId"], "plan-current-1")
        self.assertEqual(request["planStepId"], "step-file-action-1")
        self.assertEqual(request["evidenceIds"], ["ev-1"])
        self.assertNotIn("approvalId", request)

    async def test_filesystem_action_fails_closed_without_plan_and_rejects_host_output_extras(self) -> None:
        arguments = {
            "path": "/etc/app.conf",
            "expectedRevision": "b" * 64,
            "oldString": "old",
            "newString": "new",
        }
        fingerprint = approval_fingerprint(
            "filesystem.edit",
            "srv_stage1",
            arguments,
            task_id="agent-task-1",
            tenant_id="local",
            user_id="agent-session-1",
        )
        host = FakeFilesystemHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]

        # A conversation edit has no plan; the exact approval is what admits it,
        # and no task/plan identifiers are invented for the host request.
        await registry.execute(
            "filesystem.edit",
            arguments,
            self.make_context(approved_actions={fingerprint: "approved-once"}, with_plan=False),
            call_id="edit-no-plan",
        )
        self.assertEqual(len(host.calls), 1)
        _method, request, _timeout = host.calls[0]
        for field in ("taskId", "planId", "planStepId", "evidenceIds"):
            self.assertNotIn(field, request, "a conversation must not fabricate a task binding")

        extra_output_host = FakeFilesystemHost(outputs={"filesystem.edit": {
            "path": "/etc/app.conf",
            "revision": "a" * 64,
            "bytesBefore": 3,
            "bytesAfter": 3,
            "lineDelta": 0,
            "rawContent": "must not cross the Agent boundary",
        }})
        extra_output_registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(extra_output_registry, extra_output_host)  # type: ignore[arg-type]
        with self.assertRaises(ToolCallError) as invalid_output:
            await extra_output_registry.execute(
                "filesystem.edit",
                arguments,
                self.make_context(approved_actions={fingerprint: "approved-once"}),
                call_id="edit-invalid-output",
            )
        self.assertEqual(invalid_output.exception.code, "invalid_host_response")

    async def test_backup_metadata_tools_validate_filters_and_return_bounded_rows(self) -> None:
        row = {
            "id": "backup_1",
            "serverId": "srv_stage1",
            "taskId": "task-durable-1",
            "path": "/etc/app.conf",
            "backupPath": "/etc/.app.conf.backup",
            "revision": "c" * 64,
            "bytesBackedUp": 128,
            "status": "available",
            "createdAt": "2026-10-15T00:00:00Z",
            "updatedAt": "2026-10-15T00:00:00Z",
            "restoredAt": None,
            "deletedAt": None,
        }
        host = FakeFilesystemHost(outputs={
            "filesystem.backup.list": {"backups": [row], "truncated": False},
            "filesystem.backup.retention": {
                "candidates": [{
                    "path": row["path"],
                    "backupPath": row["backupPath"],
                    "revision": row["revision"],
                    "taskId": row["taskId"],
                    "createdAt": row["createdAt"],
                    "bytesBackedUp": row["bytesBackedUp"],
                }],
                "truncated": False,
                "keptCount": 2,
                "scannedCount": 3,
            },
        })
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        context = self.make_context(allowed_tools=frozenset({
            "filesystem.backup.list", "filesystem.backup.retention",
        }))

        listed = await registry.execute(
            "filesystem.backup.list", {"limit": 8}, context, call_id="backup-list"
        )
        planned = await registry.execute(
            "filesystem.backup.retention", {"keepLatest": 2}, context, call_id="backup-retention"
        )
        self.assertEqual(listed.value["backups"][0]["id"], "backup_1")
        self.assertEqual(planned.value["candidates"][0]["backupPath"], row["backupPath"])
        self.assertTrue(all(request["taskId"] == "task-durable-1" for _method, request, _timeout in host.calls))

        with self.assertRaises(ToolCallError) as invalid_filter:
            await registry.execute("filesystem.backup.retention", {}, context, call_id="bad-retention")
        self.assertEqual(invalid_filter.exception.code, "invalid_arguments")
        self.assertEqual(len(host.calls), 2)

    async def test_desktop_backup_metadata_queries_and_retention_planning_run_in_python(self) -> None:
        host = FakeBackupMetadataHost()
        registry = ToolRegistry(MemoryAuditSink())
        direct_names = register_desktop_backup_metadata_tools(registry, host)  # type: ignore[arg-type]
        self.assertEqual(direct_names, {"filesystem.backup.list", "filesystem.backup.retention"})
        context = self.make_context(allowed_tools=direct_names)

        listed = await registry.execute(
            "filesystem.backup.list", {"status": "available", "limit": 8}, context,
            call_id="python-backup-list",
        )
        retained = await registry.execute(
            "filesystem.backup.retention",
            {"pathPrefix": "/etc/", "keepLatest": 1, "olderThanDays": 30},
            context,
            call_id="python-backup-retention",
        )

        self.assertEqual(listed.value["backups"][0]["taskId"], "task-durable-1")
        self.assertEqual(retained.value, {
            "candidates": [{
                "path": "/etc/app.conf",
                "backupPath": "/etc/.yukinal-backup-old",
                "revision": "b" * 64,
                "taskId": "task-owner-other",
                "createdAt": "2020-01-01T00:00:00Z",
                "bytesBackedUp": 10,
            }],
            "truncated": False,
            "keptCount": 2,
            "scannedCount": 3,
        })
        self.assertEqual(
            [method for method, _params, _timeout in host.calls],
            [
                "host.filesystem.backup.list_query",
                "host.filesystem.backup.retention_scan",
            ],
        )
        tool_calls = [params["toolCall"] for _method, params, _timeout in host.calls]
        self.assertTrue(all(call["taskId"] == "task-durable-1" for call in tool_calls))
        self.assertTrue(all(call["planId"] == "plan-current-1" for call in tool_calls))
        self.assertTrue(all(call["evidenceIds"] == ["ev-1"] for call in tool_calls))
        self.assertEqual(tool_calls[0]["input"], {"status": "available", "limit": 8})
        self.assertEqual(tool_calls[0]["toolName"], "filesystem.backup.list")
        self.assertEqual(tool_calls[1]["toolName"], "filesystem.backup.retention")

        host.retention_rows = [
            {
                "id": f"backup_{index:02d}",
                "path": f"/etc/app-{index:02d}.conf",
                "backupPath": f"/etc/.yukinal-backup-{index:02d}",
                "revision": "e" * 64,
                "taskId": "task-durable-1",
                "createdAt": "2020-01-01T00:00:00Z",
                "bytesBackedUp": 10,
            }
            for index in range(40)
        ]
        capped = await registry.execute(
            "filesystem.backup.retention",
            {"olderThanDays": 30},
            context,
            call_id="python-backup-retention-capped",
        )
        self.assertEqual(len(capped.value["candidates"]), 32)
        self.assertTrue(capped.value["truncated"])
        self.assertEqual(capped.value["keptCount"], 0)
        self.assertEqual(capped.value["scannedCount"], 40)


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
