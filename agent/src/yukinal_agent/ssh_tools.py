from __future__ import annotations

import asyncio
import hashlib
import json
import re
import unicodedata
from pathlib import Path
from typing import Any

from yukinal_ssh import SshServiceClient, SshServiceError

from .host_rpc import HostRpcClient
from .host_tools import (
    _FILESYSTEM_WRITE_INPUT_SCHEMA,
    _execute_on_host,
)
from .filesystem_action_ledger import FilesystemActionLedger, FilesystemActionLedgerError
from .ssh_service_process import SshServiceProcess, SshServiceProcessError
from .system_snapshot import normalize_system_snapshot
from .tools import ExecutionContext, ToolCallError, ToolRegistry, ToolSpec

_ASCII_LOWER = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")
_MAX_SERVER_LOG_LINES = 500
_MAX_DOCKER_PS_CONTAINERS = 200
_MAX_DOCKER_LOG_LINE_CHARS = 4_000


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
            "server.info",
            "Collect a fresh read-only health snapshot for the selected remote server.",
            {"type": "object", "additionalProperties": False, "properties": {}},
            "system_snapshot",
        ),
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
        (
            "filesystem.read",
            "Read a bounded text file on the selected remote server without changing state.",
            {
                "type": "object",
                "additionalProperties": False,
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "^/[^\\u0000-\\u001f]*$",
                    },
                    "maxBytes": {"type": "integer", "minimum": 1, "maximum": 1048576},
                },
            },
            "filesystem_read",
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
            method = (
                "host.ssh.filesystem_read_target"
                if operation == "filesystem_read"
                else "host.ssh.service_target"
            )
            params: dict[str, Any] = {"serverId": target_id}
            if operation == "filesystem_read":
                params.update(
                    {"path": arguments["path"], "maxBytes": arguments.get("maxBytes")}
                )
            payload = await host.request(
                method, params, timeout_seconds=8.0
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
            if operation == "filesystem_read":
                read_binding = (
                    payload.get("filesystemRead") if isinstance(payload, dict) else None
                )
                max_bytes = (
                    read_binding.get("maxBytes") if isinstance(read_binding, dict) else None
                )
                if (
                    not isinstance(read_binding, dict)
                    or read_binding.get("path") != arguments["path"]
                    or isinstance(max_bytes, bool)
                    or not isinstance(max_bytes, int)
                    or not 1 <= max_bytes <= 1024 * 1024
                ):
                    return await _execute_on_host(host, tool_name, arguments, context)
                raw = await asyncio.to_thread(
                    client.sftp_read, target_id, arguments["path"], max_bytes
                )
                kept = raw[:max_bytes]
                return {
                    "path": arguments["path"],
                    "content": kept.decode("utf-8", errors="replace"),
                    "truncated": len(raw) > max_bytes,
                    "revision": hashlib.sha256(kept).hexdigest(),
                }
            elif operation == "system_snapshot":
                outputs = await asyncio.to_thread(client.system_snapshot, target_id)
                snapshot = await _execute_on_host(
                    host,
                    tool_name,
                    {"snapshotData": normalize_system_snapshot(outputs)},
                    context,
                )
                if not isinstance(snapshot, dict):
                    raise ToolCallError(
                        "invalid_response", "desktop host returned an invalid system snapshot"
                    )
                return snapshot
            elif operation == "service_status":
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
                return _parse_server_logs(result.stdout, arguments)
            elif operation == "service_inventory":
                result = await asyncio.to_thread(client.service_inventory, target_id)
                return _parse_service_inventory(result.stdout, arguments)
            elif operation == "docker_ps":
                result = await asyncio.to_thread(
                    client.docker_ps, target_id, arguments.get("all", False)
                )
                if result.exit_code != 0:
                    return {"available": False, "containers": []}
                return {"available": True, "containers": _parse_docker_ps(result.stdout)}
            elif operation == "docker_logs":
                result = await asyncio.to_thread(
                    client.docker_logs,
                    target_id,
                    arguments["container"],
                    arguments.get("tail", 120),
                )
                if result.exit_code != 0:
                    raise ToolCallError(
                        "execution_failed",
                        f"could not read logs for container `{arguments['container']}`",
                    )
                return _parse_docker_logs(
                    result.stdout, arguments["container"], arguments.get("tail", 120)
                )
            elif operation == "docker_inspect":
                result = await asyncio.to_thread(
                    client.docker_inspect, target_id, arguments["container"]
                )
                if result.exit_code != 0:
                    raise ToolCallError(
                        "not_found", f"container `{arguments['container']}` was not found"
                    )
                return _parse_docker_inspect(result.stdout)
            elif operation == "systemd_inspect":
                result = await asyncio.to_thread(
                    client.systemd_inspect, target_id, arguments["service"]
                )
                if result.exit_code != 0:
                    raise ToolCallError(
                        "not_found",
                        f"systemd service `{arguments['service']}` was not found or could not be inspected",
                    )
                return _parse_systemd_inspect(result.stdout, arguments["service"])
            elif operation == "package_inspect":
                result = await asyncio.to_thread(
                    client.package_inspect,
                    target_id,
                    arguments["manager"],
                    arguments["package"],
                )
                if result.exit_code != 0:
                    raise ToolCallError(
                        "execution_failed", f"could not inspect package `{arguments['package']}`"
                    )
                return _parse_package_inspect(
                    result.stdout, arguments["manager"], arguments["package"]
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

    if "filesystem.write" not in existing_names:
        registry.register(ToolSpec(
            name="filesystem.write",
            description=(
                "Replace one complete remote text file after exact per-call approval. "
                "The write is executed by the Python Agent through the C++ SFTP service."
            ),
            input_schema=_FILESYSTEM_WRITE_INPUT_SCHEMA,
            risk_level="write",
            requires_approval=True,
            handler=lambda arguments, context: _desktop_filesystem_write(
                host, binary, arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=20_000,
            cancellable=True,
        ))
    return frozenset(
        {name for name, _, _, _ in tools if name != "ssh.service_status"}
        | {"filesystem.write"}
    )


async def _desktop_filesystem_write(
    host: HostRpcClient,
    binary: Path,
    arguments: dict[str, Any],
    context: ExecutionContext,
) -> dict[str, object]:
    """Run an approved, plan-checked file write through the C++ SFTP service."""
    target_id = context.context_target_id
    target = context.host_target
    if (
        not isinstance(target_id, str)
        or not isinstance(target, dict)
        or target.get("host") != "remote"
        or target.get("serverId") != target_id
        or not context.run_id
        or not context.trace_id
        or not context.call_id
        or not context.host_task_id
        or not context.plan_id
        or not context.plan_step_id
    ):
        raise ToolCallError(
            "durable_plan_required", "filesystem.write requires a durable remote action plan"
        )

    plan_request = {
        "taskId": context.host_task_id,
        "toolName": "filesystem.write",
        "input": arguments,
        "target": dict(target),
    }
    try:
        plan = await host.request(
            "host.investigation.plan.check", plan_request, timeout_seconds=8.0
        )
    except asyncio.CancelledError:
        raise
    except Exception:
        raise ToolCallError("host_unavailable", "The desktop host could not validate the action plan") from None
    if (
        not isinstance(plan, dict)
        or plan.get("status") != "allowed"
        or plan.get("planId") != context.plan_id
        or plan.get("stepId") != context.plan_step_id
        or plan.get("stepKind") != "action"
        or plan.get("evidenceIds") != list(context.evidence_ids)
    ):
        raise ToolCallError("plan_deviation", "The current durable plan does not authorize this file write")

    ledger = FilesystemActionLedger.from_environment()
    if ledger is None:
        return await _execute_on_host(host, "filesystem.write", arguments, context)
    ledger_scope = {
        "run_id": context.run_id,
        "trace_id": context.trace_id,
        "call_id": context.call_id,
        "tool_name": "filesystem.write",
        "target_id": target_id,
        "arguments": arguments,
        "plan_id": context.plan_id,
        "plan_step_id": context.plan_step_id,
        "evidence_ids": context.evidence_ids,
    }
    try:
        cached = await ledger.claim(**ledger_scope)
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    if cached is not None:
        return cached

    async def run_host_compatibility_path() -> dict[str, object]:
        output = await _execute_on_host(host, "filesystem.write", arguments, context)
        if not isinstance(output, dict):
            raise ToolCallError("invalid_host_response", "The desktop host returned an invalid file write result")
        try:
            await ledger.complete(**ledger_scope, output=output)
        except FilesystemActionLedgerError as error:
            raise ToolCallError(error.code, str(error)) from None
        return output

    try:
        payload = await host.request(
            "host.ssh.filesystem_write_target",
            {
                "serverId": target_id,
                "path": arguments["path"],
                "content": arguments["content"],
                "taskId": context.host_task_id,
                "planId": context.plan_id,
                "planStepId": context.plan_step_id,
                "evidenceIds": list(context.evidence_ids),
                "target": dict(target),
            },
            timeout_seconds=8.0,
        )
    except asyncio.CancelledError:
        raise
    except Exception:
        # The target resolver rejects modes and routes that still need the Rust compatibility path.
        return await run_host_compatibility_path()

    expected_bytes = len(arguments["content"].encode("utf-8"))
    binding = payload.get("filesystemWrite") if isinstance(payload, dict) else None
    if (
        not isinstance(binding, dict)
        or binding != {"path": arguments["path"], "bytesWritten": expected_bytes}
    ):
        raise ToolCallError("invalid_host_response", "The desktop host returned an invalid write binding")
    try:
        process, token, base_url = SshServiceProcess.for_desktop_target(binary, payload)
    except SshServiceProcessError:
        return await run_host_compatibility_path()
    try:
        await process.start()
    except asyncio.CancelledError:
        await process.stop()
        raise
    except SshServiceProcessError:
        await process.stop()
        return await run_host_compatibility_path()

    try:
        client = SshServiceClient(token, base_url=base_url)
        await asyncio.to_thread(
            client.sftp_write,
            target_id,
            arguments["path"],
            arguments["content"].encode("utf-8"),
        )
        output: dict[str, object] = {
            "path": arguments["path"],
            "bytesWritten": expected_bytes,
        }
        await ledger.complete(**ledger_scope, output=output)
        return output
    except asyncio.CancelledError:
        # The ledger intentionally remains `running`: SFTP may have committed before cancellation.
        raise
    except SshServiceError as error:
        # A transport error can arrive after the server applied the write. Never retry it here.
        raise ToolCallError(error.code, str(error)) from None
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    finally:
        await process.stop()


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


def _parse_server_logs(raw: str, arguments: dict[str, Any]) -> dict[str, object]:
    source_marker = "__YUKINAL_SOURCE__="
    source = next(
        (
            line.strip()[len(source_marker):]
            for line in raw.splitlines()
            if line.strip().startswith(source_marker)
        ),
        None,
    )
    if source not in {"journalctl", "syslog", "messages", "unavailable"}:
        raise ToolCallError(
            "execution_failed", "The C++ SSH service returned an invalid log response"
        )
    if (
        arguments.get("sinceSeconds") is not None or arguments.get("unit") is not None
    ) and source != "journalctl":
        raise ToolCallError(
            "unsupported",
            "server.logs filters require journalctl; the fallback log source was not "
            "returned unfiltered",
        )
    if source == "unavailable":
        return {
            "source": source,
            "lines": [],
            "message": "未检测到可读取的 journalctl 或系统日志文件",
        }

    lines: list[dict[str, str]] = []
    for raw_line in raw.splitlines():
        line = raw_line.rstrip()
        if not line.strip() or line.strip().startswith(source_marker):
            continue
        lowered = line.translate(_ASCII_LOWER)
        if any(
            marker in lowered
            for marker in ("panic", "emerg", "alert", "crit", "error", "failed", "failure")
        ):
            level = "error"
        elif any(marker in lowered for marker in ("warning", "warn", "degraded")):
            level = "warning"
        else:
            level = "info"
        lines.append({"text": line, "level": level})
        if len(lines) == _MAX_SERVER_LOG_LINES:
            break
    return {"source": source, "lines": lines}


def _parse_docker_ps(raw: str) -> list[dict[str, object]]:
    containers: list[dict[str, object]] = []
    for raw_line in raw.splitlines():
        try:
            row = json.loads(raw_line.strip())
        except json.JSONDecodeError:
            continue
        if not isinstance(row, dict) or any(
            key in row and not isinstance(row[key], str)
            for key in ("Names", "Image", "State", "Status")
        ):
            continue
        names, image, state, status = (
            row.get("Names"), row.get("Image"), row.get("State"), row.get("Status")
        )
        if not all(isinstance(value, str) for value in (names, image, state, status)):
            continue
        name = names.split(",", 1)[0].strip()
        image = image.strip()
        state = state.strip()
        status = status.strip()
        if not name or not image or not state or not status:
            continue
        containers.append({
            "name": name,
            "image": image,
            "state": state,
            "status": status,
            "restartCount": 0,
        })
        if len(containers) == _MAX_DOCKER_PS_CONTAINERS:
            break
    return containers


def _parse_docker_logs(raw: str, container: str, limit: int) -> dict[str, object]:
    source_lines = raw.splitlines()
    truncated = len(source_lines) > limit
    lines: list[str] = []
    for line in source_lines[:limit]:
        if len(line) > _MAX_DOCKER_LOG_LINE_CHARS:
            line = line[:_MAX_DOCKER_LOG_LINE_CHARS] + "…"
            truncated = True
        lines.append(line)
    return {"container": container, "lines": lines, "truncated": truncated}


def _parse_docker_inspect(raw: str) -> dict[str, object]:
    line = next((candidate.strip() for candidate in raw.splitlines() if candidate.strip()), None)
    if line is None:
        raise ToolCallError("execution_failed", "docker inspect returned no JSON object")
    try:
        row = json.loads(line)
    except json.JSONDecodeError:
        raise ToolCallError("execution_failed", "docker inspect returned invalid JSON") from None
    if not isinstance(row, dict):
        raise ToolCallError("execution_failed", "docker inspect returned invalid JSON")

    container_id = _docker_nonempty(row.get("Id"), "Id")
    raw_name = row.get("Name")
    if not isinstance(raw_name, str):
        raise ToolCallError("execution_failed", "docker inspect returned an invalid Name")
    name = raw_name.lstrip("/")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", name):
        raise ToolCallError("execution_failed", "docker inspect returned an invalid Name")

    config = row.get("Config")
    image = _docker_nonempty(config.get("Image") if isinstance(config, dict) else None, "Config.Image")
    state_data = row.get("State")
    if not isinstance(state_data, dict):
        raise ToolCallError("execution_failed", "docker inspect omitted State")
    state = _docker_nonempty(state_data.get("Status"), "State.Status")
    restart_count = row.get("RestartCount")
    if restart_count is None:
        restart_count = 0
    if (
        isinstance(restart_count, bool)
        or not isinstance(restart_count, int)
        or not 0 <= restart_count <= 4_294_967_295
    ):
        raise ToolCallError("execution_failed", "docker inspect returned invalid JSON")

    result: dict[str, object] = {
        "id": container_id,
        "name": name,
        "image": image,
        "state": state,
        "status": state,
        "restartCount": restart_count,
    }
    started_at = state_data.get("StartedAt")
    if started_at is not None:
        if not isinstance(started_at, str):
            raise ToolCallError("execution_failed", "docker inspect returned invalid JSON")
        if started_at.strip():
            result["startedAt"] = started_at
    health = state_data.get("Health")
    if health is not None:
        if not isinstance(health, dict):
            raise ToolCallError("execution_failed", "docker inspect returned invalid JSON")
        health_status = health.get("Status")
        if health_status is not None:
            if not isinstance(health_status, str):
                raise ToolCallError("execution_failed", "docker inspect returned invalid JSON")
            if health_status.strip():
                result["health"] = health_status
    return result


def _docker_nonempty(value: object, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ToolCallError("execution_failed", f"docker inspect omitted {field}")
    return value.strip()


def _parse_systemd_inspect(raw: str, service: str) -> dict[str, object]:
    if len(raw) > 8_192:
        raise ToolCallError("execution_failed", "systemd inspect output exceeded its size limit")
    values = raw.splitlines()
    names = ("LoadState", "ActiveState", "SubState")
    if len(values) < 3:
        missing = names[len(values)] if len(values) < len(names) else "ActiveState"
        raise ToolCallError("execution_failed", f"systemd inspect omitted {missing}")
    required = [value.strip() for value in values[:3]]
    for value, field in zip(required, names):
        if not value:
            raise ToolCallError("execution_failed", f"systemd inspect omitted {field}")
    if len(values) > 4:
        raise ToolCallError("execution_failed", "systemd inspect returned unexpected extra fields")
    description = values[3].strip() if len(values) == 4 else ""
    result: dict[str, object] = {
        "service": service,
        "loadState": required[0],
        "activeState": required[1],
        "subState": required[2],
    }
    if description:
        result["description"] = description
    return result


def _parse_package_inspect(raw: str, manager: str, package: str) -> dict[str, object]:
    if len(raw) > 4_096:
        raise ToolCallError("execution_failed", "package inspect output exceeded its size limit")
    lines = [line.strip() for line in raw.splitlines() if line.strip()]
    if not lines:
        raise ToolCallError("execution_failed", "package inspect returned no status")
    if len(lines) > 1:
        raise ToolCallError("execution_failed", "package inspect returned unexpected extra fields")
    line = lines[0]
    if line == "unsupported":
        raise ToolCallError("unsupported", f"package manager {manager} is unavailable on the target")
    if line == "absent":
        return {"manager": manager, "package": package, "installed": False}
    version = line.removeprefix("installed\t") if line.startswith("installed\t") else (
        line.removeprefix("install ok installed\t")
        if line.startswith("install ok installed\t")
        else None
    )
    if version is None or not version.strip():
        raise ToolCallError("execution_failed", "package inspect returned an invalid status")
    version = version.strip()
    if len(version) > 128 or any(unicodedata.category(character) == "Cc" for character in version):
        raise ToolCallError("execution_failed", "package inspect returned an invalid version")
    return {"manager": manager, "package": package, "installed": True, "version": version}


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
