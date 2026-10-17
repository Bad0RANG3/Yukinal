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

from .db import Base
from .nodeauth import key_fingerprint

# Task statuses. ``unknown`` is the terminal-but-unresolved state used when a
# node restarts while a task is running: the remote effect may have happened, so
# the row is never replayed automatically.
TASK_STATUSES = ("queued", "running", "succeeded", "failed", "cancelled", "unknown")
ACTIVE_TASK_STATUSES = ("queued", "running")

APPROVAL_STATES = ("pending", "approved", "rejected", "expired")


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


class Approval(Base):
    __tablename__ = "approvals"

    id: Mapped[str] = mapped_column(String(64), primary_key=True)
    tenant_id: Mapped[str] = mapped_column(
        ForeignKey("tenants.id", ondelete="CASCADE"), nullable=False, index=True
    )
    task_id: Mapped[str | None] = mapped_column(
        ForeignKey("tasks.id", ondelete="SET NULL"), index=True
    )
    action: Mapped[str] = mapped_column(String(128), nullable=False)
    # Fingerprint of the exact action input. A decision only applies to the
    # matching fingerprint (the desktop host uses the same rule).
    fingerprint: Mapped[str] = mapped_column(String(128), nullable=False)
    state: Mapped[str] = mapped_column(String(16), nullable=False, default="pending")
    requested_by: Mapped[str] = mapped_column(String(200), nullable=False)
    decided_by: Mapped[str | None] = mapped_column(String(200))
    requested_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=utc_now)
    decided_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True))


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
