"""Action policy: which task kinds may run, and the fingerprint of one.

The desktop host turns facts into a decision through one place (ADR 0005): a risk
level comes from the tool, the command and the target, collapses into a permission
tier (``read``/``write``/``dangerous``), and a policy says what happens to that
tier (``auto``/``ask``/``deny``). This module is that same shape for the control
plane, using the same vocabulary as ``packages/shared/src/types/risk.ts`` so the
two layers cannot drift into two languages.

**The facts.** A kind's risk is, in order of precedence: what this tenant declared
for it, what the host declares for its own built-in tools, or — for anything left
over — the closed default. The host has two built-in sets, and both are mirrored
exactly: its plan-bound observations (``host_tool_is_plan_bound_observation``)
read and never change the target, and its named state-changing tools
(``host_tool_is_effectful``) always change it. Everything else, including every
``mcp.*`` kind, is something the control plane cannot resolve the way the host's
MCP supervisor can, so it defaults to ``high``; a tenant declares the kinds it
trusts instead.

**The decision.** The tier of that risk is looked up in the tenant's per-tier modes
(default: the production table). ``auto`` runs without an approval, ``ask`` needs
one, and ``deny`` is refused outright — a denial is absolute, so a task that was
authorised earlier is stopped when the policy changes under it.

**Which action an approval is for.** The host fingerprints the canonical tool
input and makes a decision apply only to the matching fingerprint, so an approval
cannot be replayed for a different action. Here the fingerprint is a bare
lowercase SHA-256 hex (the same shape ``content_revision_hex`` produces) over the
canonical ``{kind, nodeId, payload}`` — sorted keys, no whitespace, UTF-8 — so an
approval for one payload never clears a task with another.
"""

from __future__ import annotations

import hashlib
import json

# How long an approved decision stays usable, and how many tasks one decision
# may authorise. A decision that never lapses and can be spent any number of
# times is not an approval of an action, it is a standing grant.
DEFAULT_APPROVAL_TTL_SECONDS = 900
DEFAULT_APPROVAL_USES = 1
MAX_APPROVAL_USES = 100
MIN_APPROVAL_TTL_SECONDS = 60
MAX_APPROVAL_TTL_SECONDS = 86_400

# The shared risk vocabulary. Kept as literals rather than imported across the
# language boundary; `packages/shared/src/types/risk.ts` is the source of truth.
RISK_LEVELS = ("read", "low", "medium", "high", "critical")
PERMISSION_TIERS = ("read", "write", "dangerous")
TIER_MODES = ("auto", "ask", "deny")
ENVIRONMENTS = ("local", "development", "staging", "production", "unknown")


def risk_rank(level: str) -> int:
    return RISK_LEVELS.index(level)


def tier_of(level: str) -> str:
    """Collapse a risk level into the tier a policy is written against."""
    if level in ("read", "low"):
        return "read"
    if level == "medium":
        return "write"
    return "dangerous"


# The host's plan-bound observations (`host_tool_is_plan_bound_observation`).
# These read and never change the target.
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

# The host's built-in tools that change state by name (`host_tool_is_effectful`).
# The host decides these, so a tenant cannot relax them; a declaration would only
# suggest that it could.
ALWAYS_EFFECTFUL_TASK_KINDS = frozenset(
    {
        "docker.restart",
        "server.exec",
        "systemd.restart",
        "package.install",
        "filesystem.write",
        "filesystem.edit",
        "filesystem.backup",
        "filesystem.backup.cleanup",
        "filesystem.restore",
    }
)

# The host says only "effectful" for those, not how effectful, so they are reported
# at the conservative end rather than inventing a tier it does not have. Anything
# the host does not name at all is also this level: the closed default.
EFFECTFUL_RISK = "high"
UNKNOWN_RISK = "high"

