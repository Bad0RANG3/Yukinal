"""Domain operations.

Each function assumes an active session/transaction and writes exactly one audit
event per mutation. Tenant scoping is checked here, not only in the route, so a
cross-tenant identifier resolves to ``NotFound`` (404) rather than leaking that a
row exists elsewhere.
"""

from __future__ import annotations

import secrets
from datetime import datetime, timedelta, timezone

from sqlalchemy import delete, func, or_, select, update
from sqlalchemy.exc import IntegrityError
from sqlalchemy.ext.asyncio import AsyncSession

from . import actions, models, nodeauth
from .errors import (
    ApprovalRequired,
    Conflict,
    InvalidTransition,
    NotFound,
    Unauthorized,
)
from .models import (
    ACTIVE_TASK_STATUSES,
    APPROVAL_DECISIONS,
    APPROVAL_STATES,
    TASK_STATUSES,
)

# Allowed task transitions. A terminal state has no outgoing edges, so a
# completed task is never silently moved back to a live state. ``unknown`` can
# move to any *reported* outcome: the control plane never replays it, but the node
# that was running it may come back and state what actually happened.
_TRANSITIONS: dict[str, set[str]] = {
    "queued": {"running", "cancelled"},
    "running": {"succeeded", "failed", "cancelled", "unknown"},
    "unknown": {"running", "succeeded", "failed", "cancelled"},
    "succeeded": set(),
    "failed": set(),
    "cancelled": set(),
}


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


def new_id(prefix: str) -> str:
    return f"{prefix}_{secrets.token_hex(8)}"


async def record_audit(
    session: AsyncSession,
    *,
    tenant_id: str,
    actor: str,
    action: str,
    subject_type: str,
    subject_id: str,
    detail: dict | None = None,
) -> None:
    """Append an audit event. ``detail`` must already be free of secrets."""
    session.add(
        models.AuditEvent(
            tenant_id=tenant_id,
            actor=actor,
            action=action,
            subject_type=subject_type,
            subject_id=subject_id,
            detail=detail or {},
        )
    )


# ── tenants ──────────────────────────────────────────────────────────────────


async def create_tenant(session: AsyncSession, *, name: str, tenant_id: str | None = None) -> models.Tenant:
    tid = tenant_id or new_id("ten")
    if await session.get(models.Tenant, tid) is not None:
        raise Conflict(f"tenant `{tid}` already exists")
    tenant = models.Tenant(id=tid, name=name)
    session.add(tenant)
    await session.flush()
    await record_audit(
        session,
        tenant_id=tid,
        actor="control-plane",
        action="tenant.create",
        subject_type="tenant",
        subject_id=tid,
        detail={"name": name},
    )
    return tenant


async def require_tenant(session: AsyncSession, tenant_id: str) -> models.Tenant:
    tenant = await session.get(models.Tenant, tenant_id)
    if tenant is None or tenant.disabled:
        raise NotFound(f"tenant `{tenant_id}` was not found")
    return tenant


async def list_tenants(session: AsyncSession) -> list[models.Tenant]:
    return list((await session.scalars(select(models.Tenant).order_by(models.Tenant.id))).all())


# ── nodes ────────────────────────────────────────────────────────────────────


async def register_node(
    session: AsyncSession,
    *,
    tenant_id: str,
    name: str,
    public_key: str,
    agent_version: str | None = None,
) -> models.Node:
    await require_tenant(session, tenant_id)
    node = models.Node(
        id=new_id("nod"),
        tenant_id=tenant_id,
        name=name,
        public_key=public_key,
        agent_version=agent_version,
    )
    session.add(node)
    try:
        await session.flush()
    except IntegrityError as error:
        raise Conflict(f"a node named `{name}` already exists in this tenant") from error
    await record_audit(
        session,
        tenant_id=tenant_id,
        actor="control-plane",
        action="node.register",
        subject_type="node",
        subject_id=node.id,
        detail={"name": name},
    )
    return node


async def require_node(session: AsyncSession, tenant_id: str, node_id: str) -> models.Node:
    node = await session.get(models.Node, node_id)
    if node is None or node.tenant_id != tenant_id:
        raise NotFound(f"node `{node_id}` was not found")
    return node


