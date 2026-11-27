"""The fixed, read-only probes the Agent may run on a host.

Why this exists
---------------
A health check used to cost three `server.exec` approvals. The live acceptance
run showed it: `server.info` already reported CPU, memory and disk, but it has no
swap field, no inode counts and no "does this path exist" question, so the Agent
fell back to `grep /proc/meminfo`, `df -i` and `test -e` — three destructive-risk
approvals for three reads.

The command text for every probe is a reviewed constant in
``yukinal_ssh/toolkit_catalog.json`` (``readProbes``). That same JSON file is
compiled into the C++ SSH service, so the Agent and the transport agree on one
list: the model supplies no shell text for any probe, and the SSH service
resolves the shell text from its own compiled copy rather than trusting the
request. There is nothing to inject into and no way to widen a probe with an
argument.

They are also *not* a general escape hatch. If a question cannot be answered by
one of these constants, the Agent still has `server.exec` and a person still
approves it. That is the intended fallback, not a gap to be filled one command at
a time: a growing probe list would become a second, less-reviewed shell.

Approval policy
---------------
These are registered as read-only (`risk_level="read"`, `requires_approval=False`)
because the command is a constant in the bundled catalog, not because the Agent
or the model claims it is safe. They never write, and both sides still check the
resolved target.
"""
from __future__ import annotations

from dataclasses import dataclass

from yukinal_ssh.toolkit_catalog import (
    READ_PROBE_COMMANDS,
    READ_PROBE_PATH_PREFIXES,
    READ_PROBE_SUMMARIES,
)


@dataclass(frozen=True)
class ReadCommand:
    """One fixed read. `command` never contains model-supplied text."""

    tool: str
    summary: str
    command: str
    #: The JSON schema the Agent may fill in. Every field must be validated by the
    #: handler before it is used, and no field may be spliced into a shell word.
    input_schema: dict[str, object]


#: Paths a caller may ask about. The allowlist is a directory prefix, and the
#: handler additionally refuses any path containing a shell metacharacter or a
#: `..` segment, so a future handler bug cannot become command injection.
EXISTS_ALLOWED_PREFIXES = READ_PROBE_PATH_PREFIXES["filesystem.exists"]

_EXISTS_TEMPLATE = READ_PROBE_COMMANDS["filesystem.exists"]

_PATH_INPUT_SCHEMA: dict[str, object] = {
    "type": "object",
    "properties": {
        "path": {
            "type": "string",
            "minLength": 1,
            "maxLength": 512,
            "pattern": "^/[^\\u0000-\\u001f\\\\]*$",
        },
    },
    "required": ["path"],
    "additionalProperties": False,
}

_NO_INPUT_SCHEMA: dict[str, object] = {
    "type": "object",
    "properties": {},
    "additionalProperties": False,
}


def _read_command(tool: str, summary: str, input_schema: dict[str, object]) -> ReadCommand:
    return ReadCommand(
        tool=tool,
        summary=summary or READ_PROBE_SUMMARIES.get(tool, tool),
        command=READ_PROBE_COMMANDS[tool],
        input_schema=input_schema,
    )


#: Commands whose output is small enough to return whole inside the tool budget.
READ_COMMANDS: tuple[ReadCommand, ...] = (
    _read_command("system.swap", READ_PROBE_SUMMARIES.get("system.swap", ""), dict(_NO_INPUT_SCHEMA)),
    _read_command(
        "filesystem.inodes", READ_PROBE_SUMMARIES.get("filesystem.inodes", ""), dict(_NO_INPUT_SCHEMA)
    ),
    _read_command(
        "filesystem.mounts", READ_PROBE_SUMMARIES.get("filesystem.mounts", ""), dict(_NO_INPUT_SCHEMA)
    ),
    _read_command("system.time", READ_PROBE_SUMMARIES.get("system.time", ""), dict(_NO_INPUT_SCHEMA)),
    _read_command(
        "filesystem.exists",
        "Report whether one path exists, printing only PRESENT or ABSENT.",
        dict(_PATH_INPUT_SCHEMA),
    ),
)


def exists_command(path: str) -> str:
    """Build the only dynamically-shaped command in this module.

    `path` reaches here already validated by the schema and by
    `validate_exists_path`. It is single-quoted, and the validator refuses quotes
    and metacharacters, so the result is still one fixed `test` invocation. The
    C++ SSH service performs the identical substitution from the same template.
    """
    return _EXISTS_TEMPLATE.replace("{path}", f"'{path}'")


def validate_exists_path(path: object) -> str:
    """Accept one canonical absolute path under an allowlisted prefix."""
    if not isinstance(path, str) or not path or len(path) > 512:
        raise ValueError("path must be a non-empty string of at most 512 characters")
    if not path.startswith("/"):
        raise ValueError("path must be absolute")
    if any(character in path for character in "'\"\\`$&|;<>*?()[]{}!\n\r\t "):
        raise ValueError("path must not contain whitespace, quotes or shell metacharacters")
    segments = path.split("/")
    if any(segment in {"..", "."} for segment in segments):
        raise ValueError("path must not contain relative segments")
    if not any(path == prefix[:-1] or path.startswith(prefix) for prefix in EXISTS_ALLOWED_PREFIXES):
        raise ValueError("path is outside the readable prefixes")
    return path
