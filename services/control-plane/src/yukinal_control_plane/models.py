"""Control-plane tables.

The schema is deliberately dialect-neutral: it is created on SQLite for tests
and on PostgreSQL in production. Identifier columns are prefixed strings so a
row is recognisable without a join, and every child row carries ``tenant_id`` so
tenant isolation is a plain ``WHERE`` clause rather than a trust decision.
"""

from __future__ import annotations

import binascii
from base64 import b64decode
from datetime import datetime, timezone

from sqlalchemy import (
    JSON,
    Boolean,
    DateTime,
    ForeignKey,
    Index,
    Integer,
    String,
    UniqueConstraint,
    text,
)
from sqlalchemy.orm import Mapped, mapped_column, relationship

from .actions import action_fingerprint
from .db import Base
from .nodeauth import key_fingerprint

# Task statuses. ``unknown`` is the terminal-but-unresolved state used when a
# node restarts while a task is running: the remote effect may have happened, so
# the row is never replayed automatically.
TASK_STATUSES = ("queued", "running", "succeeded", "failed", "cancelled", "unknown")
ACTIVE_TASK_STATUSES = ("queued", "running")

APPROVAL_STATES = ("pending", "approved", "rejected", "expired", "revoked")
# Only these two are decisions an operator may make; `expired` and `revoked` are
# lifecycle outcomes, not choices.
APPROVAL_DECISIONS = ("approved", "rejected")


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


class Tenant(Base):
    __tablename__ = "tenants"

    id: Mapped[str] = mapped_column(String(64), primary_key=True)
    name: Mapped[str] = mapped_column(String(200), nullable=False)
    disabled: Mapped[bool] = mapped_column(Boolean, nullable=False, default=False)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utc_now)


class Node(Base):
    __tablename__ = "nodes"
    __table_args__ = (UniqueConstraint("tenant_id", "name", name="uq_nodes_tenant_name"),)

    id: Mapped[str] = mapped_column(String(64), primary_key=True)
    tenant_id: Mapped[str] = mapped_column(
        ForeignKey("tenants.id", ondelete="CASCADE"), nullable=False, index=True
    )
    name: Mapped[str] = mapped_column(String(200), nullable=False)
    # The node's own public identity, supplied at registration. Private keys
    # never reach the control plane.
    public_key: Mapped[str] = mapped_column(String(2048), nullable=False)
    agent_version: Mapped[str | None] = mapped_column(String(64))
    last_seen_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utc_now)

    tasks: Mapped[list["Task"]] = relationship(back_populates="node")

    @property
    def key_fingerprint(self) -> str:
        """Short digest of the public key, so the API never has to echo it.

        ``public_key`` is validated at registration, so a decode failure here
        means a row was written outside the API; report that instead of raising
        in the middle of serializing a response.
        """
        try:
            return key_fingerprint(b64decode(self.public_key, validate=True))
        except (binascii.Error, ValueError):
            return "SHA256:unparseable"


class NodeNonce(Base):
    """Consumed request nonces, so a signed node request cannot be replayed.

    The composite primary key makes a second use of the same nonce a constraint
    violation rather than a silent accept. Rows older than the signature window
    are pruned opportunistically on each claim, so the table stays small. This is
    database-backed on purpose: a per-process cache would let a replay reach a
    different worker.
    """

    __tablename__ = "node_nonces"

    node_id: Mapped[str] = mapped_column(
        ForeignKey("nodes.id", ondelete="CASCADE"), primary_key=True
    )
    nonce: Mapped[str] = mapped_column(String(128), primary_key=True)
    at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=utc_now, index=True
    )


class Task(Base):
    __tablename__ = "tasks"
    __table_args__ = (
        # At most one active task per node: the database, not just the request
        # handler, is the concurrency control. SQLite and PostgreSQL both build
        # the same partial index.
        Index(
            "uq_tasks_active_per_node",
            "node_id",
            unique=True,
            sqlite_where=text("status IN ('queued','running')"),
            postgresql_where=text("status IN ('queued','running')"),
        ),
    )

    id: Mapped[str] = mapped_column(String(64), primary_key=True)
    tenant_id: Mapped[str] = mapped_column(
        ForeignKey("tenants.id", ondelete="CASCADE"), nullable=False, index=True
    )
    node_id: Mapped[str] = mapped_column(
        ForeignKey("nodes.id", ondelete="CASCADE"), nullable=False, index=True
    )
    kind: Mapped[str] = mapped_column(String(64), nullable=False)
    # The approval that authorised this task, if its kind needed one. A soft
    # reference rather than a foreign key: `approvals.task_id` already points the
    # other way, and a second constraint would be a cycle that SQLite cannot
    # express. The task is the record of what it was authorised by; the approval
    # stays the record of what was authorised.
    approval_id: Mapped[str | None] = mapped_column(String(64), index=True)
    # Structured request for the node. Must already be free of secrets: the
    # caller resolves credential references on the node, not here.
    payload: Mapped[dict] = mapped_column(JSON, nullable=False, default=dict)
    status: Mapped[str] = mapped_column(String(16), nullable=False, default="queued")
    outcome: Mapped[str | None] = mapped_column(String(64))
    result: Mapped[dict | None] = mapped_column(JSON)
    attempts: Mapped[int] = mapped_column(Integer, nullable=False, default=0)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utc_now)
    updated_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utc_now)

    node: Mapped[Node] = relationship(back_populates="tasks")

    @property
    def fingerprint(self) -> str:
        """Fingerprint of the exact action this task performs.

        Published on the API so an operator can approve precisely this action;
        an approval stores the same value, and a mismatch between the two never
        clears the task.
        """
        return action_fingerprint(
            kind=self.kind, node_id=self.node_id, payload=self.payload or {}
        )


