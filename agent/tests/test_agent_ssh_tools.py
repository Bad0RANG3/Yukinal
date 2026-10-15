from __future__ import annotations

import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, patch

from yukinal_agent import AgentWorkflow, ExecutionContext, MemoryAuditSink, ToolCallError, ToolRegistry
from yukinal_agent.ssh_tools import _parse_service_inventory, register_desktop_ssh_tools, register_ssh_tools


class FakeDesktopHost:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any]]] = []

    async def request(
        self, method: str, params: dict[str, Any], *, timeout_seconds: float
    ) -> dict[str, Any]:
        self.calls.append((method, params))
        return {
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

    def server_logs(self, *arguments: Any) -> Any:
        return self._result("server_logs", *arguments)

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
        return self._result("docker_ps", *arguments)

    def docker_logs(self, *arguments: Any) -> Any:
        return self._result("docker_logs", *arguments)

    def docker_inspect(self, *arguments: Any) -> Any:
        return self._result("docker_inspect", *arguments)

    def systemd_inspect(self, *arguments: Any) -> Any:
        return self._result("systemd_inspect", *arguments)

    def package_inspect(self, *arguments: Any) -> Any:
        return self._result("package_inspect", *arguments)


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
            "server.logs", "server.services", "docker.ps", "docker.logs", "docker.inspect",
            "systemd.inspect", "package.inspect",
        })
        FakeDesktopSshProcess.instances.clear()
        FakeDesktopSshClient.calls.clear()

        cases = [
            ("ssh.service_status", {"service": "nginx.service"}, "service_status", ("srv_stage1", "nginx.service", 10)),
            ("server.logs", {"sinceSeconds": 60, "unit": "nginx.service"}, "server_logs", ("srv_stage1", 60, "nginx.service")),
            ("server.services", {"name": "api.service", "state": "running"}, "service_inventory", ("srv_stage1",)),
            ("docker.ps", {"all": True}, "docker_ps", ("srv_stage1", True)),
            ("docker.logs", {"container": "api", "tail": 50}, "docker_logs", ("srv_stage1", "api", 50)),
            ("docker.inspect", {"container": "api"}, "docker_inspect", ("srv_stage1", "api")),
            ("systemd.inspect", {"service": "nginx.service"}, "systemd_inspect", ("srv_stage1", "nginx.service")),
            ("package.inspect", {"manager": "apt", "package": "nginx"}, "package_inspect", ("srv_stage1", "apt", "nginx")),
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
        )

        with (
            patch("yukinal_agent.ssh_tools.SshServiceProcess", FakeDesktopSshProcess),
            patch("yukinal_agent.ssh_tools.SshServiceClient", FakeDesktopSshClient),
        ):
            for tool_name, arguments, operation, expected_arguments in cases:
                result = await registry.execute(tool_name, arguments, context)
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
                else:
                    self.assertEqual(result.value["operation"], operation)
                    self.assertEqual(result.value["target_id"], "srv_stage1")

        self.assertEqual(
            FakeDesktopSshClient.calls,
            [(operation, expected_arguments) for _, _, operation, expected_arguments in cases],
        )
        self.assertEqual(
            host.calls,
            [("host.ssh.service_target", {"serverId": "srv_stage1"})] * len(cases),
        )
        self.assertEqual(len(FakeDesktopSshProcess.instances), len(cases))
        self.assertTrue(all(process.started and process.stopped for process in FakeDesktopSshProcess.instances))

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