# One entry per environment from the shared built-in policies. `local` and
# `production` ask for a write; `development` and `staging` let it through. A
# dangerous action always asks, and nothing is ever denied by default, because a
# control plane that silently refuses work is worse than one that queues a
# decision.
ENVIRONMENT_TIER_MODES: dict[str, dict[str, str]] = {
    "local": {"read": "auto", "write": "ask", "dangerous": "ask"},
    "development": {"read": "auto", "write": "auto", "dangerous": "ask"},
    "staging": {"read": "auto", "write": "auto", "dangerous": "ask"},
    "production": {"read": "auto", "write": "ask", "dangerous": "ask"},
    # An unknown environment must never be more permissive than production.
    "unknown": {"read": "auto", "write": "ask", "dangerous": "ask"},
}

DEFAULT_ENVIRONMENT = "production"


def default_tier_modes(environment: str) -> dict[str, str]:
    """The modes of an environment's built-in policy, as a fresh dict."""
    return dict(ENVIRONMENT_TIER_MODES.get(environment, ENVIRONMENT_TIER_MODES["unknown"]))


def builtin_risk(kind: str) -> str | None:
    """The host's declaration for one of its own tools, if it makes one."""
    if kind in READ_ONLY_TASK_KINDS:
        return "read"
    if kind in ALWAYS_EFFECTFUL_TASK_KINDS:
        return EFFECTFUL_RISK
    return None


def effective_risk(kind: str, declared: dict[str, str] | None = None) -> str:
    """What this kind *is*: the host's own declaration, else the tenant's, else closed.

    The host's always-effectful set comes first on purpose. Those nine tools are
    effectful *by name* in the host, so a declaration must not be able to lower
    one, and putting the rule here rather than only in the API's validation means
    it holds however the row got written.
    """
    if kind in ALWAYS_EFFECTFUL_TASK_KINDS:
        return EFFECTFUL_RISK
    if declared and kind in declared:
        return declared[kind]
    return builtin_risk(kind) or UNKNOWN_RISK


def action_mode(
    kind: str,
    *,
    declared: dict[str, str] | None = None,
    tier_modes: dict[str, str] | None = None,
) -> str:
    """``auto``, ``ask`` or ``deny`` for this kind under this policy.

    A kind the host always treats as effectful can never resolve to ``auto``:
    ``effective_risk`` pins it to ``high`` whatever a tenant declared, that is the
    dangerous tier, and ``validate_tier_modes`` refuses to make the dangerous tier
    ``auto``. Two independent guards, because this is the one thing that must not
    be configurable away.
    """
    modes = tier_modes or ENVIRONMENT_TIER_MODES[DEFAULT_ENVIRONMENT]
    return modes[tier_of(effective_risk(kind, declared))]


def requires_approval(
    kind: str,
    *,
    declared: dict[str, str] | None = None,
    tier_modes: dict[str, str] | None = None,
) -> bool:
    """Whether a task of this kind may only exist with an approved action.

    Fails closed: with no policy given, the production table applies, and an
    unknown kind is ``high`` risk, so it needs an approval.
    """
    return action_mode(kind, declared=declared, tier_modes=tier_modes) == "ask"


def validate_tier_modes(tier_modes: dict[str, str]) -> None:
    """Reject a tier table that is incomplete or names an unknown mode.

    ``dangerous: auto`` is refused on purpose. No built-in policy in the shared
    layer auto-approves that tier, and the control plane's own always-effectful
    kinds are all ``high``, so allowing it would be a way to configure away the
    approval requirement for exactly the actions that must keep one.
    """
    missing = [tier for tier in PERMISSION_TIERS if tier not in tier_modes]
    if missing:
        raise ValueError(f"tier modes are missing: {', '.join(missing)}")
    unknown = [tier for tier in tier_modes if tier not in PERMISSION_TIERS]
    if unknown:
        raise ValueError(f"unknown permission tier: {', '.join(unknown)}")
    for tier, mode in tier_modes.items():
        if mode not in TIER_MODES:
            raise ValueError(f"unknown mode `{mode}` for tier `{tier}`")
    if tier_modes["dangerous"] == "auto":
        raise ValueError("the dangerous tier cannot be set to `auto`")


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