async def list_nodes(session: AsyncSession, tenant_id: str) -> list[models.Node]:
    await require_tenant(session, tenant_id)
    return list(
        (
            await session.scalars(
                select(models.Node)
                .where(models.Node.tenant_id == tenant_id)
                .order_by(models.Node.created_at)
            )
        ).all()
    )


async def require_node_by_id(session: AsyncSession, node_id: str) -> models.Node:
    """Resolve a node from its own id, for a node-authenticated request.

    The tenant comes from the row, never from the request, so a node cannot claim
    to belong to a tenant it does not.
    """
    node = await session.get(models.Node, node_id)
    tenant = await session.get(models.Tenant, node.tenant_id) if node is not None else None
    if node is None or tenant is None or tenant.disabled:
        raise NotFound(f"node `{node_id}` was not found")
    return node


async def authenticate_node(
    session: AsyncSession,
    *,
    node_id: str | None,
    timestamp: str | None,
    nonce: str | None,
    signature: str | None,
    method: str,
    path: str,
    body: bytes = b"",
) -> models.Node:
    """Verify a node's Ed25519 request signature and burn its nonce.

    An unknown node, a stale timestamp, a bad signature and a replayed nonce all
    raise the same ``Unauthorized``: the endpoint never confirms which node ids
    exist, nor which half of the check failed.
    """
    if not (node_id and timestamp and nonce and signature):
        raise Unauthorized("node authentication headers are missing")
    node = await session.get(models.Node, node_id)
    tenant = await session.get(models.Tenant, node.tenant_id) if node is not None else None
    if node is None or tenant is None or tenant.disabled:
        raise Unauthorized("node authentication failed")
    if not nodeauth.timestamp_within_window(timestamp):
        raise Unauthorized("the signed timestamp is outside the accepted window")
    canonical = nodeauth.canonical_request(
        timestamp=timestamp, nonce=nonce, method=method, path=path, body=body
    )
    if not nodeauth.verify_signature(node.public_key, signature, canonical):
        raise Unauthorized("the request signature is not valid for this node")
    await claim_nonce(session, node_id=node.id, nonce=nonce)
    return node


async def claim_nonce(
    session: AsyncSession, *, node_id: str, nonce: str, now: datetime | None = None
) -> None:
    """Consume a request nonce, raising ``Unauthorized`` if it was already used."""
    stamp = now or utc_now()
    session.add(models.NodeNonce(node_id=node_id, nonce=nonce, at=stamp))
    try:
        await session.flush()
    except IntegrityError as error:
        raise Unauthorized("this request nonce has already been used") from error
    # Keep the table proportional to the signature window instead of growing.
    await session.execute(
        delete(models.NodeNonce).where(
            models.NodeNonce.at
            < stamp - timedelta(seconds=nodeauth.SIGNATURE_WINDOW_SECONDS * 2)
        )
    )


async def touch_node(
    session: AsyncSession, node: models.Node, *, agent_version: str | None = None
) -> None:
    node.last_seen_at = utc_now()
    if agent_version:
        node.agent_version = agent_version
    await session.flush()


async def _mark_running_unknown(
    session: AsyncSession, *, tenant_id: str, node_id: str, reason: str, now: datetime
) -> list[str]:
    """Move every ``running`` task on a node to outcome-unknown. Never a replay."""
    running = (
        await session.scalars(
            select(models.Task).where(
                models.Task.node_id == node_id, models.Task.status == "running"
            )
        )
    ).all()
    marked: list[str] = []
    for task in running:
        task.status = "unknown"
        task.outcome = reason
        task.updated_at = now
        marked.append(task.id)
        await record_audit(
            session,
            tenant_id=tenant_id,
            actor="node",
            action="task.unknown",
            subject_type="task",
            subject_id=task.id,
            detail={"reason": reason},
        )
    return marked