class Approval(Base):
    __tablename__ = "approvals"

    id: Mapped[str] = mapped_column(String(64), primary_key=True)
    tenant_id: Mapped[str] = mapped_column(
        ForeignKey("tenants.id", ondelete="CASCADE"), nullable=False, index=True
    )
    task_id: Mapped[str | None] = mapped_column(
        ForeignKey("tasks.id", ondelete="SET NULL"), index=True
    )
    # The node the action is bound to, and the input it was requested for.
    # Stored because a decision is only meaningful if the approver can see what
    # they are approving; the fingerprint alone would be opaque.
    node_id: Mapped[str] = mapped_column(
        ForeignKey("nodes.id", ondelete="CASCADE"), nullable=False, index=True
    )
    payload: Mapped[dict] = mapped_column(JSON, nullable=False, default=dict)
    action: Mapped[str] = mapped_column(String(128), nullable=False)
    # Fingerprint of the exact action input. A decision only applies to the
    # matching fingerprint (the desktop host uses the same rule).
    fingerprint: Mapped[str] = mapped_column(String(128), nullable=False)
    state: Mapped[str] = mapped_column(String(16), nullable=False, default="pending")
    requested_by: Mapped[str] = mapped_column(String(200), nullable=False)
    decided_by: Mapped[str | None] = mapped_column(String(200))
    requested_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utc_now)
    decided_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))
    # An approved decision is usable until this moment. A decision with no expiry
    # would be a standing grant, so `approved` rows always carry one; `expired`
    # is what a sweep writes when it passes.
    expires_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), index=True)
    # How many tasks this decision has authorised, and how many it may. The unit
    # of authorisation is one task: an approval for "restart nginx" that could be
    # spent without limit is not a decision about an action.
    uses: Mapped[int] = mapped_column(Integer, nullable=False, default=0)
    max_uses: Mapped[int] = mapped_column(Integer, nullable=False, default=1)
    revoked_by: Mapped[str | None] = mapped_column(String(200))
    revoked_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))


class ModelConfig(Base):
    """A provider/model selection for a tenant.

    Only a *reference* to a credential is stored. A raw key in the request is a
    validation error (``extra="forbid"``), so no code path can persist one.
    """

    __tablename__ = "model_configs"
    __table_args__ = (
        UniqueConstraint("tenant_id", "name", name="uq_model_configs_tenant_name"),
    )

    id: Mapped[str] = mapped_column(String(64), primary_key=True)
    tenant_id: Mapped[str] = mapped_column(
        ForeignKey("tenants.id", ondelete="CASCADE"), nullable=False, index=True
    )
    name: Mapped[str] = mapped_column(String(200), nullable=False)
    provider: Mapped[str] = mapped_column(String(64), nullable=False)
    model: Mapped[str] = mapped_column(String(128), nullable=False)
    api_key_ref: Mapped[str | None] = mapped_column(String(200))
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utc_now)


class AuditEvent(Base):
    __tablename__ = "audit_events"

    id: Mapped[int] = mapped_column(Integer, primary_key=True, autoincrement=True)
    tenant_id: Mapped[str] = mapped_column(
        ForeignKey("tenants.id", ondelete="CASCADE"), nullable=False, index=True
    )
    actor: Mapped[str] = mapped_column(String(200), nullable=False)
    action: Mapped[str] = mapped_column(String(64), nullable=False)
    subject_type: Mapped[str] = mapped_column(String(32), nullable=False)
    subject_id: Mapped[str] = mapped_column(String(64), nullable=False)
    detail: Mapped[dict] = mapped_column(JSON, nullable=False, default=dict)
    at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utc_now)
