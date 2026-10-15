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
            "server.services",
            "Discover bounded read-only systemd or Docker service status on the selected server.",
            {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "minLength": 1, "maxLength": 128, "pattern": "^[A-Za-z0-9][A-Za-z0-9_.@:/-]{0,127}$"},
                    "state": {"type": "string", "enum": ["running", "stopped", "failed", "unknown"]},
                },
                "additionalProperties": False,
            },
            "service_inventory",
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
            elif operation == "service_inventory":
                result = await asyncio.to_thread(client.service_inventory, target_id)
                return _parse_service_inventory(result.stdout, arguments)
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


def _parse_service_inventory(raw: str, arguments: dict[str, Any]) -> dict[str, object]:
    source_marker = "__YUKINAL_SOURCE__="
    source_value = next(
        (line.strip()[len(source_marker):] for line in raw.splitlines() if line.strip().startswith(source_marker)),
        None,
    )
    if source_value not in {"systemd", "docker", "unavailable"}:
        raise ToolCallError("execution_failed", "The C++ SSH service returned an invalid service inventory")

    services: list[dict[str, str]] = []
    for raw_line in raw.splitlines():
        line = raw_line.strip()
        if not line or line.startswith(source_marker):
            continue
        if source_value == "systemd":
            fields = line.split()
            offset = 1 if fields and fields[0] == "●" else 0
            if len(fields) < offset + 4:
                continue
            name, active, sub = fields[offset], fields[offset + 2], fields[offset + 3]
            description = " ".join(fields[offset + 4:])
            state = _systemd_service_state(active, sub)
            status = f"{active}/{sub}"
        elif source_value == "docker":
            fields = line.split("\t", 3)
            if len(fields) < 3:
                continue
            name, raw_state, status = (field.strip() for field in fields[:3])
            description = fields[3].strip() if len(fields) == 4 else ""
            if not name or not raw_state or not status:
                continue
            state = _docker_service_state(raw_state)
        else:
            continue

        service: dict[str, str] = {"name": name, "state": state, "status": status}
        if description:
            service["description"] = description
        services.append(service)
        if len(services) == 200:
            break

    name_filter = arguments.get("name")
    if name_filter is not None and (
        not isinstance(name_filter, str)
        or not name_filter.strip()
        or len(name_filter) > 128
        or not name_filter[0].isascii()
        or not name_filter[0].isalnum()
        or not all(
            character.isascii() and (character.isalnum() or character in "_.@:/-")
            for character in name_filter
        )
    ):
        raise ToolCallError("invalid_arguments", "server.services name must be a bounded service or container reference")
    state_filter = arguments.get("state")
    if name_filter is not None:
        services = [service for service in services if service["name"] == name_filter]
    if state_filter is not None:
        services = [service for service in services if service["state"] == state_filter]

    result: dict[str, object] = {"source": source_value, "services": services}
    if source_value == "unavailable":
        result["message"] = "未检测到可读取的 systemd 或 Docker 服务管理器"
    return result


def _systemd_service_state(active: str, sub: str) -> str:
    if active == "failed" or sub == "failed":
        return "failed"
    if active == "active" and sub == "running":
        return "running"
    if active in {"inactive", "deactivating"} or sub in {"dead", "exited"}:
        return "stopped"
    return "unknown"


def _docker_service_state(state: str) -> str:
    if state in {"running", "restarting"}:
        return "running"
    if state == "dead":
        return "failed"
    if state in {"created", "exited", "paused", "removing"}:
        return "stopped"
    return "unknown"