async def heartbeat(
    session: AsyncSession,
    *,
    tenant_id: str,
    node_id: str,
    agent_version: str | None = None,
    restarted: bool = False,
) -> tuple[models.Node, list[str]]:
    """Record a node heartbeat, marking running tasks unknown after a restart.

    A restarted node lost its in-memory queue, so a task that was ``running`` may
    or may not have taken effect remotely. The row becomes ``unknown`` and is
    never replayed automatically.
    """
    node = await require_node(session, tenant_id, node_id)
    now = utc_now()
    node.last_seen_at = now
    if agent_version:
        node.agent_version = agent_version
    marked: list[str] = []
    if restarted:
        marked = await _mark_running_unknown(
            session, tenant_id=tenant_id, node_id=node_id, reason="node_restarted", now=now
        )
    await session.flush()
    return node, marked


async def mark_node_disconnected(
    session: AsyncSession, node: models.Node, *, reason: str = "node_disconnected"
) -> list[str]:
    """Record that a node dropped its channel.

    Running work is outcome-unknown: the remote effect may or may not have
    happened, so nothing is replayed and the task is listed for a human decision.
    """
    now = utc_now()
    node.last_seen_at = now
    marked = await _mark_running_unknown(
        session, tenant_id=node.tenant_id, node_id=node.id, reason=reason, now=now
    )
    await session.flush()
    return marked


async def recoverable_tasks(session: AsyncSession, *, tenant_id: str, node_id: str) -> list[models.Task]:
    """Tasks whose outcome is unknown after a disconnect. Read-only: no replay."""
    node = await require_node(session, tenant_id, node_id)
    return await recoverable_tasks_for_node(session, node)


async def recoverable_tasks_for_node(session: AsyncSession, node: models.Node) -> list[models.Task]:
    return list(
        (
            await session.scalars(
                select(models.Task)
                .where(models.Task.node_id == node.id, models.Task.status == "unknown")
                .order_by(models.Task.created_at)
            )
        ).all()
    )


async def node_tasks(session: AsyncSession, node: models.Node) -> list[models.Task]:
    """Every task for one node, oldest first. Used by the node itself."""
    return list(
        (
            await session.scalars(
                select(models.Task)
                .where(models.Task.node_id == node.id)
                .order_by(models.Task.created_at)
            )
        ).all()
    )


# ── action approval binding ──────────────────────────────────────────────────
#
# An effectful task is only created, and only leaves the queue, when an approved
# decision matches its exact action: same tenant, same kind, same node, same
# payload. The fingerprint is what makes "same" exact, so an approval for one
# input can never dispatch another. A decision also has a life: it lapses after a
# while and can be withdrawn, so the task records which decision authorised it and
# the authorisation is rechecked at dispatch rather than trusted from creation.


def task_action_fingerprint(*, kind: str, node_id: str, payload: dict) -> str:
    """Fingerprint of a prospective task, without creating one."""
    return actions.action_fingerprint(kind=kind, node_id=node_id, payload=payload)


async def approval_ttl_seconds(session: AsyncSession, tenant_id: str) -> int:
    """How long an approval stays usable in this tenant.

    One default for now; this is the seam where a tenant-level policy lands.
    """
    return actions.DEFAULT_APPROVAL_TTL_SECONDS


def as_utc(value: datetime) -> datetime:
    """Treat a naive timestamp as UTC.

    SQLite has no timezone type, so the dialect returns the naive UTC wall clock
    it stored even for a ``timezone=True`` column, while PostgreSQL returns an
    aware value. Every writer here stores UTC, so the missing offset means UTC and
    normalizing is what makes a comparison between the two backends behave the
    same.
    """
    return value if value.tzinfo is not None else value.replace(tzinfo=timezone.utc)


def _decision_is_live(approval: models.Approval, now: datetime) -> bool:
    """Whether a decision may still authorise work.

    Note what is *not* checked: the use count. Creating a task spends a use, so a
    one-shot approval would fail its own dispatch check if this tested
    ``uses < max_uses``. Availability at creation is the separate test in
    ``available_approval``.
    """
    if approval.state != "approved":
        return False
    if approval.expires_at is None:
        return True
    return as_utc(approval.expires_at) > as_utc(now)


