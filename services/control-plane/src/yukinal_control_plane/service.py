"""Domain operations.

Each function assumes an active session/transaction and writes exactly one audit
event per mutation. Tenant scoping is checked here, not only in the route, so a
cross-tenant identifier resolves to ``NotFound`` (404) rather than leaking that a
row exists elsewhere.
"""

from __future__ import annotations

import secrets
from datetime import datetime, timezone

from sqlalchemy import func, select
from sqlalchemy.exc import IntegrityError
from sqlalchemy.ext.asyncio import AsyncSession

from . import models
from .errors import Conflict, InvalidTransition, NotFound
from .models import ACTIVE_TASK_STATUSES, APPROVAL_STATES, TASK_STATUSES

# Allowed task transitions. A terminal state has no outgoing edges, so a
# completed task is never silently moved back to a live state.
_TRANSITIONS: dict[str, set[str]] = {
    "queued": {"running", "cancelled"},
    "running": {"succeeded", "failed", "cancelled", "unknown"},
    "unknown": {"running", "failed", "cancelled"},
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
        running = (
            await session.scalars(
                select(models.Task).where(
                    models.Task.node_id == node_id, models.Task.status == "running"
                )
            )
        ).all()
        for task in running:
            task.status = "unknown"
            task.outcome = "node_restarted"
            task.updated_at = now
            marked.append(task.id)
            await record_audit(
                session,
                tenant_id=tenant_id,
                actor="node",
                action="task.unknown",
                subject_type="task",
                subject_id=task.id,
                detail={"reason": "node_restarted"},
            )
    await session.flush()
    return node, marked


async def recoverable_tasks(session: AsyncSession, *, tenant_id: str, node_id: str) -> list[models.Task]:
    """Tasks whose outcome is unknown after a disconnect. Read-only: no replay."""
    node = await require_node(session, tenant_id, node_id)
    return list(
        (
            await session.scalars(
                select(models.Task)
                .where(models.Task.node_id == node.id, models.Task.status == "unknown")
                .order_by(models.Task.created_at)
            )
        ).all()
    )


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
    task = models.Task(
        id=new_id("tsk"),
        tenant_id=tenant_id,
        node_id=node_id,
        kind=kind,
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
        detail={"kind": kind, "nodeId": node_id},
    )
    return task


async def require_task(session: AsyncSession, tenant_id: str, task_id: str) -> models.Task:
    task = await session.get(models.Task, task_id)
    if task is None or task.tenant_id != tenant_id:
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
    if status not in TASK_STATUSES:
        raise Conflict(f"unknown task status `{status}`")
    if status not in _TRANSITIONS[task.status]:
        raise InvalidTransition(
            f"task `{task_id}` cannot move from `{task.status}` to `{status}`"
        )
    previous = task.status
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
        tenant_id=tenant_id,
        actor="control-plane",
        action="task.transition",
        subject_type="task",
        subject_id=task.id,
        detail={"from": previous, "to": status, "outcome": outcome},
    )
    return task


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
    fingerprint: str,
    requested_by: str,
    task_id: str | None = None,
) -> models.Approval:
    await require_tenant(session, tenant_id)
    if task_id is not None:
        await require_task(session, tenant_id, task_id)
    approval = models.Approval(
        id=new_id("apr"),
        tenant_id=tenant_id,
        task_id=task_id,
        action=action,
        fingerprint=fingerprint,
        requested_by=requested_by,
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
        detail={"action": action, "taskId": task_id},
    )
    return approval


async def decide_approval(
    session: AsyncSession,
    *,
    tenant_id: str,
    approval_id: str,
    decision: str,
    decided_by: str,
) -> models.Approval:
    if decision not in APPROVAL_STATES:
        raise Conflict(f"unknown approval decision `{decision}`")
    approval = await session.get(models.Approval, approval_id)
    if approval is None or approval.tenant_id != tenant_id:
        raise NotFound(f"approval `{approval_id}` was not found")
    if approval.state != "pending":
        raise Conflict(f"approval `{approval_id}` is already {approval.state}")
    approval.state = decision
    approval.decided_by = decided_by
    approval.decided_at = utc_now()
    await session.flush()
    await record_audit(
        session,
        tenant_id=tenant_id,
        actor=decided_by,
        action=f"approval.{decision}",
        subject_type="approval",
        subject_id=approval.id,
        detail={"action": approval.action},
    )
    return approval


async def list_approvals(session: AsyncSession, tenant_id: str) -> list[models.Approval]:
    await require_tenant(session, tenant_id)
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
    return {
        "tenants": int(tenants or 0),
        "nodes": int(nodes or 0),
        "tasks_by_status": tasks_by_status,
        "pending_approvals": int(pending or 0),
    }


# Re-exported so callers do not import the model module for constants.
__all__ = [
    "ACTIVE_TASK_STATUSES",
    "APPROVAL_STATES",
    "TASK_STATUSES",
    "create_approval",
    "create_model_config",
    "create_task",
    "create_tenant",
    "decide_approval",
    "heartbeat",
    "list_approvals",
    "list_audit",
    "list_model_configs",
    "list_nodes",
    "list_tasks",
    "list_tenants",
    "metrics",
    "new_id",
    "record_audit",
    "recoverable_tasks",
    "register_node",
    "require_node",
    "require_task",
    "require_tenant",
    "transition_task",
    "utc_now",
]
