"""Request/response models.

Every request model forbids unknown fields, so a caller that sends a raw secret
(for example ``apiKey``) gets a validation error instead of silently having it
dropped or stored.
"""

from __future__ import annotations

from datetime import datetime, timezone

from pydantic import BaseModel, ConfigDict, Field, field_serializer, field_validator

from . import actions, nodeauth


class _Request(BaseModel):
    model_config = ConfigDict(extra="forbid")


class _View(BaseModel):
    """Response model that reads straight off a SQLAlchemy row."""

    model_config = ConfigDict(from_attributes=True)

    @field_serializer("*")
    def _utc(self, value: object, _info: object) -> object:
        """Emit every timestamp as an explicit UTC instant.

        SQLite has no timezone type, so it returns the naive UTC wall clock it
        stored even for a ``timezone=True`` column, while PostgreSQL returns an
        aware value. Without this, the same field would come back as
        ``...T05:42:17`` when read from the database and ``...T05:42:17Z`` when
        echoed from memory, and a client could not compare it with a clock.
        """
        if isinstance(value, datetime) and value.tzinfo is None:
            return value.replace(tzinfo=timezone.utc)
        return value


class TenantCreate(_Request):
    id: str | None = Field(default=None, min_length=1, max_length=64, pattern=r"^ten_")
    name: str = Field(min_length=1, max_length=200)


class TenantView(_View):
    id: str
    name: str
    disabled: bool
    created_at: datetime


class NodeCreate(_Request):
    name: str = Field(min_length=1, max_length=200)
    public_key: str = Field(alias="publicKey", min_length=16, max_length=2048)
    agent_version: str | None = Field(default=None, alias="agentVersion", max_length=64)

    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    @field_validator("public_key")
    @classmethod
    def _canonical_ed25519_key(cls, value: str) -> str:
        """Store one canonical form and reject a key that could never verify.

        Registration is the only place a node identity enters the system, so a
        key that is not a 32-byte Ed25519 public key is refused here rather than
        accepted and failing every later signature check.
        """
        try:
            return nodeauth.encode_public_key(nodeauth.parse_public_key(value))
        except ValueError as error:
            raise ValueError(str(error)) from error


class NodeView(_View):
    id: str
    tenant_id: str
    name: str
    public_key: str
    key_fingerprint: str
    agent_version: str | None
    last_seen_at: datetime | None
    created_at: datetime


class HeartbeatRequest(_Request):
    agent_version: str | None = Field(default=None, alias="agentVersion", max_length=64)
    # Set when the node reports that it restarted with an empty in-memory queue,
    # so any still-``running`` task on it becomes outcome-unknown.
    restarted: bool = False

    model_config = ConfigDict(extra="forbid", populate_by_name=True)


class HeartbeatResult(BaseModel):
    node_id: str
    last_seen_at: datetime
    marked_unknown: list[str]


class TaskCreate(_Request):
    node_id: str = Field(alias="nodeId", min_length=1, max_length=64)
    kind: str = Field(min_length=1, max_length=64)
    payload: dict = Field(default_factory=dict)

    model_config = ConfigDict(extra="forbid", populate_by_name=True)


class TaskStatusUpdate(_Request):
    status: str = Field(min_length=1, max_length=16)
    outcome: str | None = Field(default=None, max_length=64)
    result: dict | None = None


class TaskView(_View):
    id: str
    tenant_id: str
    node_id: str
    kind: str
    # The approval that authorised this task, if its kind needed one, so the
    # authorisation is auditable from the task rather than only from the ledger.
    approval_id: str | None
    payload: dict
    # Computed from (kind, nodeId, payload): the exact action an approval must
    # match before the task may leave the queue.
    fingerprint: str
    status: str
    outcome: str | None
    result: dict | None
    attempts: int
    created_at: datetime
    updated_at: datetime


class ModelConfigCreate(_Request):
    name: str = Field(min_length=1, max_length=200)
    provider: str = Field(min_length=1, max_length=64)
    model: str = Field(min_length=1, max_length=128)
    api_key_ref: str | None = Field(default=None, alias="apiKeyRef", max_length=200)

    model_config = ConfigDict(extra="forbid", populate_by_name=True)


class ModelConfigView(_View):
    id: str
    tenant_id: str
    name: str
    provider: str
    model: str
    api_key_ref: str | None
    created_at: datetime


class ApprovalCreate(_Request):
    """A request to approve one exact action.

    The caller sends the action, the node and the input; the server computes the
    fingerprint from them. An operator therefore never has to reproduce the
    canonical form by hand, and cannot pick a fingerprint that does not match the
    input it claims to authorise.
    """

    action: str = Field(min_length=1, max_length=128)
    node_id: str = Field(alias="nodeId", min_length=1, max_length=64)
    payload: dict = Field(default_factory=dict)
    task_id: str | None = Field(default=None, alias="taskId", max_length=64)
    requested_by: str = Field(alias="requestedBy", min_length=1, max_length=200)
    # How many tasks the decision may authorise. Defaults to one: an approval is
    # a decision about an action, not a standing grant.
    max_uses: int = Field(
        default=actions.DEFAULT_APPROVAL_USES,
        alias="maxUses",
        ge=1,
        le=actions.MAX_APPROVAL_USES,
    )

    model_config = ConfigDict(extra="forbid", populate_by_name=True)


class ApprovalDecision(_Request):
    decision: str = Field(pattern=r"^(approved|rejected)$")
    decided_by: str = Field(alias="decidedBy", min_length=1, max_length=200)
    # A decider may narrow what was asked for but never widen it. `None` keeps
    # the requested count.
    max_uses: int | None = Field(
        default=None, alias="maxUses", ge=1, le=actions.MAX_APPROVAL_USES
    )

    model_config = ConfigDict(extra="forbid", populate_by_name=True)


class ApprovalRevoke(_Request):
    revoked_by: str = Field(alias="revokedBy", min_length=1, max_length=200)

    model_config = ConfigDict(extra="forbid", populate_by_name=True)


class ExpiredApprovals(_View):
    expired: int


class ApprovalView(_View):
    id: str
    tenant_id: str
    task_id: str | None
    node_id: str
    payload: dict
    action: str
    fingerprint: str
    state: str
    requested_by: str
    decided_by: str | None
    requested_at: datetime
    decided_at: datetime | None
    # Usable until this moment, and up to `max_uses` tasks. Both are visible so
    # a decider can see exactly what a grant would mean before making it.
    expires_at: datetime | None
    uses: int
    max_uses: int
    revoked_by: str | None
    revoked_at: datetime | None


class AuditView(_View):
    id: int
    tenant_id: str
    actor: str
    action: str
    subject_type: str
    subject_id: str
    detail: dict
    at: datetime


class HealthView(BaseModel):
    status: str
    database: str


class MetricsView(BaseModel):
    tenants: int
    nodes: int
    tasks_by_status: dict[str, int]
    approvals_by_state: dict[str, int]
    pending_approvals: int