async def expire_stale_approvals(
    session: AsyncSession, *, tenant_id: str | None = None, now: datetime | None = None
) -> int:
    """Turn lapsed decisions into ``expired`` and return how many moved.

    Correctness never depends on this sweep: every check compares ``expires_at``
    with the clock itself, so an unswept row is still refused. The sweep exists so
    the stored state tells the truth, and it is idempotent, which is why it can be
    called on each gate check without cost once nothing is due.
    """
    now = now or utc_now()
    query = select(models.Approval.id).where(
        models.Approval.state == "approved",
        models.Approval.expires_at.is_not(None),
        models.Approval.expires_at <= now,
    )
    if tenant_id is not None:
        query = query.where(models.Approval.tenant_id == tenant_id)
    due = list((await session.scalars(query)).all())
    if not due:
        return 0
    rows = (
        await session.execute(
            update(models.Approval)
            .where(models.Approval.id.in_(due))
            .values(state="expired")
            .returning(models.Approval.id, models.Approval.tenant_id)
            .execution_options(synchronize_session=False)
        )
    ).all()
    for approval_id, row_tenant_id in rows:
        await record_audit(
            session,
            tenant_id=row_tenant_id,
            actor="control-plane",
            action="approval.expired",
            subject_type="approval",
            subject_id=approval_id,
            detail={},
        )
    return len(rows)


async def available_approval(
    session: AsyncSession, *, tenant_id: str, kind: str, node_id: str, payload: dict
) -> models.Approval | None:
    """The newest decision that may still authorise exactly this action.

    Availability includes the use count, unlike the dispatch check: a decision
    that has spent its uses cannot authorise a *new* task, while a task it already
    authorised may still run under it.
    """
    fingerprint = actions.action_fingerprint(kind=kind, node_id=node_id, payload=payload)
    now = utc_now()
    return await session.scalar(
        select(models.Approval)
        .where(
            models.Approval.tenant_id == tenant_id,
            models.Approval.action == kind,
            models.Approval.fingerprint == fingerprint,
            models.Approval.state == "approved",
            models.Approval.uses < models.Approval.max_uses,
            or_(
                models.Approval.expires_at.is_(None),
                models.Approval.expires_at > now,
            ),
        )
        .order_by(models.Approval.decided_at.desc())
        .limit(1)
    )


async def consume_approval(session: AsyncSession, approval_id: str) -> bool:
    """Spend one use of a decision, if it still has one left.

    The condition is in the ``WHERE`` clause for the same reason the task claim
    is: two requests that both saw a use available must not both spend it.
    """
    now = utc_now()
    spent = await session.scalar(
        update(models.Approval)
        .where(
            models.Approval.id == approval_id,
            models.Approval.state == "approved",
            models.Approval.uses < models.Approval.max_uses,
            or_(
                models.Approval.expires_at.is_(None),
                models.Approval.expires_at > now,
            ),
        )
        .values(uses=models.Approval.uses + 1)
        .returning(models.Approval.uses)
        .execution_options(synchronize_session=False)
    )
    return spent is not None


async def require_action_approval(
    session: AsyncSession, *, tenant_id: str, kind: str, node_id: str, payload: dict
) -> models.Approval | None:
    """The decision that authorises this action, or ``None`` if none is needed.

    Raises ``ApprovalRequired`` when the kind is effectful and nothing usable
    matches, so no caller has to reimplement the check.
    """
    if not actions.requires_approval(kind):
        return None
    await expire_stale_approvals(session, tenant_id=tenant_id)
    approval = await available_approval(
        session, tenant_id=tenant_id, kind=kind, node_id=node_id, payload=payload
    )
    if approval is None:
        fingerprint = actions.action_fingerprint(kind=kind, node_id=node_id, payload=payload)
        raise ApprovalRequired(
            f"task kind `{kind}` changes its target and needs an approved action; "
            f"request approval for fingerprint {fingerprint}"
        )
    return approval


async def task_authorisation_problem(
    session: AsyncSession, task: models.Task
) -> str | None:
    """Why this task may not start, or ``None`` if it may.

    Dispatch re-checks the decision the task recorded, so a decision that lapsed
    or was withdrawn between creation and dispatch stops the task instead of being
    honoured on a stale reading.
    """
    if not actions.requires_approval(task.kind):
        return None
    if task.approval_id is None:
        return "approval_required"
    approval = await session.get(models.Approval, task.approval_id)
    if approval is None:
        return "approval_missing"
    if approval.state != "approved":
        return f"approval_{approval.state}"
    if not _decision_is_live(approval, utc_now()):
        return "approval_expired"
    return None


