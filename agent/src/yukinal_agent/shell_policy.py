from __future__ import annotations

from typing import Any


def prepare_model_tool_arguments(name: str, arguments: dict[str, Any]) -> dict[str, Any]:
    """Apply fail-fast defaults to AI commands before review and authorization.

    The complete prepared command becomes the approval input. Nothing is changed
    after approval; direct/manual tool calls retain their existing contract.
    """
    command = arguments.get("command")
    if name != "server.exec" or not isinstance(command, str) or not command:
        return arguments
    prefix = "set -e\n"
    while command.startswith(prefix):
        command = command[len(prefix):]
    return {**arguments, "command": prefix + command}
