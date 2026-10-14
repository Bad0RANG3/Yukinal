from __future__ import annotations

import asyncio
from typing import Any

from yukinal_ssh import SshServiceClient

from .tools import ExecutionContext, ToolRegistry, ToolSpec


def register_ssh_tools(registry: ToolRegistry, client: SshServiceClient) -> None:
    async def service_status(arguments: dict[str, Any], _context: ExecutionContext) -> dict[str, object]:
        result = await asyncio.to_thread(
            client.service_status,
            arguments["target_id"],
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
            description="Read the active state of one allow-listed systemd service.",
            input_schema={
                "type": "object",
                "properties": {
                    "target_id": {"type": "string", "minLength": 1, "maxLength": 64},
                    "service": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,127}$"},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 30},
                },
                "required": ["target_id", "service"],
                "additionalProperties": False,
            },
            risk_level="read",
            requires_approval=False,
            handler=service_status,
            target_required=True,
        )
    )
