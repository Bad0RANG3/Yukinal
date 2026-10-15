from __future__ import annotations

import hashlib
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, patch

from yukinal_agent import AgentWorkflow, ExecutionContext, MemoryAuditSink, ToolCallError, ToolRegistry
from yukinal_agent.tools import approval_fingerprint
from yukinal_agent.ssh_tools import (
    _parse_docker_inspect,
    _parse_docker_logs,
    _parse_docker_ps,
    _parse_package_inspect,
    _parse_server_logs,
    _parse_service_inventory,
    _parse_systemd_inspect,
    register_desktop_ssh_tools,
    register_ssh_tools,
)
from yukinal_ssh.client import _SYSTEM_SNAPSHOT_COMMAND_IDS
from yukinal_agent.system_snapshot import normalize_system_snapshot

_SNAPSHOT_OUTPUTS = {
    command_id: (0, f"{command_id}\n") for command_id in _SYSTEM_SNAPSHOT_COMMAND_IDS
}
_SNAPSHOT_VALUE = {
    "id": "snapshot-1",
    "serverId": "srv_stage1",
    "os": {"hostname": "stage.example.test"},
}


class FakeDesktopHost:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any]]] = []

    async def request(
        self, method: str, params: dict[str, Any], *, timeout_seconds: float
    ) -> dict[str, Any]:
        self.calls.append((method, params))
        if method == "host.investigation.plan.check":
            return {
                "status": "allowed",
                "planId": "plan-1",
                "stepId": "step-1",
                "stepKind": "action",
                "evidenceIds": ["evidence-1"],
                "requiresApproval": True,
            }
        if method == "host.tool.execute" and params.get("toolName") == "server.info":
            if params.get("target") != {
                "host": "remote", "serverId": "srv_stage1", "environment": "staging"
            }:
                raise AssertionError(f"server.info host request lost the selected target: {params!r}")
            tool_input = params.get("input")
            if not isinstance(tool_input, dict) or set(tool_input) != {"snapshotData"}:
                raise AssertionError(f"server.info host input was invalid: {tool_input!r}")
            return {"status": "success", "output": _SNAPSHOT_VALUE}
        payload = {
            "target": {
                "target_id": params["serverId"],
                "host": "stage.example.test",
                "port": 22,
                "username": "ops",
                "host_key_fingerprint": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "auth": {"type": "agent"},
            },
            "credentials": {"SSH_AUTH_SOCK": "/tmp/ssh-agent.sock"},
        }
        if method == "host.ssh.filesystem_read_target":
            payload["filesystemRead"] = {
                "path": params["path"],
                "maxBytes": params.get("maxBytes") or 131072,
            }
        if method == "host.ssh.filesystem_write_target":
            payload["filesystemWrite"] = {
                "path": params["path"],
                "bytesWritten": len(params["content"].encode("utf-8")),
            }
        return payload


class FakeDesktopSshProcess:
    instances: list[FakeDesktopSshProcess] = []

    def __init__(self) -> None:
        self.started = False
        self.stopped = False
        self.instances.append(self)

    @classmethod
    def for_desktop_target(
        cls, _binary: Path, _payload: object
    ) -> tuple[FakeDesktopSshProcess, str, str]:
        return cls(), "x" * 40, "http://127.0.0.1:54321"

    async def start(self) -> None:
        self.started = True

    async def stop(self) -> None:
        self.stopped = True


