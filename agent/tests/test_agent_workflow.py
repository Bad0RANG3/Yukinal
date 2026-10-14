from __future__ import annotations

import asyncio
import os
import socket
import sys
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from mcp import Client
from mcp.server import MCPServer
import uvicorn

from yukinal_agent import (
    AgentWorkflow,
    MCPClientManager,
    MCPManagerError,
    MCPServerConfig,
    MemoryAuditSink,
    TaskBudget,
    ToolRegistry,
    ToolSpec,
)
from yukinal_agent.tools import ExecutionContext


class WorkflowTests(unittest.IsolatedAsyncioTestCase):
    async def test_in_process_mcp_tool_runs_through_registry_and_langgraph(self) -> None:
        server = MCPServer("ops-test")

        @server.tool()
        def service_status(service: str) -> dict[str, str]:
            return {"service": service, "state": "active"}

        @server.tool()
        def restart_service(service: str) -> dict[str, str]:
            return {"service": service, "state": "restarted"}

        audit = MemoryAuditSink()
        registry = ToolRegistry(audit)
        async with MCPClientManager() as manager:
            await manager.attach_client("ops", Client(server), read_only_tools=frozenset({"service_status"}))
            names = await manager.register_tools("ops", registry)
            self.assertEqual(names, ("mcp.ops.service_status", "mcp.ops.restart_service"))
            self.assertEqual(registry.get("mcp.ops.service_status").risk_level, "read")
            self.assertEqual(registry.get("mcp.ops.restart_service").risk_level, "write")

            workflow = AgentWorkflow(registry)
            result = await workflow.run(
                task_id="mcp-task-1",
                tenant_id="tenant-1",
                user_id="user-1",
                allowed_tools={"mcp.ops.service_status"},
                allowed_target_ids=set(),
                tool_calls=[{
                    "name": "mcp.ops.service_status",
                    "arguments": {"service": "nginx"},
                }],
            )
            self.assertEqual(result.status, "succeeded")
            self.assertEqual(result.results[0]["result"]["structured_content"], {
                "service": "nginx", "state": "active"
            })

            denied = await workflow.run(
                task_id="mcp-task-2",
                tenant_id="tenant-1",
                user_id="user-1",
                allowed_tools={"mcp.ops.restart_service"},
                allowed_target_ids=set(),
                tool_calls=[{
                    "name": "mcp.ops.restart_service",
                    "arguments": {"service": "nginx"},
                }],
            )
            self.assertEqual(denied.status, "awaiting_approval")
            self.assertEqual(denied.error_code, "approval_required")

        self.assertEqual([event.status for event in audit.events], ["started", "succeeded", "started", "awaiting_approval"])

    async def test_call_count_limit_and_task_deadline_are_enforced(self) -> None:
        audit = MemoryAuditSink()
        registry = ToolRegistry(audit)
        cancelled = asyncio.Event()

        async def slow(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            try:
                await asyncio.sleep(1)
            finally:
                cancelled.set()
            return "done"

        registry.register(
            ToolSpec(
                name="ops.slow",
                description="Slow test tool.",
                input_schema={"type": "object", "properties": {}, "additionalProperties": False},
                risk_level="read",
                requires_approval=False,
                handler=slow,
                target_id_argument=None,
            )
        )
        workflow = AgentWorkflow(
            registry,
            TaskBudget(max_tool_calls=1, max_total_seconds=0.05, max_output_bytes=1024, max_tool_seconds=1),
        )
        request = {"name": "ops.slow", "arguments": {}}
        over_limit = await workflow.run(
            task_id="budget-1",
            tenant_id="tenant-1",
            user_id="user-1",
            allowed_tools={"ops.slow"},
            allowed_target_ids=set(),
            tool_calls=[request, request],
        )
        self.assertEqual(over_limit.error_code, "tool_call_limit_exceeded")

        timed_out = await workflow.run(
            task_id="budget-2",
            tenant_id="tenant-1",
            user_id="user-1",
            allowed_tools={"ops.slow"},
            allowed_target_ids=set(),
            tool_calls=[request],
        )
        self.assertEqual(timed_out.status, "timed_out")
        self.assertTrue(cancelled.is_set())
        self.assertEqual(audit.events[-1].status, "cancelled")

    async def test_stdio_mcp_server_connects_and_closes_as_a_managed_process(self) -> None:
        registry = ToolRegistry(MemoryAuditSink())
        test_server = Path(__file__).with_name("mcp_test_server.py")
        config = MCPServerConfig(
            name="stdio",
            transport="stdio",
            command=sys.executable,
            args=(str(test_server),),
            read_only_tools=frozenset({"ping"}),
        )
        async with MCPClientManager() as manager:
            await manager.connect(config)
            registered = await manager.register_tools("stdio", registry)
            self.assertEqual(registered, ("mcp.stdio.ping",))
            result = await AgentWorkflow(registry).run(
                task_id="stdio-task-1",
                tenant_id="tenant-1",
                user_id="user-1",
                allowed_tools={"mcp.stdio.ping"},
                allowed_target_ids=set(),
                tool_calls=[{"name": "mcp.stdio.ping", "arguments": {}}],
            )
            self.assertEqual(result.status, "succeeded")
            self.assertEqual(result.results[0]["result"]["structured_content"], {"state": "ready"})

    async def test_streamable_http_uses_environment_url_and_header_references(self) -> None:
        server = MCPServer("http-test")

        @server.tool()
        def ping() -> dict[str, str]:
            return {"state": "ready"}

        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
        listener.close()

        app = server.streamable_http_app()

        async def require_token(scope: Any, receive: Any, send: Any) -> None:
            if scope["type"] == "http":
                headers = dict(scope.get("headers", []))
                if headers.get(b"authorization") != b"Bearer test-mcp-token":
                    await send({
                        "type": "http.response.start",
                        "status": 401,
                        "headers": [(b"content-length", b"0")],
                    })
                    await send({"type": "http.response.body", "body": b""})
                    return
            await app(scope, receive, send)

        http_server = uvicorn.Server(
            uvicorn.Config(require_token, host="127.0.0.1", port=port, log_level="critical", access_log=False)
        )
        server_task = asyncio.create_task(http_server.serve())
        try:
            for _ in range(200):
                if http_server.started:
                    break
                if server_task.done():
                    raise RuntimeError("test MCP HTTP server failed to start")
                await asyncio.sleep(0.01)
            self.assertTrue(http_server.started)

            audit = MemoryAuditSink()
            registry = ToolRegistry(audit)
            with patch.dict(os.environ, {
                "YUKINAL_TEST_MCP_URL": f"http://127.0.0.1:{port}/mcp",
                "YUKINAL_TEST_MCP_TOKEN": "Bearer test-mcp-token",
            }):
                config = MCPServerConfig(
                    name="http",
                    transport="streamable_http",
                    url="${YUKINAL_TEST_MCP_URL}",
                    header_refs={"Authorization": "YUKINAL_TEST_MCP_TOKEN"},
                    read_only_tools=frozenset({"ping"}),
                )
                async with MCPClientManager() as manager:
                    await manager.connect(config)
                    self.assertEqual(await manager.register_tools("http", registry), ("mcp.http.ping",))
                    result = await AgentWorkflow(registry).run(
                        task_id="http-task-1",
                        tenant_id="tenant-1",
                        user_id="user-1",
                        allowed_tools={"mcp.http.ping"},
                        allowed_target_ids=set(),
                        tool_calls=[{"name": "mcp.http.ping", "arguments": {}}],
                    )
            self.assertEqual(result.status, "succeeded")
            self.assertEqual(result.results[0]["result"]["structured_content"], {"state": "ready"})
        finally:
            http_server.should_exit = True
            await asyncio.wait_for(server_task, timeout=5)

    async def test_mcp_endpoint_rejects_cleartext_remote_and_embedded_credentials(self) -> None:
        unsafe_urls = (
            "http://mcp.example.com/mcp",
            "https://user:secret@mcp.example.com/mcp",
            "https://mcp.example.com/mcp?token=secret",
        )
        for url in unsafe_urls:
            with self.subTest(url=url), self.assertRaises(ValueError):
                MCPServerConfig(name="unsafe", transport="streamable_http", url=url)

    async def test_mcp_connection_failure_has_a_stable_code(self) -> None:
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
        listener.close()
        async with MCPClientManager() as manager:
            with self.assertRaises(MCPManagerError) as caught:
                await manager.connect(MCPServerConfig(
                    name="offline",
                    transport="streamable_http",
                    url=f"http://127.0.0.1:{port}/mcp",
                ))
        self.assertEqual(caught.exception.code, "mcp_connect_failed")

    async def test_payload_and_identity_validation_fail_closed(self) -> None:
        registry = ToolRegistry(MemoryAuditSink())
        workflow = AgentWorkflow(registry)
        bad_identity = await workflow.run(
            task_id="bad\nidentity",
            tenant_id="tenant-1",
            user_id="user-1",
            allowed_tools=set(),
            allowed_target_ids=set(),
            tool_calls=[{"name": "missing", "arguments": {}}],
        )
        self.assertEqual(bad_identity.status, "rejected")
        self.assertEqual(bad_identity.error_code, "invalid_execution_context")


if __name__ == "__main__":
    unittest.main()