async def require_task_authorisation(session: AsyncSession, task: models.Task) -> None:
    problem = await task_authorisation_problem(session, task)
    if problem is not None:
        raise ApprovalRequired(
            f"task `{task.id}` may not start: its authorising approval is not usable ({problem})"
        )


async def claim_queued_task(session: AsyncSession, task_id: str) -> bool:
    """Move one queued task to ``running`` if it is still queued.

    Returns whether this caller won the claim. The condition lives in the
    ``WHERE`` clause, so two workers that both selected the same queued task
    cannot both assign it: the loser's update matches no row. Reading the status
    into Python and writing it back would let both through, which is precisely
    the multi-worker race.
    """
    claimed = await session.scalar(
        update(models.Task)
        .where(models.Task.id == task_id, models.Task.status == "queued")
        .values(
            status="running",
            attempts=models.Task.attempts + 1,
            updated_at=utc_now(),
        )
        .returning(models.Task.id)
        .execution_options(synchronize_session=False)
    )
    return claimed is not None


async def assign_queued_tasks(
    session: AsyncSession, node: models.Node, *, actor: str = "control-plane"
) -> tuple[list[models.Task], list[dict]]:
    """Offer this node its queued work, returning ``(assigned, waiting)``.

    Assignment is a conditional update, so a second worker that saw the same
    queued row loses the race instead of handing one task to two connections. A
    task whose authorising decision is missing, lapsed or withdrawn is reported in
    ``waiting`` with the reason rather than silently dropped, so a stalled queue is
    visible instead of looking idle.
    """
    candidates = (
        await session.scalars(
            select(models.Task)
            .where(models.Task.node_id == node.id, models.Task.status == "queued")
            .order_by(models.Task.created_at)
        )
    ).all()
    assigned: list[models.Task] = []
    waiting: list[dict] = []
    for task in candidates:
        problem = await task_authorisation_problem(session, task)
        if problem is not None:
            waiting.append(
                {
                    "taskId": task.id,
                    "reason": problem,
                    "fingerprint": actions.action_fingerprint(
                        kind=task.kind, node_id=node.id, payload=task.payload or {}
                    ),
                }
            )
            continue
        if not await claim_queued_task(session, task.id):
            # Another worker claimed it between our select and our update.
            continue
        assigned.append(await session.get(models.Task, task.id, populate_existing=True))
        await record_audit(
            session,
            tenant_id=task.tenant_id,
            actor=actor,
            action="task.transition",
            subject_type="task",
            subject_id=task.id,
            detail={"from": "queued", "to": "running", "outcome": None},
        )
    await session.flush()
    return assigned, waiting


# ── tasks ────────────────────────────────────────────────────────────────────


async def create_task(
    session: AsyncSession,
    *,
    tenant_id: str,
    node_id: str,
    kind: str,
    payload: dict,
) -> models.Task:
    await require_node(session, tenant_id, node_id)
    # An effectful task may not even be queued without its approval. The queue
    # means "ready to dispatch": a task that can never run would otherwise hold
    # the node's single active slot and stall it.
    approval = await require_action_approval(
        session, tenant_id=tenant_id, kind=kind, node_id=node_id, payload=payload
    )
    if approval is not None and not await consume_approval(session, approval.id):
        # The decision lapsed or ran out between the lookup and the claim. The
        # conditional update, not the lookup, is what decides.
        raise ApprovalRequired(
            f"approval `{approval.id}` has no use left for this action"
        )
    task = models.Task(
        id=new_id("tsk"),
        tenant_id=tenant_id,
        node_id=node_id,
        kind=kind,
        approval_id=approval.id if approval is not None else None,
        payload=payload,
    )
    session.add(task)
    try:
        await session.flush()
    except IntegrityError as error:
        raise Conflict("this node already has an active task") from error
    await record_audit(
        session,
        tenant_id=tenant_id,
        actor="control-plane",
        action="task.create",
        subject_type="task",
        subject_id=task.id,
        detail={
            "kind": kind,
            "nodeId": node_id,
            "fingerprint": task.fingerprint,
            "approvalId": approval.id if approval is not None else None,
        },
    )
    return task


