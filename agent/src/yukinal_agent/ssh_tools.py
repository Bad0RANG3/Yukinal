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
    _FILESYSTEM_BACKUP_INPUT_SCHEMA,
    _FILESYSTEM_BACKUP_CLEANUP_INPUT_SCHEMA,
    _FILESYSTEM_EDIT_INPUT_SCHEMA,
    _FILESYSTEM_RESTORE_INPUT_SCHEMA,
    _FILESYSTEM_WRITE_INPUT_SCHEMA,
    _SERVER_EXEC_SCHEMA,
    _execute_on_host,
)
from .filesystem_action_ledger import FilesystemActionLedger, FilesystemActionLedgerError
from .ssh_service_process import SshServiceProcess, SshServiceProcessError
from .system_snapshot import normalize_system_snapshot
from .tools import (
    ExecutionContext,
    ToolCallError,
    ToolRegistry,
    ToolSpec,
    approval_fingerprint,
)

_ASCII_LOWER = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")
_MAX_SERVER_LOG_LINES = 100
_MAX_SERVICE_INVENTORY_LINES = 200
_MAX_PROCESS_LINES = 64
_MAX_PORT_LINES = 128
_MAX_DIRECTORY_ENTRIES = 256
_MAX_INVENTORY_OUTPUT_BYTES = 60 * 1024
_MAX_DOCKER_PS_CONTAINERS = 200
_MAX_DOCKER_LOG_LINE_CHARS = 4_000
_MAX_AGENT_EDIT_BYTES = 512 * 1024
_MAX_AGENT_BACKUP_BYTES = 1024 * 1024
_SYSTEMD_RESTART_INPUT_SCHEMA = {
    "type": "object",
    "properties": {
        "service": {
            "type": "string",
            "pattern": "^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,119}\\.service$",
        },
        "timeoutSeconds": {"type": "integer", "minimum": 1, "maximum": 25},
    },
    "required": ["service", "timeoutSeconds"],
    "additionalProperties": False,
}
_DOCKER_RESTART_INPUT_SCHEMA = {
    "type": "object",
    "properties": {
        "container": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$"},
        "timeoutSeconds": {"type": "integer", "minimum": 1, "maximum": 25},
    },
    "required": ["container", "timeoutSeconds"],
    "additionalProperties": False,
}
_PACKAGE_INSTALL_INPUT_SCHEMA = {
    "type": "object",
    "properties": {
        "manager": {
            "type": "string",
            "enum": ["auto"],
            "description": "Always use auto; detect the package manager on the selected target during the operation.",
        },
        "package": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9+._:-]{0,127}$"},
        "version": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9+._:~^-]{0,127}$"},
        "timeoutSeconds": {"type": "integer", "minimum": 1, "maximum": 25},
    },
    "required": ["manager", "package", "timeoutSeconds"],
    "additionalProperties": False,
}


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

    async def disk_usage(arguments: dict[str, Any], context: ExecutionContext) -> dict[str, object]:
        target_id = context.context_target_id
        if not isinstance(target_id, str):
            raise ToolCallError("target_required", "A selected remote server is required")
        result = await asyncio.to_thread(
            client.disk_usage, target_id, arguments.get("timeout_seconds", 10)
        )
        if result.exit_code != 0:
            raise ToolCallError("execution_failed", "could not read root filesystem usage")
        parsed = _parse_disk_usage(result.stdout)
        return {"target_id": target_id, **parsed, "duration_ms": result.duration_ms}

    registry.register(
        ToolSpec(
            name="ssh.disk_usage",
            description="Read total, used, and available capacity for the selected server's root filesystem.",
            input_schema={
                "type": "object",
                "properties": {"timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 30}},
                "additionalProperties": False,
            },
            risk_level="read",
            requires_approval=False,
            handler=disk_usage,
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
            "Collect a fresh read-only server snapshot: CPU, memory, disks, NVIDIA GPU, network, Docker and basic security checks.",
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
            "ssh.disk_usage",
            "Read total, used, and available capacity for the selected server's root filesystem.",
            {
                "type": "object",
                "properties": {"timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 30}},
                "additionalProperties": False,
            },
            "disk_usage",
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
            "Read whether a package is installed and its normalized version. Always use manager=auto so the target selects apt, dnf, or yum in the same SSH operation.",
            {
                "type": "object",
                "properties": {
                    "manager": {
                        "type": "string",
                        "enum": ["auto"],
                        "description": "Always use auto; detect apt, dnf, or yum on the target during this SSH operation.",
                    },
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
        (
            "filesystem.list",
            "List one directory level on the selected remote server without changing state. "
            "Returns names, types and sizes; it does not follow symlinks or recurse.",
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
                },
            },
            "filesystem_list",
        ),
        (
            "server.processes",
            "Read the fixed, read-only process table on the selected server, ranked by CPU: "
            "pid, parent, user, CPU/memory percent, state, elapsed seconds and command name.",
            {"type": "object", "additionalProperties": False, "properties": {}},
            "process_inventory",
        ),
        (
            "server.ports",
            "Read the fixed, read-only listening TCP/UDP sockets on the selected server. "
            "The owning process is reported only when the SSH user may read it.",
            {"type": "object", "additionalProperties": False, "properties": {}},
            "port_inventory",
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
                else "host.ssh.filesystem_list_target"
                if operation == "filesystem_list"
                else "host.ssh.service_target"
            )
            params: dict[str, Any] = {"serverId": target_id}
            if operation == "filesystem_read":
                params.update(
                    {"path": arguments["path"], "maxBytes": arguments.get("maxBytes")}
                )
            elif operation == "filesystem_list":
                params.update({"path": arguments["path"]})
            payload = await host.request(
                method, params, timeout_seconds=8.0
            )
        except asyncio.CancelledError:
            raise
        except Exception:
            if tool_name == "ssh.disk_usage":
                raise ToolCallError("ssh_target_unavailable", "could not resolve the selected target for C++ SSH") from None
            return await _execute_on_host(host, tool_name, arguments, context)
        try:
            process, token, base_url = SshServiceProcess.for_desktop_target(binary, payload)
        except SshServiceProcessError:
            if tool_name == "ssh.disk_usage":
                raise ToolCallError("ssh_target_unsupported", "the selected target is not supported by the C++ SSH service") from None
            return await _execute_on_host(host, tool_name, arguments, context)

        try:
            await process.start()
        except asyncio.CancelledError:
            await process.stop()
            raise
        except SshServiceProcessError:
            await process.stop()
            if tool_name == "ssh.disk_usage":
                raise ToolCallError("ssh_service_unavailable", "the C++ SSH service could not be started") from None
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
            elif operation == "filesystem_list":
                list_binding = (
                    payload.get("filesystemList") if isinstance(payload, dict) else None
                )
                if (
                    not isinstance(list_binding, dict)
                    or list_binding.get("path") != arguments["path"]
                ):
                    return await _execute_on_host(host, tool_name, arguments, context)
                entries = await asyncio.to_thread(
                    client.sftp_list, target_id, arguments["path"],
                )
                return {
                    "path": arguments["path"],
                    "entries": entries[:_MAX_DIRECTORY_ENTRIES],
                    "truncated": len(entries) > _MAX_DIRECTORY_ENTRIES,
                }
            elif operation == "process_inventory":
                result = await asyncio.to_thread(client.process_inventory, target_id)
                return _parse_process_inventory(result.stdout)
            elif operation == "port_inventory":
                result = await asyncio.to_thread(client.port_inventory, target_id)
                return _parse_port_inventory(result.stdout)
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
            elif operation == "disk_usage":
                result = await asyncio.to_thread(
                    client.disk_usage, target_id, arguments.get("timeout_seconds", 10)
                )
                if result.exit_code != 0:
                    raise ToolCallError("execution_failed", "could not read root filesystem usage")
                parsed = _parse_disk_usage(result.stdout)
                return {"target_id": target_id, **parsed, "duration_ms": result.duration_ms}
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
            parallel_safe=True,
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
    if "filesystem.edit" not in existing_names:
        registry.register(ToolSpec(
            name="filesystem.edit",
            description=(
                "Replace one unique exact byte sequence in a remote file after per-call approval. "
                "The file must still match the revision from a complete read; C++ publishes it "
                "with a guarded atomic SFTP replacement."
            ),
            input_schema=_FILESYSTEM_EDIT_INPUT_SCHEMA,
            risk_level="write",
            requires_approval=True,
            handler=lambda arguments, context: _desktop_filesystem_edit(
                host, binary, arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=45_000,
            cancellable=True,
        ))
    if "filesystem.backup" not in existing_names:
        registry.register(ToolSpec(
            name="filesystem.backup",
            description=(
                "Create one bounded recovery copy through the Python Agent and C++ SFTP service. "
                "The desktop host chooses the sibling path and records ownership in its durable ledger."
            ),
            input_schema=_FILESYSTEM_BACKUP_INPUT_SCHEMA,
            risk_level="write",
            requires_approval=True,
            handler=lambda arguments, context: _desktop_filesystem_backup(
                host, binary, arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=45_000,
            cancellable=True,
        ))
    if "filesystem.restore" not in existing_names:
        registry.register(ToolSpec(
            name="filesystem.restore",
            description=(
                "Restore a host-owned remote backup after exact per-call approval. Python verifies "
                "the current revision and backup digest; C++ publishes it with guarded atomic SFTP."
            ),
            input_schema=_FILESYSTEM_RESTORE_INPUT_SCHEMA,
            risk_level="write",
            requires_approval=True,
            handler=lambda arguments, context: _desktop_filesystem_restore(
                host, binary, arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=45_000,
            cancellable=True,
        ))
    if "filesystem.backup.cleanup" not in existing_names:
        registry.register(ToolSpec(
            name="filesystem.backup.cleanup",
            description=(
                "Remove one exact host-owned recovery copy, or a bounded approved rotation batch. "
                "Python verifies each backup digest through C++ SFTP; the host consumes its ownership ledger."
            ),
            input_schema=_FILESYSTEM_BACKUP_CLEANUP_INPUT_SCHEMA,
            risk_level="write",
            requires_approval=True,
            handler=lambda arguments, context: _desktop_filesystem_backup_cleanup(
                host, binary, arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=60_000,
            cancellable=True,
        ))
    if "systemd.restart" not in existing_names:
        registry.register(ToolSpec(
            name="systemd.restart",
            description=(
                "Restart one systemd service after exact per-call approval and host plan checks. "
                "Python runs the structured operation through the C++ SSH service."
            ),
            input_schema=_SYSTEMD_RESTART_INPUT_SCHEMA,
            risk_level="write",
            requires_approval=True,
            handler=lambda arguments, context: _desktop_ssh_effectful_action(
                host, binary, "systemd.restart", arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=40_000,
            cancellable=True,
        ))
    if "docker.restart" not in existing_names:
        registry.register(ToolSpec(
            name="docker.restart",
            description=(
                "Restart one Docker container after exact per-call approval and host plan checks. "
                "Python runs the structured operation through the C++ SSH service."
            ),
            input_schema=_DOCKER_RESTART_INPUT_SCHEMA,
            risk_level="write",
            requires_approval=True,
            handler=lambda arguments, context: _desktop_ssh_effectful_action(
                host, binary, "docker.restart", arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=40_000,
            cancellable=True,
        ))
    if "package.install" not in existing_names:
        registry.register(ToolSpec(
            name="package.install",
            description=(
                "Install one apt, dnf, or yum package after exact per-call approval and host plan checks. "
                "Python runs the structured operation through the C++ SSH service."
            ),
            input_schema=_PACKAGE_INSTALL_INPUT_SCHEMA,
            risk_level="write",
            requires_approval=True,
            handler=lambda arguments, context: _desktop_ssh_effectful_action(
                host, binary, "package.install", arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=60_000,
            cancellable=True,
        ))
    if "server.exec" not in existing_names:
        registry.register(ToolSpec(
            name="server.exec",
            description=(
                "Run one host-approved, plan-bound command through the one-shot C++ SSH service. "
                "The desktop host validates the input and supplies the command wrapper and capability."
            ),
            input_schema=_SERVER_EXEC_SCHEMA,
            risk_level="destructive",
            requires_approval=True,
            handler=lambda arguments, context: _desktop_ssh_effectful_action(
                host, binary, "server.exec", arguments, context
            ),
            target_id_argument=None,
            target_required=True,
            origin={"kind": "builtin"},
            timeout_ms=45_000,
            cancellable=True,
        ))
    return frozenset(
        {name for name, _, _, _ in tools if name != "ssh.service_status"}
        | {
            "filesystem.write", "filesystem.edit", "filesystem.backup", "filesystem.restore",
            "filesystem.backup.cleanup", "systemd.restart", "docker.restart",
            "package.install", "server.exec",
        }
    )


def _host_plan_binding(context: ExecutionContext) -> dict[str, Any]:
    """Task/plan fields for a host RPC, or nothing for a conversation run.

    The host refuses a partial set, so this is all-or-nothing. A conversation has
    no plan; its call is still bound by run/trace/call identity and the approval
    ticket, and its file mutations still go through the exactly-once ledger.
    """
    if context.host_task_id and context.plan_id and context.plan_step_id:
        return {
            "taskId": context.host_task_id,
            "planId": context.plan_id,
            "planStepId": context.plan_step_id,
            "evidenceIds": list(context.evidence_ids),
        }
    return {}


def _ssh_effectful_action_host_call(
    tool_name: str, arguments: dict[str, Any], context: ExecutionContext
) -> dict[str, Any]:
    target = context.host_target
    target_id = context.context_target_id
    if (
        not isinstance(target, dict)
        or target.get("host") != "remote"
        or not isinstance(target_id, str)
        or target.get("serverId") != target_id
        or not context.run_id
        or not context.trace_id
        or not context.call_id
    ):
        raise ToolCallError("target_required", "the SSH action requires a resolved remote server")
    fingerprint = approval_fingerprint(
        tool_name,
        target_id,
        arguments,
        task_id=context.task_id,
        tenant_id=context.tenant_id,
        user_id=context.user_id,
    )
    approval_id = context.approved_actions.get(fingerprint)
    if not approval_id:
        raise ToolCallError("approval_reference_missing", "the exact SSH action approval is unavailable")
    tool_call: dict[str, Any] = {
        "callId": context.call_id,
        "runId": context.run_id,
        "traceId": context.trace_id,
        "toolName": tool_name,
        "input": arguments,
        "target": dict(target),
        "approvalId": approval_id,
    }
    # The host refuses a partial task/plan/step set; a conversation sends none.
    tool_call.update(_host_plan_binding(context))
    return tool_call


async def _finish_desktop_ssh_effectful_action(
    host: HostRpcClient,
    tool_call: dict[str, Any],
    outcome: str,
    result: dict[str, Any] | None = None,
) -> None:
    params: dict[str, Any] = {"toolCall": tool_call, "outcome": outcome}
    if result is not None:
        params["result"] = result
    try:
        response = await host.request(
            "host.ssh.effectful_action_finished",
            params,
            timeout_seconds=8.0,
        )
    except asyncio.CancelledError:
        raise
    except Exception:
        raise ToolCallError(
            "host_unavailable", "the desktop host could not record the SSH action outcome"
        ) from None
    if not isinstance(response, dict) or response.get("status") != "success":
        error = response.get("error") if isinstance(response, dict) else None
        code = error.get("code") if isinstance(error, dict) else None
        message = error.get("message") if isinstance(error, dict) else None
        if not isinstance(code, str) or not re.fullmatch(r"[a-z][a-z0-9_]{0,63}", code):
            code = "invalid_host_response"
        raise ToolCallError(
            code,
            message if isinstance(message, str) else "the host rejected the SSH action completion record",
        )


async def _desktop_ssh_effectful_action(
    host: HostRpcClient,
    binary: Path,
    tool_name: str,
    arguments: dict[str, Any],
    context: ExecutionContext,
) -> dict[str, Any]:
    resource_key = {
        "systemd.restart": "service",
        "docker.restart": "container",
        "package.install": "package",
        "server.exec": "command",
    }[tool_name]
    resource = arguments.get(resource_key)
    tool_call = _ssh_effectful_action_host_call(tool_name, arguments, context)
    try:
        response = await host.request(
            "host.ssh.effectful_action_target",
            {"toolCall": tool_call},
            timeout_seconds=15.0,
        )
    except asyncio.CancelledError:
        raise
    except Exception:
        raise ToolCallError(
            "host_unavailable", "the desktop host could not authorize the SSH action target"
        ) from None
    if not isinstance(response, dict):
        raise ToolCallError("invalid_host_response", "the desktop host returned an invalid SSH action target")
    if response.get("status") == "already_completed":
        if tool_name == "server.exec":
            return _validate_server_exec_output(response.get("output"), arguments["maxOutputBytes"])
        if tool_name == "package.install":
            completed = {"manager": arguments["manager"], "package": resource, "installed": True}
            if "version" in arguments:
                completed["version"] = arguments["version"]
            return completed
        return {resource_key: resource, "restarted": True}
    if response.get("status") == "failed":
        error = response.get("error")
        code = error.get("code") if isinstance(error, dict) else None
        message = error.get("message") if isinstance(error, dict) else None
        if code == "unsupported_route":
            return await _execute_on_host(host, tool_name, arguments, context)
        if not isinstance(code, str) or not re.fullmatch(r"[a-z][a-z0-9_]{0,63}", code):
            code = "host_tool_failed"
        raise ToolCallError(
            code,
            message if isinstance(message, str) else "the desktop host rejected the SSH action request",
        )
    payload = response.get("targetData")
    if (
        response.get("status") != "success"
        or not isinstance(payload, dict)
    ):
        if response.get("status") == "success":
            await _finish_desktop_ssh_effectful_action(host, tool_call, "failed")
        raise ToolCallError("invalid_host_response", "the desktop host returned an invalid SSH action target")

    process = None
    outcome = "failed"
    operation_started = False
    cancelled = False
    try:
        process, token, base_url = SshServiceProcess.for_desktop_target(binary, payload)
        target = payload.get("target")
        target_id = context.context_target_id
        if not isinstance(target, dict) or target.get("target_id") != target_id:
            raise SshServiceProcessError("the desktop host returned the wrong SSH action target")
        await process.start()
        operation_started = True
        client = SshServiceClient(token, base_url=base_url)
        if tool_name == "systemd.restart":
            result = await asyncio.to_thread(
                client.systemd_restart,
                target_id,
                resource,
                arguments["timeoutSeconds"],
            )
        elif tool_name == "docker.restart":
            result = await asyncio.to_thread(
                client.docker_restart,
                target_id,
                resource,
                arguments["timeoutSeconds"],
            )
        elif tool_name == "package.install":
            result = await asyncio.to_thread(
                client.package_install,
                target_id,
                arguments["manager"],
                resource,
                arguments.get("version"),
                arguments["timeoutSeconds"],
            )
        else:
            result = await asyncio.to_thread(
                client.server_exec,
                target_id,
                arguments["command"],
                arguments["timeoutMs"],
                arguments["maxOutputBytes"],
                workdir=arguments.get("workdir"),
                env=arguments.get("env"),
            )
        outcome = "succeeded" if result.exit_code == 0 else "failed"
    except asyncio.CancelledError:
        cancelled = True
        if operation_started:
            outcome = "unknown"
        await asyncio.shield(_finish_desktop_ssh_effectful_action(host, tool_call, outcome))
        raise
    except SshServiceError as error:
        outcome = "unknown" if operation_started else "failed"
        await _finish_desktop_ssh_effectful_action(host, tool_call, outcome)
        raise ToolCallError(error.code, str(error)) from None
    except SshServiceProcessError:
        await _finish_desktop_ssh_effectful_action(host, tool_call, "failed")
        raise ToolCallError(
            "host_unavailable", "the C++ SSH service could not start for the approved action"
        ) from None
    except Exception:
        outcome = "unknown" if operation_started else "failed"
        await _finish_desktop_ssh_effectful_action(host, tool_call, outcome)
        raise ToolCallError(
            "transport", "the approved SSH action did not return a usable result"
        ) from None
    else:
        completion_result = None
        if tool_name == "server.exec":
            completion_result = {
                "state": "completed",
                "exitCode": result.exit_code,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "stdoutTruncated": result.stdout_truncated,
                "stderrTruncated": result.stderr_truncated,
                "durationMs": result.duration_ms,
            }
        await _finish_desktop_ssh_effectful_action(
            host, tool_call, outcome, completion_result
        )
        if result.exit_code != 0:
            raise ToolCallError(
                "execution_failed",
                f"the approved {tool_name} command exited with code {result.exit_code}",
                result=completion_result if tool_name == "server.exec" else None,
            )
        if tool_name == "server.exec":
            return _validate_server_exec_output(completion_result, arguments["maxOutputBytes"])
        if tool_name == "package.install":
            completed = {
                "manager": arguments["manager"],
                "package": resource,
                "installed": True,
            }
            if "version" in arguments:
                completed["version"] = arguments["version"]
            return completed
        return {resource_key: resource, "restarted": True}
    finally:
        if process is not None:
            try:
                await process.stop()
            except asyncio.CancelledError:
                if not cancelled:
                    raise
            except Exception:
                if not cancelled:
                    raise


def _validate_server_exec_output(value: object, max_output_bytes: int) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {
        "state", "exitCode", "stdout", "stderr", "stdoutTruncated", "stderrTruncated", "durationMs"
    }:
        raise ToolCallError("invalid_host_response", "the host returned an invalid server.exec result")
    exit_code = value.get("exitCode")
    stdout = value.get("stdout")
    stderr = value.get("stderr")
    duration_ms = value.get("durationMs")
    if (
        value.get("state") != "completed"
        or isinstance(exit_code, bool)
        or not isinstance(exit_code, int)
        or not 0 <= exit_code <= 255
        or not isinstance(stdout, str)
        or not isinstance(stderr, str)
        or not isinstance(value.get("stdoutTruncated"), bool)
        or not isinstance(value.get("stderrTruncated"), bool)
        or isinstance(duration_ms, bool)
        or not isinstance(duration_ms, int)
        or duration_ms < 0
        or len(stdout.encode("utf-8")) + len(stderr.encode("utf-8")) > max_output_bytes * 3
    ):
        raise ToolCallError("invalid_host_response", "the host returned an invalid server.exec result")
    return dict(value)


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
    ):
        raise ToolCallError("target_required", "filesystem.write requires a resolved remote server")

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
        "plan_id": context.plan_id or "",
        "plan_step_id": context.plan_step_id or "",
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
                **_host_plan_binding(context),
                "target": dict(target),
            },
            timeout_seconds=8.0,
        )
    except asyncio.CancelledError:
        raise
    except Exception:
        # The target resolver rejects routes the C++ service cannot represent; the host path
        # below is still backed by the same C++ SSH/SFTP bridge.
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


async def _desktop_filesystem_edit(
    host: HostRpcClient,
    binary: Path,
    arguments: dict[str, Any],
    context: ExecutionContext,
) -> dict[str, object]:
    """Run an approved revision-guarded file edit through C++ atomic SFTP replacement."""
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
    ):
        raise ToolCallError("target_required", "filesystem.edit requires a resolved remote server")

    ledger = FilesystemActionLedger.from_environment()
    if ledger is None:
        return await _execute_on_host(host, "filesystem.edit", arguments, context)
    ledger_scope = {
        "run_id": context.run_id,
        "trace_id": context.trace_id,
        "call_id": context.call_id,
        "tool_name": "filesystem.edit",
        "target_id": target_id,
        "arguments": arguments,
        "plan_id": context.plan_id or "",
        "plan_step_id": context.plan_step_id or "",
        "evidence_ids": context.evidence_ids,
    }
    try:
        cached = await ledger.claim(**ledger_scope)
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    if cached is not None:
        return cached

    async def run_host_compatibility_path() -> dict[str, object]:
        output = await _execute_on_host(host, "filesystem.edit", arguments, context)
        if not isinstance(output, dict):
            raise ToolCallError("invalid_host_response", "The desktop host returned an invalid file edit result")
        try:
            await ledger.complete(**ledger_scope, output=output)
        except FilesystemActionLedgerError as error:
            raise ToolCallError(error.code, str(error)) from None
        return output

    try:
        payload = await host.request(
            "host.ssh.filesystem_edit_target",
            {
                "serverId": target_id,
                **arguments,
                **_host_plan_binding(context),
                "target": dict(target),
            },
            timeout_seconds=8.0,
        )
    except asyncio.CancelledError:
        raise
    except Exception:
        return await run_host_compatibility_path()

    expected_binding = {
        "path": arguments["path"],
        "expectedRevision": arguments["expectedRevision"],
    }
    binding = payload.get("filesystemEdit") if isinstance(payload, dict) else None
    if binding != expected_binding:
        raise ToolCallError("invalid_host_response", "The desktop host returned an invalid edit binding")
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

    def edit_remote_file() -> dict[str, object]:
        client = SshServiceClient(token, base_url=base_url)
        before = client.sftp_read(target_id, arguments["path"], _MAX_AGENT_EDIT_BYTES)
        if len(before) > _MAX_AGENT_EDIT_BYTES:
            raise ToolCallError("invalid_input", "The remote file exceeds the 512 KiB edit limit")
        actual_revision = hashlib.sha256(before).hexdigest()
        if actual_revision != arguments["expectedRevision"]:
            raise ToolCallError(
                "invalid_input",
                "The remote file changed after it was read; read it again before editing",
            )

        metadata = client.sftp_stat(target_id, arguments["path"])
        if metadata["kind"] != "file":
            raise ToolCallError("unsafe_remote_write", "Only a regular remote file can be edited")
        if metadata["size"] != len(before):
            raise ToolCallError(
                "concurrent_change", "The remote file changed while its revision was being checked"
            )
        if client.sftp_link_count(target_id, arguments["path"]) != 1:
            raise ToolCallError(
                "unsafe_remote_write", "The server cannot prove this file has exactly one name"
            )

        old_bytes = arguments["oldString"].encode("utf-8")
        offset = before.find(old_bytes)
        occurrences = 0
        cursor = offset
        while cursor >= 0:
            occurrences += 1
            cursor = before.find(old_bytes, cursor + 1)
        if occurrences == 0:
            raise ToolCallError("invalid_input", "oldString was not found in the file; re-read it and retry")
        if occurrences != 1:
            raise ToolCallError(
                "invalid_input",
                f"oldString occurs {occurrences} times in the file; include enough context to make it unique",
            )

        updated = (
            before[:offset]
            + arguments["newString"].encode("utf-8")
            + before[offset + len(old_bytes):]
        )
        if len(updated) > _MAX_AGENT_EDIT_BYTES:
            raise ToolCallError("invalid_input", "The edited file would exceed the 512 KiB limit")
        result = client.sftp_replace_guarded(
            target_id,
            arguments["path"],
            updated,
            expected_size=len(before),
            expected_modified=metadata["modified"],
            expected_digest=actual_revision,
            timeout_seconds=25,
        )
        if result["size"] != len(updated):
            raise ToolCallError(
                "concurrent_change", "The published file does not match the replacement size"
            )
        return {
            "path": arguments["path"],
            "revision": hashlib.sha256(updated).hexdigest(),
            "bytesBefore": len(before),
            "bytesAfter": len(updated),
            "lineDelta": updated.count(b"\n") - before.count(b"\n"),
        }

    try:
        output = await asyncio.to_thread(edit_remote_file)
        await ledger.complete(**ledger_scope, output=output)
        return output
    except asyncio.CancelledError:
        # The guarded rename may have committed before cancellation; retain the unknown ledger row.
        raise
    except ToolCallError:
        raise
    except SshServiceError as error:
        # A transport failure can arrive after the atomic rename; do not retry automatically.
        code = "unsafe_remote_write" if error.code == "replace_unsupported" else error.code
        raise ToolCallError(code, str(error)) from None
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    finally:
        await process.stop()


async def _desktop_filesystem_backup(
    host: HostRpcClient,
    binary: Path,
    arguments: dict[str, Any],
    context: ExecutionContext,
) -> dict[str, object]:
    """Copy one stable, bounded remote file through C++ and register host ownership."""
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
    ):
        raise ToolCallError("target_required", "filesystem.backup requires a resolved remote server")

    ledger = FilesystemActionLedger.from_environment()
    if ledger is None:
        return await _execute_on_host(host, "filesystem.backup", arguments, context)
    ledger_scope = {
        "run_id": context.run_id,
        "trace_id": context.trace_id,
        "call_id": context.call_id,
        "tool_name": "filesystem.backup",
        "target_id": target_id,
        "arguments": arguments,
        "plan_id": context.plan_id or "",
        "plan_step_id": context.plan_step_id or "",
        "evidence_ids": context.evidence_ids,
    }
    try:
        cached = await ledger.claim(**ledger_scope)
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    if cached is not None:
        return cached

    async def run_host_compatibility_path() -> dict[str, object]:
        output = await _execute_on_host(host, "filesystem.backup", arguments, context)
        if not isinstance(output, dict):
            raise ToolCallError("invalid_host_response", "The desktop host returned an invalid backup result")
        try:
            await ledger.complete(**ledger_scope, output=output)
        except FilesystemActionLedgerError as error:
            raise ToolCallError(error.code, str(error)) from None
        return output

    try:
        payload = await host.request(
            "host.ssh.filesystem_backup_target",
            {
                "serverId": target_id,
                "path": arguments["path"],
                **_host_plan_binding(context),
                "target": dict(target),
            },
            timeout_seconds=8.0,
        )
    except asyncio.CancelledError:
        raise
    except Exception:
        return await run_host_compatibility_path()

    binding = payload.get("filesystemBackup") if isinstance(payload, dict) else None
    if (
        not isinstance(binding, dict)
        or binding.get("path") != arguments["path"]
        or not isinstance(binding.get("backupPath"), str)
    ):
        raise ToolCallError("invalid_host_response", "The desktop host returned an invalid backup binding")
    backup_path = binding["backupPath"]
    _validate_host_backup_path(arguments["path"], backup_path)
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

    def create_remote_backup() -> dict[str, object]:
        client = SshServiceClient(token, base_url=base_url)
        before = client.sftp_stat(target_id, arguments["path"])
        if before["kind"] != "file":
            raise ToolCallError("unsafe_remote_write", "Only a regular remote file can be backed up")
        if client.sftp_link_count(target_id, arguments["path"]) != 1:
            raise ToolCallError(
                "unsafe_remote_write", "The server cannot prove this file has exactly one name"
            )
        content = client.sftp_read(target_id, arguments["path"], _MAX_AGENT_BACKUP_BYTES)
        if len(content) > _MAX_AGENT_BACKUP_BYTES:
            raise ToolCallError("invalid_input", "The remote file exceeds the 1 MiB backup limit")
        after = client.sftp_stat(target_id, arguments["path"])
        if before["size"] != after["size"] or before["modified"] != after["modified"]:
            raise ToolCallError("concurrent_change", "The remote file changed while it was being backed up")
        if len(content) != before["size"]:
            raise ToolCallError("concurrent_change", "The remote file size changed while it was being read")
        revision = hashlib.sha256(content).hexdigest()
        result = client.sftp_create_exclusive(target_id, backup_path, content)
        if result.exit_code != 0:
            raise ToolCallError("execution_failed", "The C++ SSH service could not create the backup file")
        return {
            "path": arguments["path"],
            "backupPath": backup_path,
            "revision": revision,
            "bytesBackedUp": len(content),
        }

    try:
        output = await asyncio.to_thread(create_remote_backup)
        try:
            registration = await host.request(
                "host.investigation.filesystem_backup.register",
                {
                    "serverId": target_id,
                    "path": output["path"],
                    "backupPath": output["backupPath"],
                    "revision": output["revision"],
                    "bytesBackedUp": output["bytesBackedUp"],
                    **_host_plan_binding(context),
                    "traceId": context.trace_id,
                    "callId": context.call_id,
                    "target": dict(target),
                },
                timeout_seconds=8.0,
            )
        except asyncio.CancelledError:
            raise
        except Exception:
            raise ToolCallError(
                "backup_registration_unknown",
                f"The remote backup at {backup_path} was created, but host ledger registration could not be confirmed; preserve it and reconcile before restoring",
            ) from None
        if registration != {"registered": True, "backupPath": backup_path}:
            raise ToolCallError(
                "backup_registration_unknown",
                f"The remote backup at {backup_path} was created, but host ledger registration could not be confirmed; preserve it and reconcile before restoring",
            )
        await ledger.complete(**ledger_scope, output=output)
        return output
    except asyncio.CancelledError:
        # The exclusive remote create may have committed before cancellation; retain the unknown ledger row.
        raise
    except ToolCallError:
        raise
    except SshServiceError as error:
        raise ToolCallError(error.code, str(error)) from None
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    finally:
        await process.stop()


def _validate_host_backup_path(path: str, backup_path: str) -> None:
    parent = path.rsplit("/", 1)[0] or "/"
    source_fingerprint = hashlib.sha256(path.encode("utf-8")).hexdigest()[:16]
    backup_name_prefix = f".yukinal-backup-{source_fingerprint}-"
    backup_prefix = f"/{backup_name_prefix}" if parent == "/" else f"{parent}/{backup_name_prefix}"
    if (
        len(backup_path.encode("utf-8")) > 4096
        or re.fullmatch(re.escape(backup_prefix) + r"[a-f0-9]{32}", backup_path) is None
    ):
        raise ToolCallError("invalid_host_response", "The desktop host returned an invalid backup path")


def _bounded_cleanup_reason(value: str) -> str:
    readable = "".join(
        " " if ord(character) < 0x20 or ord(character) == 0x7F else character
        for character in value
    )
    return readable.encode("utf-8")[:256].decode("utf-8", errors="ignore")


async def _desktop_filesystem_restore(
    host: HostRpcClient,
    binary: Path,
    arguments: dict[str, Any],
    context: ExecutionContext,
) -> dict[str, object]:
    """Verify a host-owned backup and restore it with a guarded C++ SFTP replace."""
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
    ):
        raise ToolCallError("target_required", "filesystem.restore requires a resolved remote server")

    ledger = FilesystemActionLedger.from_environment()
    if ledger is None:
        return await _execute_on_host(host, "filesystem.restore", arguments, context)
    ledger_scope = {
        "run_id": context.run_id,
        "trace_id": context.trace_id,
        "call_id": context.call_id,
        "tool_name": "filesystem.restore",
        "target_id": target_id,
        "arguments": arguments,
        "plan_id": context.plan_id or "",
        "plan_step_id": context.plan_step_id or "",
        "evidence_ids": context.evidence_ids,
    }
    try:
        cached = await ledger.claim(**ledger_scope)
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    if cached is not None:
        return cached

    async def run_host_compatibility_path() -> dict[str, object]:
        output = await _execute_on_host(host, "filesystem.restore", arguments, context)
        if not isinstance(output, dict):
            raise ToolCallError("invalid_host_response", "The desktop host returned an invalid restore result")
        try:
            await ledger.complete(**ledger_scope, output=output)
        except FilesystemActionLedgerError as error:
            raise ToolCallError(error.code, str(error)) from None
        return output

    try:
        payload = await host.request(
            "host.ssh.filesystem_restore_target",
            {
                "serverId": target_id,
                **arguments,
                **_host_plan_binding(context),
                "target": dict(target),
            },
            timeout_seconds=8.0,
        )
    except asyncio.CancelledError:
        raise
    except Exception:
        return await run_host_compatibility_path()

    binding = payload.get("filesystemRestore") if isinstance(payload, dict) else None
    if (
        not isinstance(binding, dict)
        or binding.get("path") != arguments["path"]
        or binding.get("backupPath") != arguments["backupPath"]
        or binding.get("expectedRevision") != arguments["expectedRevision"]
        or not isinstance(binding.get("revision"), str)
        or re.fullmatch(r"[a-fA-F0-9]{64}", binding["revision"]) is None
        or isinstance(binding.get("bytesBackedUp"), bool)
        or not isinstance(binding.get("bytesBackedUp"), int)
        or not 0 <= binding["bytesBackedUp"] <= _MAX_AGENT_BACKUP_BYTES
    ):
        raise ToolCallError("invalid_host_response", "The desktop host returned an invalid restore binding")
    backup_path = binding["backupPath"]
    _validate_host_backup_path(arguments["path"], backup_path)
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

    def restore_remote_file() -> dict[str, object]:
        client = SshServiceClient(token, base_url=base_url)
        target_path = arguments["path"]
        target_before = client.sftp_stat(target_id, target_path)
        if target_before["kind"] != "file":
            raise ToolCallError("unsafe_remote_write", "Only a regular remote target can be restored")
        if client.sftp_link_count(target_id, target_path) != 1:
            raise ToolCallError(
                "unsafe_remote_write", "The server cannot prove the target has exactly one name"
            )
        target_bytes = client.sftp_read(target_id, target_path, _MAX_AGENT_BACKUP_BYTES)
        if len(target_bytes) > _MAX_AGENT_BACKUP_BYTES:
            raise ToolCallError("invalid_input", "The current target exceeds the 1 MiB restore limit")
        target_revision = hashlib.sha256(target_bytes).hexdigest()
        if target_revision != arguments["expectedRevision"]:
            raise ToolCallError(
                "invalid_input", "The target changed after it was read; inspect it again before restoring"
            )
        target_after = client.sftp_stat(target_id, target_path)
        if (
            target_before["size"] != target_after["size"]
            or target_before["modified"] != target_after["modified"]
            or target_before["size"] != len(target_bytes)
        ):
            raise ToolCallError("concurrent_change", "The target changed while its revision was checked")

        backup_before = client.sftp_stat(target_id, backup_path)
        if backup_before["kind"] != "file":
            raise ToolCallError("unsafe_remote_write", "The host-owned backup is not a regular file")
        if client.sftp_link_count(target_id, backup_path) != 1:
            raise ToolCallError(
                "unsafe_remote_write", "The server cannot prove the backup has exactly one name"
            )
        backup_bytes = client.sftp_read(target_id, backup_path, _MAX_AGENT_BACKUP_BYTES)
        if len(backup_bytes) > _MAX_AGENT_BACKUP_BYTES:
            raise ToolCallError("invalid_input", "The host-owned backup exceeds the 1 MiB restore limit")
        backup_after = client.sftp_stat(target_id, backup_path)
        if (
            backup_before["size"] != backup_after["size"]
            or backup_before["modified"] != backup_after["modified"]
            or backup_before["size"] != len(backup_bytes)
            or len(backup_bytes) != binding["bytesBackedUp"]
        ):
            raise ToolCallError("concurrent_change", "The host-owned backup changed while it was read")
        revision = hashlib.sha256(backup_bytes).hexdigest()
        if revision != binding["revision"].lower():
            raise ToolCallError("invalid_input", "The host-owned backup no longer matches its ledger revision")

        replaced = client.sftp_replace_guarded(
            target_id,
            target_path,
            backup_bytes,
            expected_size=len(target_bytes),
            expected_modified=target_before["modified"],
            expected_digest=target_revision,
            timeout_seconds=25,
        )
        if replaced["size"] != len(backup_bytes):
            raise ToolCallError("concurrent_change", "The restored target has an unexpected size")
        return {
            "path": target_path,
            "backupPath": backup_path,
            "revision": revision,
            "bytesBefore": len(target_bytes),
            "bytesAfter": len(backup_bytes),
        }

    try:
        output = await asyncio.to_thread(restore_remote_file)
        try:
            confirmation = await host.request(
                "host.investigation.filesystem_backup.restored",
                {
                    "serverId": target_id,
                    **output,
                    "expectedRevision": arguments["expectedRevision"],
                    **_host_plan_binding(context),
                    "traceId": context.trace_id,
                    "callId": context.call_id,
                    "target": dict(target),
                },
                timeout_seconds=8.0,
            )
        except asyncio.CancelledError:
            raise
        except Exception:
            raise ToolCallError(
                "restore_registration_unknown",
                f"The target was restored from {backup_path}, but host ledger consumption could not be confirmed; reconcile before restoring again",
            ) from None
        expected_confirmation = {
            "restored": True,
            **output,
        }
        if confirmation != expected_confirmation:
            raise ToolCallError(
                "restore_registration_unknown",
                f"The target was restored from {backup_path}, but host ledger consumption could not be confirmed; reconcile before restoring again",
            )
        await ledger.complete(**ledger_scope, output=output)
        return output
    except asyncio.CancelledError:
        # A guarded atomic replace may have committed before cancellation; never replay this call.
        raise
    except ToolCallError:
        raise
    except SshServiceError as error:
        code = "unsafe_remote_write" if error.code == "replace_unsupported" else error.code
        raise ToolCallError(code, str(error)) from None
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    finally:
        await process.stop()


async def _desktop_filesystem_backup_cleanup(
    host: HostRpcClient,
    binary: Path,
    arguments: dict[str, Any],
    context: ExecutionContext,
) -> dict[str, object]:
    """Verify exact host-owned backup bytes and remove them through C++ SFTP."""
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
    ):
        raise ToolCallError(
            "target_required", "filesystem.backup.cleanup requires a resolved remote server"
        )

    batch = "items" in arguments
    items = arguments["items"] if batch else [arguments]
    if batch:
        worst_case_output = {
            "items": [
                {
                    "path": item["path"],
                    "backupPath": item["backupPath"],
                    "outcome": "failed",
                    "reason": '"' * 256,
                }
                for item in items
            ],
            "partial": True,
        }
        if len(json.dumps(worst_case_output, ensure_ascii=False, separators=(",", ":")).encode("utf-8")) > 16 * 1024:
            raise ToolCallError(
                "output_too_large", "The cleanup batch paths exceed the durable result size limit"
            )
    ledger = FilesystemActionLedger.from_environment()
    if ledger is None:
        return await _execute_on_host(host, "filesystem.backup.cleanup", arguments, context)
    ledger_scope = {
        "run_id": context.run_id,
        "trace_id": context.trace_id,
        "call_id": context.call_id,
        "tool_name": "filesystem.backup.cleanup",
        "target_id": target_id,
        "arguments": arguments,
        "plan_id": context.plan_id or "",
        "plan_step_id": context.plan_step_id or "",
        "evidence_ids": context.evidence_ids,
    }
    try:
        cached = await ledger.claim(**ledger_scope)
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    if cached is not None:
        return cached

    async def run_host_compatibility_path() -> dict[str, object]:
        output = await _execute_on_host(host, "filesystem.backup.cleanup", arguments, context)
        if not isinstance(output, dict):
            raise ToolCallError("invalid_host_response", "The desktop host returned an invalid cleanup result")
        try:
            await ledger.complete(**ledger_scope, output=output)
        except FilesystemActionLedgerError as error:
            raise ToolCallError(error.code, str(error)) from None
        return output

    try:
        payload = await host.request(
            "host.ssh.filesystem_cleanup_target",
            {
                "serverId": target_id,
                "input": arguments,
                **_host_plan_binding(context),
                "runId": context.run_id,
                "traceId": context.trace_id,
                "callId": context.call_id,
                "target": dict(target),
            },
            timeout_seconds=8.0,
        )
    except asyncio.CancelledError:
        raise
    except Exception:
        return await run_host_compatibility_path()

    binding = payload.get("filesystemCleanup") if isinstance(payload, dict) else None
    bound_items = binding.get("items") if isinstance(binding, dict) else None
    if not isinstance(bound_items, list) or len(bound_items) != len(items):
        raise ToolCallError("invalid_host_response", "The desktop host returned an invalid cleanup binding")
    statuses: list[dict[str, Any]] = []
    for requested, bound in zip(items, bound_items):
        if (
            not isinstance(bound, dict)
            or bound.get("path") != requested.get("path")
            or bound.get("backupPath") != requested.get("backupPath")
            or bound.get("expectedRevision") != requested.get("expectedRevision")
        ):
            raise ToolCallError("invalid_host_response", "The desktop host returned a mismatched cleanup item")
        status = bound.get("status")
        if status == "ready":
            revision = bound.get("revision")
            byte_count = bound.get("bytesBackedUp")
            owner_task_id = bound.get("ownerTaskId")
            if (
                not isinstance(revision, str)
                or re.fullmatch(r"[a-fA-F0-9]{64}", revision) is None
                or revision.lower() != requested["expectedRevision"].lower()
                or isinstance(byte_count, bool)
                or not isinstance(byte_count, int)
                or not 0 <= byte_count <= _MAX_AGENT_BACKUP_BYTES
                or (owner_task_id is not None and (not isinstance(owner_task_id, str) or not owner_task_id))
                or (not batch and owner_task_id != context.host_task_id)
            ):
                raise ToolCallError("invalid_host_response", "The desktop host returned invalid cleanup metadata")
            _validate_host_backup_path(requested["path"], requested["backupPath"])
            statuses.append({
                "path": requested["path"],
                "backupPath": requested["backupPath"],
                "expectedRevision": requested["expectedRevision"],
                "status": "ready",
                "revision": revision,
                "bytesBackedUp": byte_count,
            })
        elif batch and status in {"skipped", "failed"}:
            reason = bound.get("reason")
            if not isinstance(reason, str) or not reason or len(reason) > 1024:
                raise ToolCallError("invalid_host_response", "The desktop host returned an invalid cleanup reason")
            statuses.append({
                "path": requested["path"],
                "backupPath": requested["backupPath"],
                "expectedRevision": requested["expectedRevision"],
                "status": status,
                "reason": reason,
            })
        else:
            raise ToolCallError("invalid_host_response", "The desktop host returned an invalid cleanup status")

    ready = [item for item in statuses if item["status"] == "ready"]
    if not batch and not ready:
        reason = statuses[0].get("reason", "The host backup is not available for cleanup")
        raise ToolCallError("cleanup_denied", str(reason))
    results: list[dict[str, object] | None] = [None] * len(items)
    for index, status in enumerate(statuses):
        if status["status"] != "ready":
            results[index] = {
                "path": status["path"],
                "backupPath": status["backupPath"],
                "outcome": status["status"],
                "reason": _bounded_cleanup_reason(status["reason"]),
            }

    if not ready:
        output = {
            "items": [result for result in results if result is not None],
            "partial": True,
        }
        try:
            await ledger.complete(**ledger_scope, output=output)
        except FilesystemActionLedgerError as error:
            raise ToolCallError(error.code, str(error)) from None
        return output

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

    client = SshServiceClient(token, base_url=base_url)

    def remove_verified_backup(item: dict[str, Any], binding_item: dict[str, Any]) -> dict[str, object]:
        backup_path = item["backupPath"]
        before = client.sftp_stat(target_id, backup_path)
        if before["kind"] != "file":
            raise ToolCallError("unsafe_remote_write", "Only a regular host-owned backup can be cleaned up")
        if before["size"] > _MAX_AGENT_BACKUP_BYTES:
            raise ToolCallError("invalid_input", "The host-owned backup exceeds the 1 MiB cleanup limit")
        if client.sftp_link_count(target_id, backup_path) != 1:
            raise ToolCallError(
                "unsafe_remote_write", "The server cannot prove the backup has exactly one name"
            )
        content = client.sftp_read(target_id, backup_path, _MAX_AGENT_BACKUP_BYTES)
        if len(content) > _MAX_AGENT_BACKUP_BYTES:
            raise ToolCallError("invalid_input", "The host-owned backup exceeds the 1 MiB cleanup limit")
        revision = hashlib.sha256(content).hexdigest()
        if revision != item["expectedRevision"].lower() or revision != binding_item["revision"].lower():
            raise ToolCallError("invalid_input", "The host-owned backup no longer matches its recorded revision")
        after = client.sftp_stat(target_id, backup_path)
        if (
            before["size"] != after["size"]
            or before["modified"] != after["modified"]
            or before["size"] != len(content)
            or len(content) != binding_item["bytesBackedUp"]
        ):
            raise ToolCallError("concurrent_change", "The host-owned backup changed while it was checked")
        try:
            removed = client.sftp_remove_file(target_id, backup_path, timeout_seconds=20)
        except SshServiceError as error:
            # The remote unlink may have completed before the transport stopped responding.
            raise ToolCallError(
                "cleanup_outcome_unknown",
                f"Cleanup of {backup_path} may have completed; reconcile before trying again",
            ) from error
        if removed.exit_code != 0:
            raise ToolCallError(
                "cleanup_failed",
                removed.stderr[:1024] or "The SSH service could not remove the backup",
            )
        return {
            "path": item["path"],
            "backupPath": backup_path,
            "revision": revision,
            "bytesDeleted": len(content),
        }

    try:
        for index, requested in enumerate(items):
            binding_item = statuses[index]
            if binding_item["status"] != "ready":
                continue
            try:
                deleted = await asyncio.to_thread(remove_verified_backup, requested, binding_item)
            except asyncio.CancelledError:
                # An in-flight SFTP unlink can complete after cancellation; keep the exact call fenced.
                raise
            except ToolCallError as error:
                if error.code == "cleanup_outcome_unknown":
                    raise
                if not batch:
                    raise
                results[index] = {
                    "path": requested["path"],
                    "backupPath": requested["backupPath"],
                    "outcome": "failed",
                    "reason": _bounded_cleanup_reason(str(error)),
                }
                continue
            except SshServiceError as error:
                if not batch:
                    raise ToolCallError(error.code, str(error)) from None
                results[index] = {
                    "path": requested["path"],
                    "backupPath": requested["backupPath"],
                    "outcome": "failed",
                    "reason": _bounded_cleanup_reason(str(error)),
                }
                continue

            try:
                confirmation = await host.request(
                    "host.investigation.filesystem_backup.deleted",
                    {
                        "serverId": target_id,
                        "input": arguments,
                        "item": requested,
                        **deleted,
                        **_host_plan_binding(context),
                        "traceId": context.trace_id,
                        "callId": context.call_id,
                        "target": dict(target),
                    },
                    timeout_seconds=8.0,
                )
            except asyncio.CancelledError:
                raise
            except Exception:
                raise ToolCallError(
                    "cleanup_registration_unknown",
                    f"The backup at {requested['backupPath']} was removed, but host ledger consumption could not be confirmed; reconcile before another cleanup",
                ) from None
            expected_confirmation = {"deleted": True, **deleted}
            if confirmation != expected_confirmation:
                raise ToolCallError(
                    "cleanup_registration_unknown",
                    f"The backup at {requested['backupPath']} was removed, but host ledger consumption could not be confirmed; reconcile before another cleanup",
                )
            if batch:
                results[index] = {
                    "path": requested["path"],
                    "backupPath": requested["backupPath"],
                    "outcome": "removed",
                    "revision": deleted["revision"],
                    "bytesDeleted": deleted["bytesDeleted"],
                }
            else:
                output = deleted
                try:
                    await ledger.complete(**ledger_scope, output=output)
                except FilesystemActionLedgerError as error:
                    raise ToolCallError(error.code, str(error)) from None
                return output

        output = {
            "items": [result for result in results if result is not None],
            "partial": any(result is None or result.get("outcome") != "removed" for result in results),
        }
        try:
            await ledger.complete(**ledger_scope, output=output)
        except FilesystemActionLedgerError as error:
            raise ToolCallError(error.code, str(error)) from None
        return output
    except asyncio.CancelledError:
        raise
    except ToolCallError:
        raise
    except SshServiceError as error:
        raise ToolCallError(error.code, str(error)) from None
    except FilesystemActionLedgerError as error:
        raise ToolCallError(error.code, str(error)) from None
    finally:
        await process.stop()


def _parse_disk_usage(raw: str) -> dict[str, object]:
    if not isinstance(raw, str) or len(raw) > 4096:
        raise ToolCallError("invalid_response", "The C++ SSH service returned invalid disk usage data")
    lines = [line.split() for line in raw.splitlines() if line.strip()]
    if len(lines) != 2 or len(lines[0]) < 6 or len(lines[1]) < 6:
        raise ToolCallError("invalid_response", "The C++ SSH service returned invalid disk usage data")
    header, row = lines
    if header[:5] != ["Filesystem", "1024-blocks", "Used", "Available", "Capacity"]:
        raise ToolCallError("invalid_response", "The C++ SSH service returned invalid disk usage data")
    if not row[4].endswith("%") or not row[5].startswith("/"):
        raise ToolCallError("invalid_response", "The C++ SSH service returned invalid disk usage data")
    try:
        blocks, used, available = (int(value) for value in row[1:4])
        percent = int(row[4][:-1])
    except ValueError:
        raise ToolCallError("invalid_response", "The C++ SSH service returned invalid disk usage data") from None
    if (
        any(value < 0 or value > 2**53 - 1 for value in (blocks, used, available))
        or not 0 <= percent <= 100
    ):
        raise ToolCallError("invalid_response", "The C++ SSH service returned invalid disk usage data")
    return {
        "filesystem": row[0][:256],
        "mount": " ".join(row[5:])[:512],
        "total_bytes": blocks * 1024,
        "used_bytes": used * 1024,
        "available_bytes": available * 1024,
        "usage_percent": percent,
    }


def _bounded_inventory_lines(raw: str, maximum_lines: int) -> tuple[list[str], bool]:
    lines: list[str] = []
    total_bytes = 0
    partial = False
    for line in raw.splitlines():
        stripped = line.strip()
        if stripped.startswith("__YUKINAL_SOURCE__="):
            continue
        if stripped.startswith("__YUKINAL_TRUNCATED__="):
            partial = partial or stripped == "__YUKINAL_TRUNCATED__=1"
            continue
        if stripped.startswith("__YUKINAL_STATUS__="):
            partial = True
            continue
        if not stripped:
            continue
        size = len(line.encode("utf-8")) + 1
        if len(lines) >= maximum_lines or total_bytes + size > _MAX_INVENTORY_OUTPUT_BYTES:
            partial = True
            continue
        lines.append(line)
        total_bytes += size
    return lines, partial


def _parse_service_inventory(raw: str, arguments: dict[str, Any]) -> dict[str, object]:
    source_marker = "__YUKINAL_SOURCE__="
    source_value = next(
        (line.strip()[len(source_marker):] for line in reversed(raw.splitlines()) if line.strip().startswith(source_marker)),
        None,
    )
    if source_value not in {"systemd", "docker", "unavailable"}:
        raise ToolCallError("execution_failed", "The C++ SSH service returned an invalid service inventory")

    inventory_lines, partial = _bounded_inventory_lines(raw, _MAX_SERVICE_INVENTORY_LINES)
    services: list[dict[str, str]] = []
    for raw_line in inventory_lines:
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
    elif partial:
        result["message"] = "仅显示部分服务（最多 200 项、60 KiB）；超出限制或过长的完整条目已省略。"
    return result


def _parse_server_logs(raw: str, arguments: dict[str, Any]) -> dict[str, object]:
    source_marker = "__YUKINAL_SOURCE__="
    source = next(
        (
            line.strip()[len(source_marker):]
            for line in reversed(raw.splitlines())
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

    inventory_lines, partial = _bounded_inventory_lines(raw, _MAX_SERVER_LOG_LINES)
    lines: list[dict[str, str]] = []
    for raw_line in inventory_lines:
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
    result: dict[str, object] = {"source": source, "lines": lines}
    if partial:
        result["message"] = "仅显示部分日志（最多 100 行、60 KiB）；超出限制或过长的完整行已省略。"
    return result


def _parse_process_inventory(raw: str) -> dict[str, object]:
    """Parse the fixed `ps` probe; malformed rows are skipped, never guessed."""
    source_marker = "__YUKINAL_SOURCE__="
    source = next(
        (line.strip()[len(source_marker):] for line in reversed(raw.splitlines())
         if line.strip().startswith(source_marker)),
        None,
    )
    if source not in {"ps", "busybox", "unavailable"}:
        raise ToolCallError("execution_failed", "The C++ SSH service returned an invalid process inventory")
    if source == "unavailable":
        return {"source": source, "processes": [], "message": "目标上没有可用的 ps 命令"}
    inventory_lines, partial = _bounded_inventory_lines(raw, _MAX_PROCESS_LINES * 4)
    processes: list[dict[str, object]] = []
    for raw_line in inventory_lines:
        line = raw_line.strip()
        if not line or line.startswith(source_marker):
            continue
        fields = line.split(None, 7)
        if len(fields) != 8:
            continue
        pid, ppid, user, cpu, memory, state, elapsed, command = fields
        if not (pid.isascii() and pid.isdigit() and ppid.isascii() and ppid.isdigit()
                and elapsed.isascii() and elapsed.isdigit()):
            continue
        try:
            cpu_percent, memory_percent = float(cpu), float(memory)
        except ValueError:
            continue
        if not (0 <= cpu_percent <= 100 * 1024 and 0 <= memory_percent <= 100):
            continue
        if not command.strip() or len(command) > 256:
            continue
        processes.append({
            "pid": int(pid),
            "parentPid": int(ppid),
            "user": user[:64],
            "cpuPercent": round(cpu_percent, 2),
            "memoryPercent": round(memory_percent, 2),
            "state": state[:16],
            "elapsedSeconds": int(elapsed),
            "command": command.strip(),
        })
        if len(processes) >= _MAX_PROCESS_LINES:
            partial = True
            break
    result: dict[str, object] = {"source": source, "processes": processes}
    if partial:
        result["message"] = f"仅显示 CPU 占用最高的 {_MAX_PROCESS_LINES} 个进程；其余已省略。"
    return result


def _parse_port_inventory(raw: str) -> dict[str, object]:
    """Parse `ss -lntup` or `netstat -lntup`; only listening sockets are reported."""
    source_marker = "__YUKINAL_SOURCE__="
    source = next(
        (line.strip()[len(source_marker):] for line in reversed(raw.splitlines())
         if line.strip().startswith(source_marker)),
        None,
    )
    if source not in {"ss", "netstat", "unavailable"}:
        raise ToolCallError("execution_failed", "The C++ SSH service returned an invalid port inventory")
    if source == "unavailable":
        return {"source": source, "listeners": [], "processInfoAvailable": False,
                "message": "目标上没有可用的 ss 或 netstat 命令"}
    inventory_lines, partial = _bounded_inventory_lines(raw, _MAX_PORT_LINES * 2)
    listeners: list[dict[str, object]] = []
    process_names: set[str] = set()
    for raw_line in inventory_lines:
        line = raw_line.strip()
        if not line or line.startswith(source_marker):
            continue
        fields = line.split()
        if not fields or fields[0] not in {"tcp", "tcp6", "udp", "udp6"}:
            continue
        protocol = "tcp" if fields[0].startswith("tcp") else "udp"
        # ss prints: State Recv-Q Send-Q Local:Port Peer:Port [Process]
        # netstat prints: Proto Recv-Q Send-Q Local Peer [State] [Process]
        local_index = 4 if source == "ss" else 3
        if len(fields) <= local_index:
            continue
        address, _, port = fields[local_index].rpartition(":")
        if not port.isascii() or not port.isdigit() or not 0 < int(port) < 65536:
            continue
        entry: dict[str, object] = {
            "protocol": protocol,
            "address": address.strip("[]")[:128] or "*",
            "port": int(port),
        }
        process = fields[-1] if source == "ss" else " ".join(fields[local_index + 1:])
        match = re.search(r'users:\(\("([^"]{1,128})"', process)
        if match is not None:
            entry["process"] = match.group(1)
            process_names.add(match.group(1))
        listeners.append(entry)
        if len(listeners) >= _MAX_PORT_LINES:
            partial = True
            break
    result: dict[str, object] = {
        "source": source,
        "listeners": listeners,
        # `-p` needs privilege; an empty process column is a permission fact,
        # not proof that no process owns the socket.
        "processInfoAvailable": bool(process_names),
    }
    if not process_names:
        result["message"] = "当前 SSH 用户无法读取监听端口的所属进程；端口与地址仍然有效。"
    if partial:
        result["message"] = f"仅显示前 {_MAX_PORT_LINES} 个监听端口；其余已省略。"
    return result


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
    line = lines[0]
    detected_manager = manager
    manager_prefix = "__YUKINAL_MANAGER__="
    if line.startswith(manager_prefix):
        detected_manager = line.removeprefix(manager_prefix)
        if detected_manager not in {"apt", "dnf", "yum", "unsupported"}:
            raise ToolCallError("execution_failed", "package inspect returned an invalid manager")
        if detected_manager == "unsupported":
            # The marker is the whole verdict; the target has no supported manager.
            raise ToolCallError("unsupported", f"no supported package manager is available on the target (requested {manager})")
        lines = lines[1:]
        if not lines:
            raise ToolCallError("execution_failed", "package inspect returned no status")
        if len(lines) > 1:
            raise ToolCallError("execution_failed", "package inspect returned unexpected extra fields")
        line = lines[0]
    if line == "unsupported":
        raise ToolCallError("unsupported", f"no supported package manager is available on the target (requested {manager}, detected {detected_manager})")
    if line == "absent":
        return {"manager": detected_manager, "package": package, "installed": False}
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
    return {"manager": detected_manager, "package": package, "installed": True, "version": version}


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
