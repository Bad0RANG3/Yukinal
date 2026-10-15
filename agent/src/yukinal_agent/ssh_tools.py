from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from yukinal_ssh import SshServiceClient, SshServiceError

from .host_rpc import HostRpcClient
from .host_tools import _execute_on_host
from .ssh_service_process import SshServiceProcess, SshServiceProcessError
from .tools import ExecutionContext, ToolCallError, ToolRegistry, ToolSpec


def register_ssh_tools(registry: ToolRegistry, client: SshServiceClient) -> None:
    async def service_status(arguments: dict[str, Any], context: ExecutionContext) -> dict[str, object]:
        target_id = context.context_target_id
        if not isinstance(target_id, str):
            raise ToolCallError("target_required", "A selected remote server is required")
        result = await asyncio.to_thread(
            client.service_status,
            target_id,
            arguments["service"],
            arguments.get("timeout_seconds", 10),
        )
        return {
            "target_id": result.target_id,
            "operation": result.operation,
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "duration_ms": result.duration_ms,
            "output_truncated": result.output_truncated,
        }

    registry.register(
        ToolSpec(
            name="ssh.service_status",
            description="Read the active state of one allow-listed systemd service on the selected remote server.",
            input_schema={
                "type": "object",
                "properties": {
                    "service": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,127}$"},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 30},
                },
                "required": ["service"],
                "additionalProperties": False,
            },
            risk_level="read",
            requires_approval=False,
            handler=service_status,
            target_id_argument=None,
            target_required=True,
            timeout_ms=35_000,
        )
    )


def register_desktop_ssh_tools(
    registry: ToolRegistry, host: HostRpcClient, binary: Path
) -> frozenset[str]:
    """Use the desktop's saved identity and pin for one short-lived C++ process."""

    tools: tuple[tuple[str, str, dict[str, Any], str], ...] = (
        (
            "ssh.service_status",
            "Read the active state of one allow-listed systemd service on the selected remote server.",
            {
                "type": "object",
                "properties": {
                    "service": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,127}$"},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 30},
                },
                "required": ["service"],
                "additionalProperties": False,
            },
            "service_status",
        ),
        (
            "server.logs",
            "Read a bounded tail from the selected server's journal or system log.",
            {
                "type": "object",
                "properties": {
                    "sinceSeconds": {"type": "integer", "minimum": 1, "maximum": 604800},
                    "unit": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,119}\\.service$"},
                },
                "additionalProperties": False,
            },
            "server_logs",
        ),
        (
            "docker.ps",
            "List Docker containers on the selected server without changing state.",
            {
                "type": "object",
                "properties": {"all": {"type": "boolean"}},
                "additionalProperties": False,
            },
            "docker_ps",
        ),
        (
            "docker.logs",
            "Read a bounded log tail from one Docker container.",
            {
                "type": "object",
                "properties": {
                    "container": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$"},
                    "tail": {"type": "integer", "minimum": 1, "maximum": 500},
                },
                "required": ["container"],
                "additionalProperties": False,
            },
            "docker_logs",
        ),
        (
            "docker.inspect",
            "Read normalized state and health for one Docker container.",
            {
                "type": "object",
                "properties": {
                    "container": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$"},
                },
                "required": ["container"],
                "additionalProperties": False,
            },
            "docker_inspect",
        ),
        (
            "systemd.inspect",
            "Read the normalized load, active, and sub-state of one systemd service.",
            {
                "type": "object",
                "properties": {
                    "service": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,119}\\.service$"},
                },
                "required": ["service"],
                "additionalProperties": False,
            },
            "systemd_inspect",
        ),
        (
            "package.inspect",
            "Read whether one apt or dnf package is installed and its normalized version.",
            {
                "type": "object",
                "properties": {
                    "manager": {"type": "string", "enum": ["apt", "dnf"]},
                    "package": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9+._:-]{0,127}$"},
                },
                "required": ["manager", "package"],
                "additionalProperties": False,
            },
            "package_inspect",
        ),
    )

    async def execute(
        tool_name: str,
        operation: str,
        arguments: dict[str, Any],
        context: ExecutionContext,
    ) -> dict[str, object]:
        target_id = context.context_target_id
        if not isinstance(target_id, str):
            raise ToolCallError("target_required", "A selected remote server is required")
        try:
            payload = await host.request(
                "host.ssh.service_target", {"serverId": target_id}, timeout_seconds=8.0
            )
        except asyncio.CancelledError:
            raise
        except Exception:
            return await _execute_on_host(host, tool_name, arguments, context)
        try:
            process, token, base_url = SshServiceProcess.for_desktop_target(binary, payload)
        except SshServiceProcessError:
            return await _execute_on_host(host, tool_name, arguments, context)

        try:
            await process.start()
        except asyncio.CancelledError:
            await process.stop()
            raise
        except SshServiceProcessError:
            await process.stop()
            return await _execute_on_host(host, tool_name, arguments, context)

        try:
            client = SshServiceClient(token, base_url=base_url)
            if operation == "service_status":
                result = await asyncio.to_thread(
                    client.service_status,
                    target_id,
                    arguments["service"],
                    arguments.get("timeout_seconds", 10),
                )
            elif operation == "server_logs":
                result = await asyncio.to_thread(
                    client.server_logs,
                    target_id,
                    arguments.get("sinceSeconds"),
                    arguments.get("unit"),
                )
            elif operation == "docker_ps":
                result = await asyncio.to_thread(
                    client.docker_ps, target_id, arguments.get("all", False)
                )
            elif operation == "docker_logs":
                result = await asyncio.to_thread(
                    client.docker_logs,
                    target_id,
                    arguments["container"],
                    arguments.get("tail", 120),
                )
            elif operation == "docker_inspect":
                result = await asyncio.to_thread(
                    client.docker_inspect, target_id, arguments["container"]
                )
            elif operation == "systemd_inspect":
                result = await asyncio.to_thread(
                    client.systemd_inspect, target_id, arguments["service"]
                )
            elif operation == "package_inspect":
                result = await asyncio.to_thread(
                    client.package_inspect,
                    target_id,
                    arguments["manager"],
                    arguments["package"],
                )
            else:
                raise ToolCallError("unknown_tool", f"Unsupported C++ SSH operation for {tool_name}")
        except asyncio.CancelledError:
            raise
        except SshServiceError as error:
            raise ToolCallError(error.code, str(error)) from None
        finally:
            await process.stop()

        return {
            "target_id": result.target_id,
            "operation": result.operation,
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "duration_ms": result.duration_ms,
            "output_truncated": result.output_truncated,
        }

    existing_names = {spec.name for spec in registry.list()}
    registry.register_many([
        ToolSpec(
            name=name,
            description=description,
            input_schema=schema,
            risk_level="read",
            requires_approval=False,
            handler=lambda arguments, context, tool_name=name, operation=operation: execute(
                tool_name, operation, arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=35_000,
            cancellable=True,
        )
        for name, description, schema, operation in tools
        if name not in existing_names
    ])
    return frozenset(name for name, _, _, _ in tools if name != "ssh.service_status")