async def require_task(session: AsyncSession, tenant_id: str, task_id: str) -> models.Task:
    task = await session.get(models.Task, task_id)
    if task is None or task.tenant_id != tenant_id:
        raise NotFound(f"task `{task_id}` was not found")
    return task


async def require_node_task(session: AsyncSession, node: models.Node, task_id: str) -> models.Task:
    """A node may only act on its own tasks, even inside one tenant."""
    task = await session.get(models.Task, task_id)
    if task is None or task.node_id != node.id:
        raise NotFound(f"task `{task_id}` was not found")
    return task


async def list_tasks(
    session: AsyncSession, tenant_id: str, *, node_id: str | None = None
) -> list[models.Task]:
    await require_tenant(session, tenant_id)
    query = select(models.Task).where(models.Task.tenant_id == tenant_id)
    if node_id is not None:
        query = query.where(models.Task.node_id == node_id)
    return list((await session.scalars(query.order_by(models.Task.created_at))).all())


async def _apply_transition(
    session: AsyncSession,
    task: models.Task,
    *,
    status: str,
    outcome: str | None,
    result: dict | None,
    actor: str,
) -> models.Task:
    if status not in TASK_STATUSES:
        raise Conflict(f"unknown task status `{status}`")
    if status not in _TRANSITIONS[task.status]:
        raise InvalidTransition(
            f"task `{task.id}` cannot move from `{task.status}` to `{status}`"
        )
    previous = task.status
    if status == "running" and previous == "queued":
        # Re-checked here as well as at creation: dispatch is the moment the node
        # is told to act, so the gate belongs on this edge too. The check follows
        # the decision the task recorded, which catches one that lapsed or was
        # withdrawn in between.
        await require_task_authorisation(session, task)
    task.status = status
    task.updated_at = utc_now()
    if status == "running":
        task.attempts += 1
    if outcome is not None:
        task.outcome = outcome
    if result is not None:
        task.result = result
    await session.flush()
    await record_audit(
        session,
        tenant_id=task.tenant_id,
        actor=actor,
        action="task.transition",
        subject_type="task",
        subject_id=task.id,
        detail={"from": previous, "to": status, "outcome": outcome},
    )
    return task


async def transition_task(
    session: AsyncSession,
    *,
    tenant_id: str,
    task_id: str,
    status: str,
    outcome: str | None = None,
    result: dict | None = None,
) -> models.Task:
    task = await require_task(session, tenant_id, task_id)
    return await _apply_transition(
        session, task, status=status, outcome=outcome, result=result, actor="control-plane"
    )


async def transition_node_task(
    session: AsyncSession,
    *,
    node: models.Node,
    task_id: str,
    status: str,
    outcome: str | None = None,
    result: dict | None = None,
) -> models.Task:
    """A node reports the outcome of one of its own tasks."""
    task = await require_node_task(session, node, task_id)
    return await _apply_transition(
        session, task, status=status, outcome=outcome, result=result, actor=node.id
    )


# ── model configs ────────────────────────────────────────────────────────────


async def create_model_config(
    session: AsyncSession,
    *,
    tenant_id: str,
    name: str,
    provider: str,
    model: str,
    api_key_ref: str | None = None,
) -> models.ModelConfig:
    await require_tenant(session, tenant_id)
    config = models.ModelConfig(
        id=new_id("mcf"),
        tenant_id=tenant_id,
        name=name,
        provider=provider,
        model=model,
        api_key_ref=api_key_ref,
    )
    session.add(config)
    try:
        await session.flush()
    except IntegrityError as error:
        raise Conflict(f"a model config named `{name}` already exists in this tenant") from error
    await record_audit(
        session,
        tenant_id=tenant_id,
        actor="control-plane",
        action="model_config.create",
        subject_type="model_config",
        subject_id=config.id,
        detail={
            "name": name,
            "provider": provider,
            "model": model,
            # The reference name only; the secret itself never reaches here.
            "hasKeyRef": api_key_ref is not None,
        },
    )
    return config


