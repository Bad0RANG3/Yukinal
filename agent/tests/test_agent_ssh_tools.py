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
    _parse_disk_usage,
    _parse_docker_inspect,
    _parse_docker_logs,
    _parse_docker_ps,
    _parse_package_inspect,
    _parse_port_inventory,
    _parse_process_inventory,
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
        if method == "host.ssh.effectful_action_finished":
            return {"status": "success", "finished": True}
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
        tool_call = params.get("toolCall")
        nested_target = tool_call.get("target") if isinstance(tool_call, dict) else None
        server_id = params.get("serverId")
        if not isinstance(server_id, str) and isinstance(nested_target, dict):
            server_id = nested_target.get("serverId")
        if not isinstance(server_id, str):
            raise AssertionError(f"SSH target request omitted its server id: {params!r}")
        payload = {
            "target": {
                "target_id": server_id,
                "host": "stage.example.test",
                "port": 22,
                "username": "ops",
                "host_key_fingerprint": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "auth": {"type": "agent"},
            },
            "credentials": {"SSH_AUTH_SOCK": "/tmp/ssh-agent.sock"},
        }
        if method == "host.ssh.effectful_action_target":
            response = {"status": "success", "targetData": payload}
            if isinstance(tool_call, dict) and tool_call.get("toolName") == "server.exec":
                payload["target"]["allow_server_exec"] = True
            return response
        if method == "host.ssh.filesystem_read_target":
            payload["filesystemRead"] = {
                "path": params["path"],
                "maxBytes": params.get("maxBytes") or 131072,
            }
        if method == "host.ssh.filesystem_list_target":
            payload["filesystemList"] = {"path": params["path"]}
        if method == "host.ssh.filesystem_write_target":
            payload["filesystemWrite"] = {
                "path": params["path"],
                "bytesWritten": len(params["content"].encode("utf-8")),
            }
        if method == "host.ssh.filesystem_backup_target":
            payload["filesystemBackup"] = {
                "path": params["path"],
                "backupPath": (
                    f"/etc/.yukinal-backup-{hashlib.sha256(params['path'].encode()).hexdigest()[:16]}-"
                    f"{'0' * 32}"
                ),
            }
        if method == "host.ssh.filesystem_restore_target":
            backup_path = (
                f"/var/.yukinal-backup-{hashlib.sha256(params['path'].encode()).hexdigest()[:16]}-"
                f"{'0' * 32}"
            )
            payload["filesystemRestore"] = {
                "path": params["path"],
                "backupPath": backup_path,
                "expectedRevision": params["expectedRevision"],
                "revision": hashlib.sha256(b"old config").hexdigest(),
                "bytesBackedUp": len(b"old config"),
            }
        if method == "host.ssh.filesystem_cleanup_target":
            raw_input = params["input"]
            items = raw_input.get("items") if isinstance(raw_input, dict) else None
            if not isinstance(items, list):
                items = [raw_input]
            bindings = []
            for item in items:
                common = {
                    "path": item["path"],
                    "backupPath": item["backupPath"],
                    "expectedRevision": item["expectedRevision"],
                }
                if item["path"] == "/opt/missing.conf":
                    bindings.append({**common, "status": "skipped", "reason": "no available host ledger record"})
                else:
                    bindings.append({
                        **common,
                        "status": "ready",
                        "revision": item["expectedRevision"],
                        "bytesBackedUp": 7,
                        "ownerTaskId": params["taskId"],
                    })
            payload["filesystemCleanup"] = {"items": bindings}
        if method == "host.investigation.filesystem_backup.register":
            return {"registered": True, "backupPath": params["backupPath"]}
        if method == "host.investigation.filesystem_backup.restored":
            return {
                "restored": True,
                "path": params["path"],
                "backupPath": params["backupPath"],
                "revision": params["revision"],
                "bytesBefore": params["bytesBefore"],
                "bytesAfter": params["bytesAfter"],
            }
        if method == "host.investigation.filesystem_backup.deleted":
            return {"deleted": True, **{
                "path": params["path"],
                "backupPath": params["backupPath"],
                "revision": params["revision"],
                "bytesDeleted": params["bytesDeleted"],
            }}
        return payload


