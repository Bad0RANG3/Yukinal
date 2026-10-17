"""Action binding: which task kinds need an approval, and the fingerprint of one.

Two rules from the desktop host are mirrored here, because the control plane
brokers the same tools the host runs.

**Which kinds are effectful.** The host names its state-changing tools
explicitly and treats an MCP tool whose risk it cannot resolve as effectful
(``host_tool_is_effectful``, ADR 0014/0074). The control plane cannot resolve an
MCP tool's risk at all, so it inverts the test: a kind needs approval *unless* it
is one of the host's plan-bound observations. An unrecognised kind is therefore
effectful, which is the closed default rather than an open one.

**Which action the approval is for.** The host fingerprints the canonical tool
input and makes a decision apply only to the matching fingerprint, so an approval
cannot be replayed for a different action. Here the fingerprint is a bare
lowercase SHA-256 hex (the same shape ``content_revision_hex`` produces) over the
canonical ``{kind, nodeId, payload}`` — sorted keys, no whitespace, UTF-8 — so an
approval for one payload never clears a task with another.
"""

from __future__ import annotations

import hashlib
import json

# The host's plan-bound observations (`host_tool_is_plan_bound_observation`).
# These read and never change the target, so they run without an approval.
READ_ONLY_TASK_KINDS = frozenset(
    {
        "server.info",
        "server.logs",
        "server.services",
        "docker.ps",
        "docker.logs",
        "docker.inspect",
        "systemd.inspect",
        "package.inspect",
        "filesystem.read",
        "filesystem.backup.list",
        "filesystem.backup.retention",
    }
)


def requires_approval(kind: str) -> bool:
    """Whether a task of this kind may only exist with an approved action.

    Fails closed: an unknown kind is effectful. The host's own dispatcher is the
    closed set of real tool names; the control plane has no such set, so the
    allow-list of observations is what it trusts.
    """
    return kind not in READ_ONLY_TASK_KINDS


def canonical_action(*, kind: str, node_id: str, payload: dict) -> str:
    """The exact bytes an approval and a task must agree on.

    ``node_id`` is part of it: the same command on two nodes is two actions.
    """
    return json.dumps(
        {"kind": kind, "nodeId": node_id, "payload": payload},
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    )


def action_fingerprint(*, kind: str, node_id: str, payload: dict) -> str:
    """Lowercase SHA-256 hex of the canonical action."""
    canonical = canonical_action(kind=kind, node_id=node_id, payload=payload)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()