async def list_model_configs(session: AsyncSession, tenant_id: str) -> list[models.ModelConfig]:
    await require_tenant(session, tenant_id)
    return list(
        (
            await session.scalars(
                select(models.ModelConfig)
                .where(models.ModelConfig.tenant_id == tenant_id)
                .order_by(models.ModelConfig.created_at)
            )
        ).all()
    )


# ── approvals ────────────────────────────────────────────────────────────────


async def create_approval(
    session: AsyncSession,
    *,
    tenant_id: str,
    action: str,
    node_id: str,
    payload: dict,
    requested_by: str,
    task_id: str | None = None,
    max_uses: int = actions.DEFAULT_APPROVAL_USES,
) -> models.Approval:
    """Record a request to approve exactly one action.

    The fingerprint is computed here from the action, node and payload, so the
    approval can only ever authorise the input it names; a caller cannot name a
    fingerprint that does not match what they asked for. The requested use count
    is visible on the approval, so the decider sees what a grant would mean.
    """
    await require_node(session, tenant_id, node_id)
    if task_id is not None:
        await require_task(session, tenant_id, task_id)
    fingerprint = actions.action_fingerprint(kind=action, node_id=node_id, payload=payload)
    approval = models.Approval(
        id=new_id("apr"),
        tenant_id=tenant_id,
        task_id=task_id,
        node_id=node_id,
        payload=payload,
        action=action,
        fingerprint=fingerprint,
        requested_by=requested_by,
        max_uses=max_uses,
    )
    session.add(approval)
    await session.flush()
    await record_audit(
        session,
        tenant_id=tenant_id,
        actor=requested_by,
        action="approval.request",
        subject_type="approval",
        subject_id=approval.id,
        detail={
            "action": action,
            "nodeId": node_id,
            "taskId": task_id,
            "fingerprint": fingerprint,
            "maxUses": max_uses,
        },
    )
    return approval


async def decide_approval(
    session: AsyncSession,
    *,
    tenant_id: str,
    approval_id: str,
    decision: str,
    decided_by: str,
    max_uses: int | None = None,
) -> models.Approval:
    if decision not in APPROVAL_DECISIONS:
        raise Conflict(f"unknown approval decision `{decision}`")
    approval = await session.get(models.Approval, approval_id)
    if approval is None or approval.tenant_id != tenant_id:
        raise NotFound(f"approval `{approval_id}` was not found")
    if approval.state != "pending":
        raise Conflict(f"approval `{approval_id}` is already {approval.state}")
    if max_uses is not None:
        # A decider may narrow a request but never widen it: the request is what
        # the requester is on the hook for, and only the decider can reduce it.
        if max_uses > approval.max_uses:
            raise Conflict(
                f"a decision cannot raise the use count from {approval.max_uses} to {max_uses}"
            )
        approval.max_uses = max_uses
    now = utc_now()
    approval.state = decision
    approval.decided_by = decided_by
    approval.decided_at = now
    if decision == "approved":
        # A decision without an end is a standing grant, so approving always sets
        # one. The clock starts when the decision is made, not when it was asked.
        approval.expires_at = now + timedelta(
            seconds=await approval_ttl_seconds(session, tenant_id)
        )
    await session.flush()
    await record_audit(
        session,
        tenant_id=tenant_id,
        actor=decided_by,
        action=f"approval.{decision}",
        subject_type="approval",
        subject_id=approval.id,
        detail={
            "action": approval.action,
            "maxUses": approval.max_uses,
            "expiresAt": approval.expires_at.isoformat() if approval.expires_at else None,
        },
    )
    return approval


