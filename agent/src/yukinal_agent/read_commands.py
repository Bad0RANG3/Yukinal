"""The fixed, read-only JSON-RPC commands the Agent may run on a host.

Why this exists
---------------
A health check used to cost three `server.exec` approvals. The live acceptance
run showed it: `server.info` already reported CPU, memory and disk, but it has no
swap field, no inode counts and no "does this path exist" question, so the Agent
fell back to `grep /proc/meminfo`, `df -i` and `test -e` — three destructive-risk
approvals for three reads. Each `server.exec` also costs a human decision, a
one-shot ticket and roughly 1.3 s of round-trip overhead.

Every command below is a constant in this file. **The model supplies no shell
text for any of them**, so the safety argument is different from `server.exec`:
there is nothing to inject into, and the command cannot be widened by an
argument. Each one reads a file or runs a fixed program with a fixed argument
vector.

They are also *not* a general escape hatch. If a question cannot be answered by
one of these constants, the Agent still has `server.exec` and a person still
approves it. That is the intended fallback, not a gap to be filled one command at
a time: a growing command catalog would become a second, less-reviewed shell.

Approval policy
---------------
These are registered as read-only (`risk_level="read"`, `requires_approval=False`)
because the command is a constant here, not because the Agent or the model claims
it is safe. They never write, and the host still checks the resolved target.
"""
from __future__ import annotations

from dataclasses import dataclass


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
EXISTS_ALLOWED_PREFIXES = (
    "/etc/", "/opt/", "/srv/", "/var/log/", "/var/lib/", "/usr/local/",
    "/home/", "/root/", "/tmp/", "/run/",
)

#: Commands whose output is small enough to return whole inside the tool budget.
READ_COMMANDS: tuple[ReadCommand, ...] = (
    ReadCommand(
        tool="system.swap",
        summary=(
            "Read total, used and free swap from /proc/meminfo. The health snapshot has "
            "no swap field, and a host with no swap configured is a real finding."
        ),
        command="grep -E '^(SwapTotal|SwapFree|SwapCached):' /proc/meminfo",
        input_schema={"type": "object", "properties": {}, "additionalProperties": False},
    ),
    ReadCommand(
        tool="filesystem.inodes",
        summary=(
            "Read inode usage for every mounted filesystem with a fixed `df -i`. The health "
            "snapshot reports byte usage only, so inode exhaustion is invisible without this."
        ),
        command="df -i -x tmpfs -x devtmpfs -x squashfs",
        input_schema={"type": "object", "properties": {}, "additionalProperties": False},
    ),
    ReadCommand(
        tool="filesystem.mounts",
        summary=(
            "Read the real filesystem table with a fixed `df -hT`, excluding only virtual "
            "filesystems. The health snapshot lists its own collector's subset."
        ),
        command="df -hT -x tmpfs -x devtmpfs -x squashfs",
        input_schema={"type": "object", "properties": {}, "additionalProperties": False},
    ),
    ReadCommand(
        tool="system.time",
        summary=(
            "Read the target's current time and timezone with a fixed `date`. Use this instead "
            "of assuming the caller's clock when stamping anything on the host."
        ),
        command="date -Is",
        input_schema={"type": "object", "properties": {}, "additionalProperties": False},
    ),
    ReadCommand(
        tool="filesystem.exists",
        summary=(
            "Report whether one path exists, printing only PRESENT or ABSENT. Reads no file "
            "content. This is the read that a delete needs to verify itself without a shell."
        ),
        command="",  # built per call from a validated path; see the handler
        input_schema={
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
        },
    ),
)


def exists_command(path: str) -> str:
    """Build the only dynamically-shaped command in this module.

    `path` reaches here already validated by the schema and by
    `validate_exists_path`. It is single-quoted, and the validator refuses quotes
    and metacharacters, so the result is still one fixed `test` invocation.
    """
    return f"if [ -e '{path}' ]; then echo PRESENT; else echo ABSENT; fi"


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