class FakeDesktopSshClient:
    calls: list[tuple[str, tuple[Any, ...]]] = []

    def __init__(self, _token: str, *, base_url: str) -> None:
        self.base_url = base_url

    def _result(self, operation: str, *arguments: Any) -> Any:
        self.calls.append((operation, arguments))
        return SimpleNamespace(
            target_id=arguments[0],
            operation=operation,
            exit_code=0,
            stdout="{}",
            stderr="",
            duration_ms=3,
            output_truncated=False,
        )

    def service_status(self, *arguments: Any) -> Any:
        return self._result("service_status", *arguments)

    def system_snapshot(self, *arguments: Any) -> dict[str, tuple[int, str]]:
        self.calls.append(("system_snapshot", arguments))
        return _SNAPSHOT_OUTPUTS

    def server_logs(self, *arguments: Any) -> Any:
        self.calls.append(("server_logs", arguments))
        return SimpleNamespace(
            target_id=arguments[0],
            operation="server_logs",
            exit_code=0,
            stdout=(
                "2026-01-02T03:04:05Z host app[12]: ERROR failed to start\n"
                "2026-01-02T03:04:06Z host app[12]: warning retrying\n"
                "__YUKINAL_SOURCE__=journalctl\n"
            ),
            stderr="",
            duration_ms=3,
            output_truncated=False,
        )

    def service_inventory(self, *arguments: Any) -> Any:
        self.calls.append(("service_inventory", arguments))
        return SimpleNamespace(
            target_id=arguments[0],
            operation="service_inventory",
            exit_code=0,
            stdout=(
                "api.service loaded active running API service\n"
                "stopped.service loaded inactive dead Stopped service\n"
                "__YUKINAL_SOURCE__=systemd\n"
            ),
            stderr="",
            duration_ms=3,
            output_truncated=False,
        )

    def docker_ps(self, *arguments: Any) -> Any:
        self.calls.append(("docker_ps", arguments))
        return SimpleNamespace(
            target_id=arguments[0], operation="docker_ps", exit_code=0,
            stdout='{"Names":"api,api-old","Image":"nginx:1.27","State":"running","Status":"Up 3 hours"}\n',
            stderr="", duration_ms=3, output_truncated=False,
        )

    def docker_logs(self, *arguments: Any) -> Any:
        self.calls.append(("docker_logs", arguments))
        return SimpleNamespace(
            target_id=arguments[0], operation="docker_logs", exit_code=0,
            stdout="2026-01-02T03:04:05Z app started\n2026-01-02T03:04:06Z ready\n",
            stderr="", duration_ms=3, output_truncated=False,
        )

    def docker_inspect(self, *arguments: Any) -> Any:
        self.calls.append(("docker_inspect", arguments))
        return SimpleNamespace(
            target_id=arguments[0], operation="docker_inspect", exit_code=0,
            stdout=(
                '{"Id":"abc123","Name":"/api","Config":{"Image":"nginx:1.27"},'
                '"State":{"Status":"running","StartedAt":"2026-01-01T00:00:00Z",'
                '"Health":{"Status":"healthy"}},"RestartCount":2}\n'
            ),
            stderr="", duration_ms=3, output_truncated=False,
        )

    def systemd_inspect(self, *arguments: Any) -> Any:
        self.calls.append(("systemd_inspect", arguments))
        return SimpleNamespace(
            target_id=arguments[0], operation="systemd_inspect", exit_code=0,
            stdout="loaded\nactive\nrunning\nNginx web server\n",
            stderr="", duration_ms=3, output_truncated=False,
        )

    def package_inspect(self, *arguments: Any) -> Any:
        self.calls.append(("package_inspect", arguments))
        return SimpleNamespace(
            target_id=arguments[0], operation="package_inspect", exit_code=0,
            stdout="installed\t1.27.0-1\n", stderr="", duration_ms=3,
            output_truncated=False,
        )

    def sftp_read(self, *arguments: Any) -> bytes:
        self.calls.append(("sftp_read", arguments))
        return b"hello\x00x"

    def sftp_write(self, *arguments: Any) -> Any:
        return self._result("sftp_write", *arguments)


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
    async def test_desktop_read_tool_falls_back_when_cpp_target_resolution_is_unsupported(self) -> None:
        host = FakeDesktopHost()
        host.request = AsyncMock(side_effect=RuntimeError("unsupported proxy"))  # type: ignore[method-assign]
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        context = ExecutionContext(
            task_id="task-ssh-1",
            tenant_id="tenant-1",
            user_id="operator-1",
            allowed_tools=frozenset({"docker.ps"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            context_target_id="srv_stage1",
        )
        fallback = AsyncMock(return_value={"available": True, "containers": []})

        with patch("yukinal_agent.ssh_tools._execute_on_host", fallback):
            result = await registry.execute("docker.ps", {}, context)

        self.assertEqual(result.value, {"available": True, "containers": []})
        fallback.assert_awaited_once()

    async def test_desktop_read_tools_use_the_selected_target_and_cpp_operations(self) -> None:
        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        direct_names = register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        self.assertEqual(direct_names, {
            "server.info", "server.logs", "server.services", "docker.ps", "docker.logs",
            "docker.inspect", "systemd.inspect", "package.inspect", "filesystem.read",
            "filesystem.write",
        })
        FakeDesktopSshProcess.instances.clear()
        FakeDesktopSshClient.calls.clear()

        cases = [
            ("server.info", {}, "system_snapshot", ("srv_stage1",)),
            ("ssh.service_status", {"service": "nginx.service"}, "service_status", ("srv_stage1", "nginx.service", 10)),
            ("server.logs", {"sinceSeconds": 60, "unit": "nginx.service"}, "server_logs", ("srv_stage1", 60, "nginx.service")),
            ("server.services", {"name": "api.service", "state": "running"}, "service_inventory", ("srv_stage1",)),
            ("docker.ps", {"all": True}, "docker_ps", ("srv_stage1", True)),
            ("docker.logs", {"container": "api", "tail": 50}, "docker_logs", ("srv_stage1", "api", 50)),
            ("docker.inspect", {"container": "api"}, "docker_inspect", ("srv_stage1", "api")),
            ("systemd.inspect", {"service": "nginx.service"}, "systemd_inspect", ("srv_stage1", "nginx.service")),
            ("package.inspect", {"manager": "apt", "package": "nginx"}, "package_inspect", ("srv_stage1", "apt", "nginx")),
            ("filesystem.read", {"path": "/srv/app/data.bin", "maxBytes": 5}, "filesystem_read", ("srv_stage1", "/srv/app/data.bin", 5)),
        ]
        context = ExecutionContext(
            task_id="task-ssh-1",
            tenant_id="tenant-1",
            user_id="operator-1",
            allowed_tools=frozenset(case[0] for case in cases),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            context_target_id="srv_stage1",
            call_id="call-desktop-ssh-test",
            trace_id="trace-desktop-ssh-test",
        )

        with (
            patch("yukinal_agent.ssh_tools.SshServiceProcess", FakeDesktopSshProcess),
            patch("yukinal_agent.ssh_tools.SshServiceClient", FakeDesktopSshClient),
        ):
            for tool_name, arguments, operation, expected_arguments in cases:
                result = await registry.execute(
                    tool_name,
                    arguments,
                    context,
                    call_id="call-server-info-test" if tool_name == "server.info" else None,
                )
                if tool_name == "server.services":
                    self.assertEqual(result.value, {
                        "source": "systemd",
                        "services": [{
                            "name": "api.service",
                            "state": "running",
                            "status": "active/running",
                            "description": "API service",
                        }],
                    })
                elif tool_name == "filesystem.read":
                    self.assertEqual(result.value, {
                        "path": "/srv/app/data.bin",
                        "content": "hello",
                        "truncated": True,
                        "revision": hashlib.sha256(b"hello").hexdigest(),
                    })
                elif tool_name == "server.logs":
                    self.assertEqual(result.value, {
                        "source": "journalctl",
                        "lines": [
                            {"text": "2026-01-02T03:04:05Z host app[12]: ERROR failed to start", "level": "error"},
                            {"text": "2026-01-02T03:04:06Z host app[12]: warning retrying", "level": "warning"},
                        ],
                    })
                elif tool_name == "docker.ps":
                    self.assertEqual(result.value, {
                        "available": True,
                        "containers": [{
                            "name": "api",
                            "image": "nginx:1.27",
                            "state": "running",
                            "status": "Up 3 hours",
                            "restartCount": 0,
                        }],
                    })
                elif tool_name == "docker.logs":
                    self.assertEqual(result.value, {
                        "container": "api",
                        "lines": ["2026-01-02T03:04:05Z app started", "2026-01-02T03:04:06Z ready"],
                        "truncated": False,
                    })
                elif tool_name == "docker.inspect":
                    self.assertEqual(result.value, {
                        "id": "abc123",
                        "name": "api",
                        "image": "nginx:1.27",
                        "state": "running",
                        "status": "running",
                        "restartCount": 2,
                        "startedAt": "2026-01-01T00:00:00Z",
                        "health": "healthy",
                    })
                elif tool_name == "systemd.inspect":
                    self.assertEqual(result.value, {
                        "service": "nginx.service",
                        "loadState": "loaded",
                        "activeState": "active",
                        "subState": "running",
                        "description": "Nginx web server",
                    })
                elif tool_name == "package.inspect":
                    self.assertEqual(result.value, {
                        "manager": "apt",
                        "package": "nginx",
                        "installed": True,
                        "version": "1.27.0-1",
                    })
                elif tool_name == "server.info":
                    self.assertEqual(result.value, _SNAPSHOT_VALUE)
                else:
                    self.assertEqual(result.value["operation"], operation)
                    self.assertEqual(result.value["target_id"], "srv_stage1")

        self.assertEqual(
            FakeDesktopSshClient.calls,
            [("sftp_read" if operation == "filesystem_read" else operation, expected_arguments)
             for _, _, operation, expected_arguments in cases],
        )
        expected_host_calls: list[tuple[str, dict[str, Any]]] = []
        for tool_name, _, operation, _ in cases:
            if operation == "filesystem_read":
                expected_host_calls.append((
                    "host.ssh.filesystem_read_target",
                    {"serverId": "srv_stage1", "path": "/srv/app/data.bin", "maxBytes": 5},
                ))
            else:
                expected_host_calls.append((
                    "host.ssh.service_target", {"serverId": "srv_stage1"}
                ))
                if tool_name == "server.info":
                    expected_host_calls.append((
                        "host.tool.execute",
                        {
                            "callId": "call-server-info-test",
                            "traceId": "trace-desktop-ssh-test",
                            "toolName": "server.info",
                            "input": {
                                "snapshotData": normalize_system_snapshot(_SNAPSHOT_OUTPUTS),
                            },
                            "target": {
                                "host": "remote",
                                "serverId": "srv_stage1",
                                "environment": "staging",
                            },
                        },
                    ))
        self.assertEqual(host.calls, expected_host_calls)
        self.assertEqual(len(FakeDesktopSshProcess.instances), len(cases))
        self.assertTrue(all(process.started and process.stopped for process in FakeDesktopSshProcess.instances))

    async def test_filesystem_write_uses_cpp_after_plan_check_and_replays_exact_success(self) -> None:
        import os
        import tempfile

        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        arguments = {"path": "/etc/app.conf", "content": "hello=world\n"}
        fingerprint = approval_fingerprint(
            "filesystem.write",
            "srv_stage1",
            arguments,
            task_id="task-ssh-1",
            tenant_id="tenant-1",
            user_id="operator-1",
        )
        context = ExecutionContext(
            task_id="task-ssh-1",
            tenant_id="tenant-1",
            user_id="operator-1",
            allowed_tools=frozenset({"filesystem.write"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-write"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-1",
            plan_step_id="step-1",
            evidence_ids=("evidence-1",),
            run_id="run-1",
            trace_id="trace-1",
            context_target_id="srv_stage1",
        )
        FakeDesktopSshClient.calls.clear()
        FakeDesktopSshProcess.instances.clear()

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.dict(os.environ, {"YUKINAL_DATA_DIR": temporary}),
            patch("yukinal_agent.ssh_tools.SshServiceProcess", FakeDesktopSshProcess),
            patch("yukinal_agent.ssh_tools.SshServiceClient", FakeDesktopSshClient),
        ):
            first = await registry.execute("filesystem.write", arguments, context, call_id="call-write-1")
            second = await registry.execute("filesystem.write", arguments, context, call_id="call-write-1")

        expected = {"path": "/etc/app.conf", "bytesWritten": len(arguments["content"].encode())}
        self.assertEqual(first.value, expected)
        self.assertEqual(second.value, expected)
        self.assertEqual(
            FakeDesktopSshClient.calls,
            [("sftp_write", ("srv_stage1", "/etc/app.conf", b"hello=world\n"))],
        )
        self.assertEqual(
            [method for method, _params in host.calls],
            [
                "host.investigation.plan.check",
                "host.ssh.filesystem_write_target",
                "host.investigation.plan.check",
            ],
        )
        self.assertEqual(host.calls[1][1], {
            "serverId": "srv_stage1",
            "path": "/etc/app.conf",
            "content": "hello=world\n",
            "taskId": "task-durable-1",
            "planId": "plan-1",
            "planStepId": "step-1",
            "evidenceIds": ["evidence-1"],
            "target": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        })
        self.assertTrue(FakeDesktopSshProcess.instances[0].started)
        self.assertTrue(FakeDesktopSshProcess.instances[0].stopped)

    async def test_service_inventory_parser_handles_docker_and_unavailable_sources(self) -> None:
        docker = _parse_service_inventory(
            "api\trunning\tUp 3 days\tnginx:latest\n"
            "stopped\texited\tExited (0) 1 day ago\talpine\n"
            "__YUKINAL_SOURCE__=docker\n",
            {},
        )
        self.assertEqual(docker, {
            "source": "docker",
            "services": [
                {"name": "api", "state": "running", "status": "Up 3 days", "description": "nginx:latest"},
                {"name": "stopped", "state": "stopped", "status": "Exited (0) 1 day ago", "description": "alpine"},
            ],
        })
        unavailable = _parse_service_inventory("__YUKINAL_SOURCE__=unavailable\n", {})
        self.assertEqual(unavailable["source"], "unavailable")
        self.assertEqual(unavailable["services"], [])
        self.assertEqual(unavailable["message"], "未检测到可读取的 systemd 或 Docker 服务管理器")

    async def test_server_log_parser_preserves_levels_filters_and_unavailable_shape(self) -> None:
        parsed = _parse_server_logs(
            "panic: service failed\nwarning: retrying\nplain info\n__YUKINAL_SOURCE__=journalctl\n",
            {"sinceSeconds": 60},
        )
        self.assertEqual(parsed, {
            "source": "journalctl",
            "lines": [
                {"text": "panic: service failed", "level": "error"},
                {"text": "warning: retrying", "level": "warning"},
                {"text": "plain info", "level": "info"},
            ],
        })
        unavailable = _parse_server_logs("__YUKINAL_SOURCE__=unavailable\n", {})
        self.assertEqual(unavailable, {
            "source": "unavailable",
            "lines": [],
            "message": "未检测到可读取的 journalctl 或系统日志文件",
        })
        with self.assertRaises(ToolCallError) as unsupported:
            _parse_server_logs(
                "plain log\n__YUKINAL_SOURCE__=syslog\n", {"unit": "nginx.service"}
            )
        self.assertEqual(unsupported.exception.code, "unsupported")

    async def test_structured_read_parsers_match_existing_tool_result_shapes(self) -> None:
        self.assertEqual(
            _parse_docker_ps(
                '{"Names":"web,old","Image":"nginx:1.27","State":"running","Status":"Up"}\n'
                '{"Names":"bad","Image":"","State":"running","Status":"Up"}\nnot-json\n'
            ),
            [{
                "name": "web", "image": "nginx:1.27", "state": "running",
                "status": "Up", "restartCount": 0,
            }],
        )
        self.assertEqual(
            _parse_docker_logs("a\nb\nc\n", "api", 2),
            {"container": "api", "lines": ["a", "b"], "truncated": True},
        )
        inspect = _parse_docker_inspect(
            '{"Id":"abc","Name":"/api","Config":{"Image":"nginx"},'
            '"State":{"Status":"running","StartedAt":"","Health":{"Status":"healthy"}}}\n'
        )
        self.assertEqual(inspect, {
            "id": "abc", "name": "api", "image": "nginx", "state": "running",
            "status": "running", "restartCount": 0, "health": "healthy",
        })
        self.assertEqual(
            _parse_systemd_inspect("loaded\ninactive\ndead\n\n", "nginx.service"),
            {"service": "nginx.service", "loadState": "loaded", "activeState": "inactive", "subState": "dead"},
        )
        self.assertEqual(
            _parse_package_inspect("absent\n", "apt", "nginx"),
            {"manager": "apt", "package": "nginx", "installed": False},
        )
        with self.assertRaises(ToolCallError) as unsupported:
            _parse_package_inspect("unsupported\n", "dnf", "nginx")
        self.assertEqual(unsupported.exception.code, "unsupported")

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