async def revoke_approval(
    session: AsyncSession, *, tenant_id: str, approval_id: str, revoked_by: str
) -> models.Approval:
    """Withdraw a decision, whether it is still pending or already approved.

    Withdrawing an approved decision does not touch the task it authorised: the
    task keeps its record and the dispatch check refuses it, which is the honest
    outcome — the control plane never claims work was un-authorised. Both cases
    end in ``revoked``; they differ only in the audit detail, because a decision
    that was never made cannot be un-made.
    """
    approval = await session.get(models.Approval, approval_id)
    if approval is None or approval.tenant_id != tenant_id:
        raise NotFound(f"approval `{approval_id}` was not found")
    if approval.state not in ("pending", "approved"):
        raise Conflict(f"approval `{approval_id}` is already {approval.state}")
    previous = approval.state
    approval.state = "revoked"
    approval.revoked_by = revoked_by
    approval.revoked_at = utc_now()
    await session.flush()
    await record_audit(
        session,
        tenant_id=tenant_id,
        actor=revoked_by,
        action="approval.revoke",
        subject_type="approval",
        subject_id=approval.id,
        detail={"from": previous, "action": approval.action},
    )
    return approval


async def list_approvals(session: AsyncSession, tenant_id: str) -> list[models.Approval]:
    await require_tenant(session, tenant_id)
    # Sweep first, so the list shows `expired` rather than a stale `approved` for
    # a decision that lapsed some time ago.
    await expire_stale_approvals(session, tenant_id=tenant_id)
    await session.flush()
    return list(
        (
            await session.scalars(
                select(models.Approval)
                .where(models.Approval.tenant_id == tenant_id)
                .order_by(models.Approval.requested_at)
            )
        ).all()
    )


# ── audit and metrics ────────────────────────────────────────────────────────


async def list_audit(session: AsyncSession, tenant_id: str, *, limit: int = 200) -> list[models.AuditEvent]:
    await require_tenant(session, tenant_id)
    return list(
        (
            await session.scalars(
                select(models.AuditEvent)
                .where(models.AuditEvent.tenant_id == tenant_id)
                .order_by(models.AuditEvent.id.desc())
                .limit(limit)
            )
        ).all()
    )


async def metrics(session: AsyncSession) -> dict:
    tenants = await session.scalar(select(func.count()).select_from(models.Tenant))
    nodes = await session.scalar(select(func.count()).select_from(models.Node))
    rows = await session.execute(
        select(models.Task.status, func.count()).group_by(models.Task.status)
    )
    tasks_by_status = {status: count for status, count in rows.all()}
    pending = await session.scalar(
        select(func.count())
        .select_from(models.Approval)
        .where(models.Approval.state == "pending")
    )
    approval_rows = await session.execute(
        select(models.Approval.state, func.count()).group_by(models.Approval.state)
    )
    return {
        "tenants": int(tenants or 0),
        "nodes": int(nodes or 0),
        "tasks_by_status": tasks_by_status,
        # Stored state only. A decision that has lapsed is reported as expired once
        # a sweep reaches it, which the approval listing does.
        "approvals_by_state": {state: count for state, count in approval_rows.all()},
        "pending_approvals": int(pending or 0),
    }


# Re-exported so callers do not import the model module for constants.
__all__ = [
    "ACTIVE_TASK_STATUSES",
    "APPROVAL_DECISIONS",
    "APPROVAL_STATES",
    "TASK_STATUSES",
    "approval_ttl_seconds",
    "assign_queued_tasks",
    "authenticate_node",
    "available_approval",
    "claim_nonce",
    "claim_queued_task",
    "consume_approval",
    "create_approval",
    "create_model_config",
    "create_task",
    "create_tenant",
    "decide_approval",
    "expire_stale_approvals",
    "heartbeat",
    "list_approvals",
    "list_audit",
    "list_model_configs",
    "list_nodes",
    "list_tasks",
    "list_tenants",
    "mark_node_disconnected",
    "metrics",
    "new_id",
    "node_tasks",
    "record_audit",
    "recoverable_tasks",
    "recoverable_tasks_for_node",
    "register_node",
    "require_action_approval",
    "require_node",
    "require_node_by_id",
    "require_node_task",
    "require_task",
    "require_task_authorisation",
    "require_tenant",
    "revoke_approval",
    "task_action_fingerprint",
    "task_authorisation_problem",
    "touch_node",
    "transition_node_task",
    "transition_task",
    "utc_now",
]