class FakeDesktopSshProcess:
    instances: list[FakeDesktopSshProcess] = []
    payloads: list[object] = []

    def __init__(self) -> None:
        self.started = False
        self.stopped = False
        self.instances.append(self)

    @classmethod
    def for_desktop_target(
        cls, _binary: Path, _payload: object
    ) -> tuple[FakeDesktopSshProcess, str, str]:
        cls.payloads.append(_payload)
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

    def disk_usage(self, *arguments: Any) -> Any:
        self.calls.append(("disk_usage", arguments))
        return SimpleNamespace(
            target_id=arguments[0], operation="disk_usage", exit_code=0,
            stdout=("Filesystem 1024-blocks Used Available Capacity Mounted on\n"
                    "/dev/root 1000 250 750 25% /\n"),
            stderr="", duration_ms=3, output_truncated=False,
        )

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

    def process_inventory(self, *arguments: Any) -> Any:
        self.calls.append(("process_inventory", arguments))
        return SimpleNamespace(
            target_id=arguments[0],
            operation="process_inventory",
            exit_code=0,
            stdout=(
                "  912     1 www-data  38.5  2.1 Ssl  1200 nginx\n"
                "  980   912 root       1.5  0.4 S      900 php-fpm\n"
                "__YUKINAL_SOURCE__=ps\n"
            ),
            stderr="",
            duration_ms=3,
            output_truncated=False,
        )

    def port_inventory(self, *arguments: Any) -> Any:
        self.calls.append(("port_inventory", arguments))
        return SimpleNamespace(
            target_id=arguments[0],
            operation="port_inventory",
            exit_code=0,
            stdout=(
                "tcp   LISTEN 0  511  0.0.0.0:80  0.0.0.0:* users:((\"nginx\",pid=912,fd=6))\n"
                "tcp   LISTEN 0  128  127.0.0.1:5432  0.0.0.0:* users:((\"postgres\",pid=1200,fd=7))\n"
                "__YUKINAL_SOURCE__=ss\n"
            ),
            stderr="",
            duration_ms=3,
            output_truncated=False,
        )

    def sftp_list(self, *arguments: Any) -> Any:
        self.calls.append(("sftp_list", arguments))
        return [
            {"name": "nginx.conf", "type": "file", "size": 1024},
            {"name": "conf.d", "type": "directory", "size": 4096},
            {"name": "access.log", "type": "symlink", "size": 12},
        ]

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

    def docker_restart(self, *arguments: Any) -> Any:
        return self._result("docker_restart", *arguments)

    def systemd_inspect(self, *arguments: Any) -> Any:
        self.calls.append(("systemd_inspect", arguments))
        return SimpleNamespace(
            target_id=arguments[0], operation="systemd_inspect", exit_code=0,
            stdout="loaded\nactive\nrunning\nNginx web server\n",
            stderr="", duration_ms=3, output_truncated=False,
        )

    def systemd_restart(self, *arguments: Any) -> Any:
        return self._result("systemd_restart", *arguments)

    def package_install(self, *arguments: Any) -> Any:
        return self._result("package_install", *arguments)

    def server_exec(
        self,
        target_id: str,
        command: str,
        timeout_ms: int,
        max_output_bytes: int,
        workdir: str | None = None,
        env: dict[str, str] | None = None,
    ) -> Any:
        self.calls.append(
            ("server_exec", (target_id, command, timeout_ms, max_output_bytes, workdir, env))
        )
        return SimpleNamespace(
            exit_code=0,
            stdout="hello\n",
            stderr="",
            stdout_truncated=False,
            stderr_truncated=False,
            duration_ms=9,
        )

    def package_inspect(self, *arguments: Any) -> Any:
        self.calls.append(("package_inspect", arguments))
        return SimpleNamespace(
            target_id=arguments[0], operation="package_inspect", exit_code=0,
            stdout="__YUKINAL_MANAGER__=yum\ninstalled\t1.27.0-1\n", stderr="", duration_ms=3,
            output_truncated=False,
        )

    def sftp_write(self, *arguments: Any) -> Any:
        return self._result("sftp_write", *arguments)

    def sftp_stat(self, *arguments: Any) -> dict[str, Any]:
        self.calls.append(("sftp_stat", arguments))
        path = arguments[1]
        if path == "/var/app.conf":
            return {"kind": "file", "size": len(b"new config"), "modified": 200}
        if ".yukinal-backup-" in path and path.startswith("/var/"):
            return {"kind": "file", "size": len(b"old config"), "modified": 201}
        return {"kind": "file", "size": 7, "modified": 123}

    def sftp_link_count(self, *arguments: Any) -> int:
        self.calls.append(("sftp_link_count", arguments))
        return 1

    def sftp_create_exclusive(self, *arguments: Any) -> Any:
        return self._result("sftp_create_exclusive", *arguments)

    def sftp_remove_file(self, *arguments: Any, **kwargs: Any) -> Any:
        return self._result("sftp_remove_file", *arguments, kwargs.get("timeout_seconds", 10))

    def sftp_replace_guarded(self, target_id: str, path: str, content: bytes, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("sftp_replace_guarded", (target_id, path, content, kwargs)))
        return {"size": len(content), "modified": 202}

    def sftp_read(self, target_id: str, path: str, max_bytes: int, *arguments: Any) -> bytes:
        self.calls.append(("sftp_read", (target_id, path, max_bytes, *arguments)))
        if path == "/var/app.conf":
            return b"new config"
        if ".yukinal-backup-" in path and path.startswith("/var/"):
            return b"old config"
        return b"hello\x00x"


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

    def disk_usage(self, target_id: str, timeout_seconds: int) -> Any:
        self.calls.append((target_id, "disk_usage", timeout_seconds))
        return type("Result", (), {
            "target_id": target_id,
            "operation": "disk_usage",
            "exit_code": 0,
            "stdout": "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/root 1000 250 750 25% /\n",
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
    async def test_disk_usage_parser_rejects_malformed_or_unbounded_rows(self) -> None:
        self.assertEqual(
            _parse_disk_usage(
                "Filesystem 1024-blocks Used Available Capacity Mounted on\n"
                "/dev/root 1000 250 750 25% /\n"
            ),
            {
                "filesystem": "/dev/root",
                "mount": "/",
                "total_bytes": 1_024_000,
                "used_bytes": 256_000,
                "available_bytes": 768_000,
                "usage_percent": 25,
            },
        )
        for malformed in (
            "Filesystem Blocks Used Available Capacity Mounted on\n/dev/root 1000 250 750 25% /\n",
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/root -1 250 750 25% /\n",
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/root 1000 250 750 25% relative\n",
            "x" * 4097,
        ):
            with self.subTest(malformed=malformed[:30]), self.assertRaises(ToolCallError) as raised:
                _parse_disk_usage(malformed)
            self.assertEqual(raised.exception.code, "invalid_response")

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
            "server.info", "ssh.disk_usage", "server.logs", "server.services", "docker.ps", "docker.logs",
            "docker.inspect", "systemd.inspect", "package.inspect", "filesystem.read",
            "filesystem.list", "server.processes", "server.ports",
            "filesystem.write", "filesystem.edit", "filesystem.backup", "filesystem.restore",
            "filesystem.backup.cleanup", "systemd.restart", "docker.restart", "package.install",
            "server.exec",
        })
        FakeDesktopSshProcess.instances.clear()
        FakeDesktopSshClient.calls.clear()

        cases = [
            ("server.info", {}, "system_snapshot", ("srv_stage1",)),
            ("ssh.service_status", {"service": "nginx.service"}, "service_status", ("srv_stage1", "nginx.service", 10)),
            ("ssh.disk_usage", {}, "disk_usage", ("srv_stage1", 10)),
            ("server.logs", {"sinceSeconds": 60, "unit": "nginx.service"}, "server_logs", ("srv_stage1", 60, "nginx.service")),
            ("server.services", {"name": "api.service", "state": "running"}, "service_inventory", ("srv_stage1",)),
            ("docker.ps", {"all": True}, "docker_ps", ("srv_stage1", True)),
            ("docker.logs", {"container": "api", "tail": 50}, "docker_logs", ("srv_stage1", "api", 50)),
            ("docker.inspect", {"container": "api"}, "docker_inspect", ("srv_stage1", "api")),
            ("systemd.inspect", {"service": "nginx.service"}, "systemd_inspect", ("srv_stage1", "nginx.service")),
            ("package.inspect", {"manager": "auto", "package": "nginx"}, "package_inspect", ("srv_stage1", "auto", "nginx")),
            ("filesystem.read", {"path": "/srv/app/data.bin", "maxBytes": 5}, "filesystem_read", ("srv_stage1", "/srv/app/data.bin", 5)),
            ("filesystem.list", {"path": "/etc/nginx"}, "sftp_list", ("srv_stage1", "/etc/nginx")),
            ("server.processes", {}, "process_inventory", ("srv_stage1",)),
            ("server.ports", {}, "port_inventory", ("srv_stage1",)),
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
                elif tool_name == "filesystem.list":
                    self.assertEqual(result.value, {
                        "path": "/etc/nginx",
                        "entries": [
                            {"name": "nginx.conf", "type": "file", "size": 1024},
                            {"name": "conf.d", "type": "directory", "size": 4096},
                            {"name": "access.log", "type": "symlink", "size": 12},
                        ],
                        "truncated": False,
                    })
                elif tool_name == "server.processes":
                    self.assertEqual(result.value["source"], "ps")
                    self.assertEqual([item["pid"] for item in result.value["processes"]], [912, 980])
                elif tool_name == "server.ports":
                    self.assertTrue(result.value["processInfoAvailable"])
                    self.assertEqual([item["port"] for item in result.value["listeners"]], [80, 5432])
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
                        "manager": "yum",
                        "package": "nginx",
                        "installed": True,
                        "version": "1.27.0-1",
                    })
                elif tool_name == "server.info":
                    self.assertEqual(result.value, _SNAPSHOT_VALUE)
                elif tool_name == "ssh.disk_usage":
                    self.assertEqual(result.value, {
                        "target_id": "srv_stage1",
                        "filesystem": "/dev/root",
                        "mount": "/",
                        "total_bytes": 1_024_000,
                        "used_bytes": 256_000,
                        "available_bytes": 768_000,
                        "usage_percent": 25,
                        "duration_ms": 3,
                    })
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
            elif operation == "sftp_list":
                expected_host_calls.append((
                    "host.ssh.filesystem_list_target",
                    {"serverId": "srv_stage1", "path": "/etc/nginx"},
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

    async def test_desktop_systemd_restart_uses_approved_host_route_and_cpp_operation(self) -> None:
        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        arguments = {"service": "nginx.service", "timeoutSeconds": 10}
        fingerprint = approval_fingerprint(
            "systemd.restart",
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
            allowed_tools=frozenset({"systemd.restart"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-systemd"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-1",
            plan_step_id="step-1",
            evidence_ids=("evidence-1",),
            run_id="run-systemd-1",
            trace_id="trace-systemd-1",
            call_id="call-systemd-1",
            context_target_id="srv_stage1",
        )
        FakeDesktopSshClient.calls.clear()
        FakeDesktopSshProcess.instances.clear()

        with (
            patch("yukinal_agent.ssh_tools.SshServiceProcess", FakeDesktopSshProcess),
            patch("yukinal_agent.ssh_tools.SshServiceClient", FakeDesktopSshClient),
        ):
            result = await registry.execute("systemd.restart", arguments, context)

        self.assertEqual(result.value, {"service": "nginx.service", "restarted": True})
        self.assertEqual(
            FakeDesktopSshClient.calls,
            [("systemd_restart", ("srv_stage1", "nginx.service", 10))],
        )
        self.assertEqual(
            [method for method, _params in host.calls],
            ["host.ssh.effectful_action_target", "host.ssh.effectful_action_finished"],
        )
        tool_call = host.calls[0][1]["toolCall"]
        self.assertEqual(tool_call["approvalId"], "apr-approved-systemd")
        self.assertEqual(tool_call["planId"], "plan-1")
        self.assertEqual(tool_call["planStepId"], "step-1")
        self.assertEqual(host.calls[1][1]["outcome"], "succeeded")
        self.assertEqual(len(FakeDesktopSshProcess.instances), 1)
        self.assertTrue(FakeDesktopSshProcess.instances[0].started)
        self.assertTrue(FakeDesktopSshProcess.instances[0].stopped)

    async def test_desktop_docker_restart_uses_approved_host_route_and_cpp_operation(self) -> None:
        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        arguments = {"container": "api-1", "timeoutSeconds": 10}
        fingerprint = approval_fingerprint(
            "docker.restart",
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
            allowed_tools=frozenset({"docker.restart"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-docker"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-1",
            plan_step_id="step-1",
            evidence_ids=("evidence-1",),
            run_id="run-docker-1",
            trace_id="trace-docker-1",
            call_id="call-docker-1",
            context_target_id="srv_stage1",
        )
        FakeDesktopSshClient.calls.clear()
        FakeDesktopSshProcess.instances.clear()

        with (
            patch("yukinal_agent.ssh_tools.SshServiceProcess", FakeDesktopSshProcess),
            patch("yukinal_agent.ssh_tools.SshServiceClient", FakeDesktopSshClient),
        ):
            result = await registry.execute(
                "docker.restart", arguments, context, call_id=context.call_id
            )

        self.assertEqual(result.value, {"container": "api-1", "restarted": True})
        self.assertEqual(
            FakeDesktopSshClient.calls,
            [("docker_restart", ("srv_stage1", "api-1", 10))],
        )
        self.assertEqual(
            [method for method, _params in host.calls],
            ["host.ssh.effectful_action_target", "host.ssh.effectful_action_finished"],
        )
        self.assertEqual(host.calls[0][1]["toolCall"]["toolName"], "docker.restart")
        self.assertEqual(host.calls[0][1]["toolCall"]["approvalId"], "apr-approved-docker")
        self.assertEqual(host.calls[1][1]["outcome"], "succeeded")
        self.assertTrue(FakeDesktopSshProcess.instances[0].started)
        self.assertTrue(FakeDesktopSshProcess.instances[0].stopped)

    async def test_desktop_package_install_uses_approved_host_route_and_cpp_operation(self) -> None:
        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        arguments = {
            "manager": "auto",
            "package": "nginx",
            "version": "1.27.0-1",
            "timeoutSeconds": 10,
        }
        fingerprint = approval_fingerprint(
            "package.install",
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
            allowed_tools=frozenset({"package.install"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-package-install"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-1",
            plan_step_id="step-1",
            evidence_ids=("evidence-1",),
            run_id="run-package-install-1",
            trace_id="trace-package-install-1",
            call_id="call-package-install-1",
            context_target_id="srv_stage1",
        )
        FakeDesktopSshClient.calls.clear()
        FakeDesktopSshProcess.instances.clear()

        with (
            patch("yukinal_agent.ssh_tools.SshServiceProcess", FakeDesktopSshProcess),
            patch("yukinal_agent.ssh_tools.SshServiceClient", FakeDesktopSshClient),
        ):
            result = await registry.execute(
                "package.install", arguments, context, call_id=context.call_id
            )

        self.assertEqual(
            result.value,
            {"manager": "auto", "package": "nginx", "version": "1.27.0-1", "installed": True},
        )
        self.assertEqual(
            FakeDesktopSshClient.calls,
            [("package_install", ("srv_stage1", "auto", "nginx", "1.27.0-1", 10))],
        )
        self.assertEqual(
            [method for method, _params in host.calls],
            ["host.ssh.effectful_action_target", "host.ssh.effectful_action_finished"],
        )
        self.assertEqual(host.calls[0][1]["toolCall"]["toolName"], "package.install")
        self.assertEqual(
            host.calls[0][1]["toolCall"]["approvalId"], "apr-approved-package-install"
        )
        self.assertEqual(host.calls[1][1]["outcome"], "succeeded")
        self.assertTrue(FakeDesktopSshProcess.instances[0].started)
        self.assertTrue(FakeDesktopSshProcess.instances[0].stopped)

    async def test_desktop_server_exec_uses_approved_one_shot_cpp_operation(self) -> None:
        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        arguments = {
            "command": "printf hello",
            "purpose": "check deployment output",
            "timeoutMs": 1000,
            "maxOutputBytes": 4096,
            "workdir": "/srv/app",
            "env": {"LANG": "C"},
        }
        fingerprint = approval_fingerprint(
            "server.exec",
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
            allowed_tools=frozenset({"server.exec"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-server-exec"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-1",
            plan_step_id="step-1",
            evidence_ids=("evidence-1",),
            run_id="run-server-exec-1",
            trace_id="trace-server-exec-1",
            call_id="call-server-exec-1",
            context_target_id="srv_stage1",
        )
        FakeDesktopSshClient.calls.clear()
        FakeDesktopSshProcess.instances.clear()
        FakeDesktopSshProcess.payloads.clear()

        with (
            patch("yukinal_agent.ssh_tools.SshServiceProcess", FakeDesktopSshProcess),
            patch("yukinal_agent.ssh_tools.SshServiceClient", FakeDesktopSshClient),
        ):
            result = await registry.execute("server.exec", arguments, context)

        expected_output = {
            "state": "completed",
            "exitCode": 0,
            "stdout": "hello\n",
            "stderr": "",
            "stdoutTruncated": False,
            "stderrTruncated": False,
            "durationMs": 9,
        }
        self.assertEqual(result.value, expected_output)
        self.assertEqual(
            FakeDesktopSshClient.calls,
            [
                (
                    "server_exec",
                    ("srv_stage1", "printf hello", 1000, 4096, "/srv/app", {"LANG": "C"}),
                )
            ],
        )
        self.assertEqual(
            [method for method, _params in host.calls],
            ["host.ssh.effectful_action_target", "host.ssh.effectful_action_finished"],
        )
        tool_call = host.calls[0][1]["toolCall"]
        self.assertEqual(tool_call["approvalId"], "apr-approved-server-exec")
        self.assertEqual(tool_call["toolName"], "server.exec")
        self.assertTrue(
            FakeDesktopSshProcess.payloads[0]["target"]["allow_server_exec"]  # type: ignore[index]
        )
        self.assertEqual(host.calls[1][1]["outcome"], "succeeded")
        self.assertEqual(host.calls[1][1]["result"], expected_output)
        self.assertTrue(FakeDesktopSshProcess.instances[0].started)
        self.assertTrue(FakeDesktopSshProcess.instances[0].stopped)

    async def test_desktop_server_exec_nonzero_preserves_result_for_agent(self) -> None:
        host = FakeDesktopHost()
        audit = MemoryAuditSink()
        registry = ToolRegistry(audit)
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        arguments = {
            "command": "check-config",
            "purpose": "inspect the validator failure",
            "timeoutMs": 1000,
            "maxOutputBytes": 4096,
        }
        fingerprint = approval_fingerprint(
            "server.exec",
            "srv_stage1",
            arguments,
            task_id="task-ssh-nonzero",
            tenant_id="tenant-1",
            user_id="operator-1",
        )
        context = ExecutionContext(
            task_id="task-ssh-nonzero",
            tenant_id="tenant-1",
            user_id="operator-1",
            allowed_tools=frozenset({"server.exec"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-server-exec"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-nonzero",
            plan_id="plan-1",
            plan_step_id="step-1",
            run_id="run-server-exec-nonzero",
            trace_id="trace-server-exec-nonzero",
            call_id="call-server-exec-nonzero",
            context_target_id="srv_stage1",
        )
        command_result = SimpleNamespace(
            exit_code=2,
            stdout="config check failed\n",
            stderr="invalid directive\n",
            stdout_truncated=False,
            stderr_truncated=False,
            duration_ms=18,
        )
        expected_output = {
            "state": "completed",
            "exitCode": 2,
            "stdout": "config check failed\n",
            "stderr": "invalid directive\n",
            "stdoutTruncated": False,
            "stderrTruncated": False,
            "durationMs": 18,
        }
        FakeDesktopSshProcess.instances.clear()
        FakeDesktopSshProcess.payloads.clear()

        with (
            patch("yukinal_agent.ssh_tools.SshServiceProcess", FakeDesktopSshProcess),
            patch("yukinal_agent.ssh_tools.SshServiceClient", FakeDesktopSshClient),
            patch.object(FakeDesktopSshClient, "server_exec", return_value=command_result),
            self.assertRaises(ToolCallError) as caught,
        ):
            await registry.execute("server.exec", arguments, context)

        self.assertEqual(caught.exception.code, "execution_failed")
        self.assertEqual(caught.exception.result, expected_output)
        self.assertEqual(host.calls[1][1]["outcome"], "failed")
        self.assertEqual(host.calls[1][1]["result"], expected_output)
        self.assertEqual(audit.events[-1].status, "failed")
        self.assertEqual(audit.events[-1].error_code, "execution_failed")
        self.assertEqual(audit.events[-1].result_summary["keys"], sorted(expected_output))
        self.assertTrue(FakeDesktopSshProcess.instances[0].stopped)

    async def test_desktop_systemd_restart_does_not_fallback_after_target_rpc_failure(self) -> None:
        host = FakeDesktopHost()
        host.request = AsyncMock(side_effect=RuntimeError("host timeout"))  # type: ignore[method-assign]
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        arguments = {"service": "nginx.service", "timeoutSeconds": 10}
        fingerprint = approval_fingerprint(
            "systemd.restart",
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
            allowed_tools=frozenset({"systemd.restart"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-systemd"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-1",
            plan_step_id="step-1",
            run_id="run-systemd-1",
            trace_id="trace-systemd-1",
            call_id="call-systemd-1",
            context_target_id="srv_stage1",
        )
        fallback = AsyncMock(return_value={"service": "nginx.service", "restarted": True})

        with patch("yukinal_agent.ssh_tools._execute_on_host", fallback):
            with self.assertRaises(ToolCallError) as caught:
                await registry.execute("systemd.restart", arguments, context)

        self.assertEqual(caught.exception.code, "host_unavailable")
        fallback.assert_not_awaited()

    async def test_desktop_systemd_restart_falls_back_only_when_host_rejects_cpp_route(self) -> None:
        host = FakeDesktopHost()
        host.request = AsyncMock(return_value={
            "status": "failed",
            "error": {"code": "unsupported_route", "message": "legacy SSH mode"},
        })  # type: ignore[method-assign]
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        arguments = {"service": "nginx.service", "timeoutSeconds": 10}
        fingerprint = approval_fingerprint(
            "systemd.restart",
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
            allowed_tools=frozenset({"systemd.restart"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-systemd"},
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            host_task_id="task-durable-1",
            plan_id="plan-1",
            plan_step_id="step-1",
            run_id="run-systemd-1",
            trace_id="trace-systemd-1",
            call_id="call-systemd-1",
            context_target_id="srv_stage1",
        )
        fallback = AsyncMock(return_value={"service": "nginx.service", "restarted": True})

        with patch("yukinal_agent.ssh_tools._execute_on_host", fallback):
            result = await registry.execute(
                "systemd.restart", arguments, context, call_id=context.call_id
            )

        self.assertEqual(result.value, {"service": "nginx.service", "restarted": True})
        fallback.assert_awaited_once_with(host, "systemd.restart", arguments, context)

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
                "host.ssh.filesystem_write_target",
            ],
        )
        self.assertEqual(host.calls[0][1], {
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

    async def test_filesystem_backup_uses_cpp_and_registers_host_ownership_once(self) -> None:
        import os
        import tempfile

        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        arguments = {"path": "/etc/app.conf"}
        fingerprint = approval_fingerprint(
            "filesystem.backup",
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
            allowed_tools=frozenset({"filesystem.backup"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-backup"},
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
            first = await registry.execute("filesystem.backup", arguments, context, call_id="call-backup-1")
            second = await registry.execute("filesystem.backup", arguments, context, call_id="call-backup-1")

        backup_path = (
            f"/etc/.yukinal-backup-{hashlib.sha256(arguments['path'].encode()).hexdigest()[:16]}-"
            f"{'0' * 32}"
        )
        expected = {
            "path": "/etc/app.conf",
            "backupPath": backup_path,
            "revision": hashlib.sha256(b"hello\x00x").hexdigest(),
            "bytesBackedUp": 7,
        }
        self.assertEqual(first.value, expected)
        self.assertEqual(second.value, expected)
        self.assertEqual(
            FakeDesktopSshClient.calls,
            [
                ("sftp_stat", ("srv_stage1", "/etc/app.conf")),
                ("sftp_link_count", ("srv_stage1", "/etc/app.conf")),
                ("sftp_read", ("srv_stage1", "/etc/app.conf", 1_048_576)),
                ("sftp_stat", ("srv_stage1", "/etc/app.conf")),
                ("sftp_create_exclusive", ("srv_stage1", backup_path, b"hello\x00x")),
            ],
        )
        self.assertEqual(
            [method for method, _params in host.calls],
            [
                "host.ssh.filesystem_backup_target",
                "host.investigation.filesystem_backup.register",
            ],
        )
        self.assertEqual(host.calls[1][1]["revision"], expected["revision"])
        self.assertEqual(host.calls[1][1]["bytesBackedUp"], 7)
        self.assertTrue(FakeDesktopSshProcess.instances[0].started)
        self.assertTrue(FakeDesktopSshProcess.instances[0].stopped)

    async def test_filesystem_restore_checks_both_revisions_and_consumes_host_ledger_once(self) -> None:
        import os
        import tempfile

        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        backup_path = (
            f"/var/.yukinal-backup-{hashlib.sha256(b'/var/app.conf').hexdigest()[:16]}-"
            f"{'0' * 32}"
        )
        arguments = {
            "path": "/var/app.conf",
            "backupPath": backup_path,
            "expectedRevision": hashlib.sha256(b"new config").hexdigest(),
        }
        fingerprint = approval_fingerprint(
            "filesystem.restore",
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
            allowed_tools=frozenset({"filesystem.restore"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-restore"},
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
            first = await registry.execute("filesystem.restore", arguments, context, call_id="call-restore-1")
            second = await registry.execute("filesystem.restore", arguments, context, call_id="call-restore-1")

        restored_revision = hashlib.sha256(b"old config").hexdigest()
        expected = {
            "path": arguments["path"],
            "backupPath": backup_path,
            "revision": restored_revision,
            "bytesBefore": len(b"new config"),
            "bytesAfter": len(b"old config"),
        }
        self.assertEqual(first.value, expected)
        self.assertEqual(second.value, expected)
        self.assertEqual(
            FakeDesktopSshClient.calls,
            [
                ("sftp_stat", ("srv_stage1", arguments["path"])),
                ("sftp_link_count", ("srv_stage1", arguments["path"])),
                ("sftp_read", ("srv_stage1", arguments["path"], 1_048_576)),
                ("sftp_stat", ("srv_stage1", arguments["path"])),
                ("sftp_stat", ("srv_stage1", backup_path)),
                ("sftp_link_count", ("srv_stage1", backup_path)),
                ("sftp_read", ("srv_stage1", backup_path, 1_048_576)),
                ("sftp_stat", ("srv_stage1", backup_path)),
                (
                    "sftp_replace_guarded",
                    (
                        "srv_stage1", arguments["path"], b"old config",
                        {
                            "expected_size": len(b"new config"),
                            "expected_modified": 200,
                            "expected_digest": arguments["expectedRevision"],
                            "timeout_seconds": 25,
                        },
                    ),
                ),
            ],
        )
        self.assertEqual(
            [method for method, _params in host.calls],
            [
                "host.ssh.filesystem_restore_target",
                "host.investigation.filesystem_backup.restored",
            ],
        )
        self.assertEqual(host.calls[1][1]["revision"], restored_revision)
        self.assertTrue(FakeDesktopSshProcess.instances[0].started)
        self.assertTrue(FakeDesktopSshProcess.instances[0].stopped)

    async def test_filesystem_backup_cleanup_verifies_cpp_bytes_and_consumes_host_ledger_once(self) -> None:
        import os
        import tempfile

        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        backup_path = (
            f"/etc/.yukinal-backup-{hashlib.sha256(b'/etc/app.conf').hexdigest()[:16]}-"
            f"{'0' * 32}"
        )
        arguments = {
            "path": "/etc/app.conf",
            "backupPath": backup_path,
            "expectedRevision": hashlib.sha256(b"hello\x00x").hexdigest(),
        }
        fingerprint = approval_fingerprint(
            "filesystem.backup.cleanup",
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
            allowed_tools=frozenset({"filesystem.backup.cleanup"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-cleanup"},
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
            first = await registry.execute(
                "filesystem.backup.cleanup", arguments, context, call_id="call-cleanup-1"
            )
            second = await registry.execute(
                "filesystem.backup.cleanup", arguments, context, call_id="call-cleanup-1"
            )

        expected = {
            "path": arguments["path"],
            "backupPath": backup_path,
            "revision": arguments["expectedRevision"],
            "bytesDeleted": 7,
        }
        self.assertEqual(first.value, expected)
        self.assertEqual(second.value, expected)
        self.assertEqual(
            FakeDesktopSshClient.calls,
            [
                ("sftp_stat", ("srv_stage1", backup_path)),
                ("sftp_link_count", ("srv_stage1", backup_path)),
                ("sftp_read", ("srv_stage1", backup_path, 1_048_576)),
                ("sftp_stat", ("srv_stage1", backup_path)),
                ("sftp_remove_file", ("srv_stage1", backup_path, 20)),
            ],
        )
        self.assertEqual(
            [method for method, _params in host.calls],
            [
                "host.ssh.filesystem_cleanup_target",
                "host.investigation.filesystem_backup.deleted",
            ],
        )
        self.assertTrue(FakeDesktopSshProcess.instances[0].started)
        self.assertTrue(FakeDesktopSshProcess.instances[0].stopped)

    async def test_filesystem_backup_cleanup_batch_keeps_partial_host_results(self) -> None:
        import os
        import tempfile

        host = FakeDesktopHost()
        registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(registry, host, Path("ssh-service.exe"))  # type: ignore[arg-type]
        revision = hashlib.sha256(b"hello\x00x").hexdigest()
        first_path = (
            f"/etc/.yukinal-backup-{hashlib.sha256(b'/etc/app.conf').hexdigest()[:16]}-"
            f"{'1' * 32}"
        )
        missing_path = (
            f"/opt/.yukinal-backup-{hashlib.sha256(b'/opt/missing.conf').hexdigest()[:16]}-"
            f"{'2' * 32}"
        )
        arguments = {"items": [
            {"path": "/etc/app.conf", "backupPath": first_path, "expectedRevision": revision},
            {"path": "/opt/missing.conf", "backupPath": missing_path, "expectedRevision": revision},
        ]}
        fingerprint = approval_fingerprint(
            "filesystem.backup.cleanup",
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
            allowed_tools=frozenset({"filesystem.backup.cleanup"}),
            allowed_target_ids=frozenset({"srv_stage1"}),
            approved_actions={fingerprint: "apr-approved-cleanup-batch"},
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
            result = await registry.execute(
                "filesystem.backup.cleanup", arguments, context, call_id="call-cleanup-batch"
            )

        self.assertEqual(result.value, {
            "items": [
                {
                    "path": "/etc/app.conf",
                    "backupPath": first_path,
                    "outcome": "removed",
                    "revision": revision,
                    "bytesDeleted": 7,
                },
                {
                    "path": "/opt/missing.conf",
                    "backupPath": missing_path,
                    "outcome": "skipped",
                    "reason": "no available host ledger record",
                },
            ],
            "partial": True,
        })
        self.assertEqual(
            [operation for operation, _arguments in FakeDesktopSshClient.calls].count("sftp_remove_file"),
            1,
        )
        self.assertEqual(
            [method for method, _params in host.calls].count(
                "host.investigation.filesystem_backup.deleted"
            ),
            1,
        )

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
        self.assertEqual(
            _parse_package_inspect("__YUKINAL_MANAGER__=yum\nabsent\n", "auto", "fastfetch"),
            {"manager": "yum", "package": "fastfetch", "installed": False},
        )
        with self.assertRaises(ToolCallError) as unsupported:
            _parse_package_inspect("unsupported\n", "dnf", "nginx")
        self.assertEqual(unsupported.exception.code, "unsupported")
        # The `auto` probe reports a target with no supported manager through the
        # marker alone. That must read as "unsupported", not as a malformed reply:
        # the CentOS 7 host that first exposed this returned a second status line.
        for raw in ("__YUKINAL_MANAGER__=unsupported\n",
                    "__YUKINAL_MANAGER__=unsupported\nunsupported\n"):
            with self.subTest(raw=raw.strip()):
                with self.assertRaises(ToolCallError) as detected:
                    _parse_package_inspect(raw, "auto", "fastfetch")
                self.assertEqual(detected.exception.code, "unsupported")

    async def test_process_and_port_parsers_keep_provenance_and_bounded_fields(self) -> None:
        processes = _parse_process_inventory(
            "  912     1 www-data  38.5  2.1 Ssl  1200 nginx\n"
            "not a process row\n"
            "  980   912 root       1.5  0.4 S      900 php-fpm\n"
            "__YUKINAL_SOURCE__=ps\n"
        )
        self.assertEqual(processes["source"], "ps")
        self.assertEqual(processes["processes"], [
            {"pid": 912, "parentPid": 1, "user": "www-data", "cpuPercent": 38.5,
             "memoryPercent": 2.1, "state": "Ssl", "elapsedSeconds": 1200, "command": "nginx"},
            {"pid": 980, "parentPid": 912, "user": "root", "cpuPercent": 1.5,
             "memoryPercent": 0.4, "state": "S", "elapsedSeconds": 900, "command": "php-fpm"},
        ])
        self.assertEqual(_parse_process_inventory("__YUKINAL_SOURCE__=unavailable\n")["processes"], [])
        with self.assertRaises(ToolCallError):
            _parse_process_inventory("912 1 root 1 1 S 1 nginx\n")

        listeners = _parse_port_inventory(
            'tcp   LISTEN 0  511  0.0.0.0:80  0.0.0.0:* users:(("nginx",pid=912,fd=6))\n'
            'tcp   LISTEN 0  128  127.0.0.1:5432  0.0.0.0:* users:(("postgres",pid=1200,fd=7))\n'
            "__YUKINAL_SOURCE__=ss\n"
        )
        self.assertTrue(listeners["processInfoAvailable"])
        self.assertEqual(listeners["listeners"], [
            {"protocol": "tcp", "address": "0.0.0.0", "port": 80, "process": "nginx"},
            {"protocol": "tcp", "address": "127.0.0.1", "port": 5432, "process": "postgres"},
        ])
        # Without privilege the socket facts remain; the missing owner is stated.
        hidden = _parse_port_inventory("tcp   LISTEN 0  511  0.0.0.0:80  0.0.0.0:*\n__YUKINAL_SOURCE__=ss\n")
        self.assertFalse(hidden["processInfoAvailable"])
        self.assertIn("message", hidden)
        with self.assertRaises(ToolCallError):
            _parse_port_inventory("tcp LISTEN 0 1 0.0.0.0:80 0.0.0.0:*\n")

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

    async def test_disk_usage_is_a_structured_read_only_cpp_tool(self) -> None:
        client = FakeSshServiceClient()
        registry = ToolRegistry(MemoryAuditSink())
        register_ssh_tools(registry, client)  # type: ignore[arg-type]
        spec = registry.get("ssh.disk_usage")
        self.assertIsNotNone(spec)
        self.assertEqual(spec.risk_level, "read")
        self.assertFalse(spec.requires_approval)
        self.assertNotIn("target_id", spec.input_schema["properties"])

        result = await AgentWorkflow(registry).run(
            task_id="task-ssh-1",
            tenant_id="tenant-1",
            user_id="operator-1",
            allowed_tools={"ssh.disk_usage"},
            allowed_target_ids={"srv_stage1"},
            tool_calls=[{"name": "ssh.disk_usage", "arguments": {}}],
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        )

        self.assertEqual(result.status, "succeeded")
        self.assertEqual(client.calls, [("srv_stage1", "disk_usage", 10)])
        self.assertEqual(result.results[0]["result"], {
            "target_id": "srv_stage1",
            "filesystem": "/dev/root",
            "mount": "/",
            "total_bytes": 1_024_000,
            "used_bytes": 256_000,
            "available_bytes": 768_000,
            "usage_percent": 25,
            "duration_ms": 12,
        })

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
