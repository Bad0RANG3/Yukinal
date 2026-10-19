"""FastAPI application factory for the control plane.

The app is deliberately thin: routes validate the envelope, scope the call to a
tenant and hand off to :mod:`yukinal_control_plane.service`. Every mutation is
authenticated, tenant-scoped and audited, and the credential boundary is enforced
before any write.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager

from fastapi import Depends, FastAPI, Header, HTTPException, Request, WebSocket
from fastapi.responses import JSONResponse
from sqlalchemy import inspect, text
from sqlalchemy.exc import IntegrityError
from sqlalchemy.ext.asyncio import AsyncEngine, AsyncSession

from . import channel, models, nodeauth, schemas, service
from .config import Settings
from .db import Base, create_engine, create_session_factory
from .errors import ControlPlaneError
from .security import assert_no_secret_fields


def create_app(settings: Settings, *, engine: AsyncEngine | None = None) -> FastAPI:
    """Build the ASGI app.

    ``settings.require_token()`` runs here so a deployment with neither a token
    nor an explicit anonymous opt-in fails at startup instead of serving an
    unauthenticated registry.
    """
    settings.require_token()
    engine = engine if engine is not None else create_engine(settings.database_url)
    session_factory = create_session_factory(engine)

    @asynccontextmanager
    async def lifespan(_app: FastAPI) -> AsyncIterator[None]:
        # Refuse to serve an unmigrated database: a missing table would otherwise
        # surface later as a 500 from whichever endpoint touched it first.
        async with engine.connect() as connection:
            existing = await connection.run_sync(
                lambda sync_connection: set(inspect(sync_connection).get_table_names())
            )
        missing = sorted(set(Base.metadata.tables) - existing)
        if missing:
            raise RuntimeError(
                "the control-plane schema is not migrated; missing tables: "
                + ", ".join(missing)
                + " (run `alembic upgrade head`)"
            )
        yield

    app = FastAPI(title="Yukinal control plane", version="0.1.0", lifespan=lifespan)
    app.state.settings = settings
    app.state.engine = engine
    app.state.session_factory = session_factory

    async def get_session() -> AsyncIterator[AsyncSession]:
        async with session_factory() as session:
            async with session.begin():
                yield session

    async def current_actor(
        request: Request,
        authorization: str | None = Header(default=None),
        x_yukinal_actor: str | None = Header(default=None, alias="X-Yukinal-Actor"),
    ) -> str:
        token = request.app.state.settings.require_token()
        if token is not None:
            expected = f"Bearer {token}"
            if authorization is None or not _constant_time_equal(authorization, expected):
                raise HTTPException(status_code=401, detail="unauthorized")
        actor = (x_yukinal_actor or "operator").strip()
        return actor or "operator"

    def canonical_path(request: Request) -> str:
        """The path a node signs: the request path plus any query string."""
        query = request.url.query
        return request.url.path + (f"?{query}" if query else "")

    async def current_node(
        request: Request,
        session: AsyncSession = Depends(get_session),
        x_yukinal_node: str | None = Header(default=None, alias=nodeauth.HEADER_NODE),
        x_yukinal_timestamp: str | None = Header(default=None, alias=nodeauth.HEADER_TIMESTAMP),
        x_yukinal_nonce: str | None = Header(default=None, alias=nodeauth.HEADER_NONCE),
        x_yukinal_signature: str | None = Header(default=None, alias=nodeauth.HEADER_SIGNATURE),
    ) -> models.Node:
        """Authenticate a request as a registered node.

        The signature covers the body, so the tenant is read from the node row
        rather than the URL: node-facing routes take no tenant id at all, and a
        node can never act on another tenant's data.
        """
        return await service.authenticate_node(
            session,
            nonce_session_factory=session_factory,
            node_id=x_yukinal_node,
            timestamp=x_yukinal_timestamp,
            nonce=x_yukinal_nonce,
            signature=x_yukinal_signature,
            method=request.method,
            path=canonical_path(request),
            body=await request.body(),
        )

    @app.exception_handler(ControlPlaneError)
    async def handle_domain_error(_request: Request, error: ControlPlaneError) -> JSONResponse:
        return JSONResponse(
            status_code=error.status_code,
            content={"code": error.code, "message": error.message},
        )

    @app.exception_handler(IntegrityError)
    async def handle_integrity_error(_request: Request, error: IntegrityError) -> JSONResponse:
        # A constraint the route did not anticipate is still a conflict, never a
        # 500 with the driver text in the body.
        return JSONResponse(
            status_code=409,
            content={"code": "conflict", "message": "the request violates a stored constraint"},
        )

    @app.get("/healthz", response_model=schemas.HealthView)
    async def healthz(session: AsyncSession = Depends(get_session)) -> dict:
        await session.execute(text("SELECT 1"))
        return {"status": "ok", "database": "ok"}

    @app.get("/v1/metrics", response_model=schemas.MetricsView)
    async def metrics(
        session: AsyncSession = Depends(get_session), _actor: str = Depends(current_actor)
    ) -> dict:
        return await service.metrics(session)

    # ── tenants ──────────────────────────────────────────────────────────────

    @app.get("/v1/tenants", response_model=list[schemas.TenantView])
    async def list_tenants(
        session: AsyncSession = Depends(get_session), _actor: str = Depends(current_actor)
    ) -> list:
        return await service.list_tenants(session)

    @app.post("/v1/tenants", response_model=schemas.TenantView, status_code=201)
    async def create_tenant(
        body: schemas.TenantCreate,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        return await service.create_tenant(session, name=body.name, tenant_id=body.id)

    @app.get("/v1/tenants/{tenant_id}", response_model=schemas.TenantView)
    async def get_tenant(
        tenant_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        return await service.require_tenant(session, tenant_id)

    # ── nodes ────────────────────────────────────────────────────────────────

    @app.get("/v1/tenants/{tenant_id}/nodes", response_model=list[schemas.NodeView])
    async def list_nodes(
        tenant_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> list:
        return await service.list_nodes(session, tenant_id)

    @app.post("/v1/tenants/{tenant_id}/nodes", response_model=schemas.NodeView, status_code=201)
    async def register_node(
        tenant_id: str,
        body: schemas.NodeCreate,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        return await service.register_node(
            session,
            tenant_id=tenant_id,
            name=body.name,
            public_key=body.public_key,
            agent_version=body.agent_version,
        )

    @app.get("/v1/tenants/{tenant_id}/nodes/{node_id}", response_model=schemas.NodeView)
    async def get_node(
        tenant_id: str,
        node_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        return await service.require_node(session, tenant_id, node_id)

    @app.post(
        "/v1/tenants/{tenant_id}/nodes/{node_id}/heartbeat",
        response_model=schemas.HeartbeatResult,
    )
    async def node_heartbeat(
        tenant_id: str,
        node_id: str,
        body: schemas.HeartbeatRequest,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> dict:
        node, marked = await service.heartbeat(
            session,
            tenant_id=tenant_id,
            node_id=node_id,
            agent_version=body.agent_version,
            restarted=body.restarted,
        )
        return {
            "node_id": node.id,
            "last_seen_at": node.last_seen_at,
            "marked_unknown": marked,
        }

    @app.get(
        "/v1/tenants/{tenant_id}/nodes/{node_id}/recoverable",
        response_model=list[schemas.TaskView],
    )
    async def recoverable(
        tenant_id: str,
        node_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> list:
        return await service.recoverable_tasks(session, tenant_id=tenant_id, node_id=node_id)

    # ── tasks ────────────────────────────────────────────────────────────────

    @app.get("/v1/tenants/{tenant_id}/tasks", response_model=list[schemas.TaskView])
    async def list_tasks(
        tenant_id: str,
        node_id: str | None = None,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> list:
        return await service.list_tasks(session, tenant_id, node_id=node_id)

    @app.post("/v1/tenants/{tenant_id}/tasks", response_model=schemas.TaskView, status_code=201)
    async def create_task(
        tenant_id: str,
        body: schemas.TaskCreate,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        assert_no_secret_fields(body.payload)
        return await service.create_task(
            session,
            tenant_id=tenant_id,
            node_id=body.node_id,
            kind=body.kind,
            payload=body.payload,
        )

    @app.get("/v1/tenants/{tenant_id}/tasks/{task_id}", response_model=schemas.TaskView)
    async def get_task(
        tenant_id: str,
        task_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        return await service.require_task(session, tenant_id, task_id)

    @app.post("/v1/tenants/{tenant_id}/tasks/{task_id}/status", response_model=schemas.TaskView)
    async def update_task_status(
        tenant_id: str,
        task_id: str,
        body: schemas.TaskStatusUpdate,
        session: AsyncSession = Depends(get_session),
        actor: str = Depends(current_actor),
    ) -> object:
        return await service.transition_task(
            session,
            tenant_id=tenant_id,
            task_id=task_id,
            status=body.status,
            outcome=body.outcome,
            result=body.result,
        )

    # ── model configs ────────────────────────────────────────────────────────

    @app.get(
        "/v1/tenants/{tenant_id}/model-configs",
        response_model=list[schemas.ModelConfigView],
    )
    async def list_model_configs(
        tenant_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> list:
        return await service.list_model_configs(session, tenant_id)

    @app.post(
        "/v1/tenants/{tenant_id}/model-configs",
        response_model=schemas.ModelConfigView,
        status_code=201,
    )
    async def create_model_config(
        tenant_id: str,
        body: schemas.ModelConfigCreate,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        return await service.create_model_config(
            session,
            tenant_id=tenant_id,
            name=body.name,
            provider=body.provider,
            model=body.model,
            api_key_ref=body.api_key_ref,
        )

    # ── the tenant's action policy ───────────────────────────────────────────

    @app.get("/v1/tenants/{tenant_id}/policy", response_model=schemas.TenantPolicyView)
    async def get_tenant_policy(
        tenant_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> dict:
        return await service.get_tenant_policy(session, tenant_id)

    @app.put("/v1/tenants/{tenant_id}/policy", response_model=schemas.TenantPolicyView)
    async def put_tenant_policy(
        tenant_id: str,
        body: schemas.TenantPolicyUpdate,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> dict:
        """Set it, or change one field of it.

        Omitting ``tierModes`` derives them from the environment; sending them is
        how a tenant departs from the built-in table without changing what the
        environment means.
        """
        return await service.upsert_tenant_policy(
            session,
            tenant_id=tenant_id,
            environment=body.environment,
            tier_modes=body.tier_modes.model_dump() if body.tier_modes else None,
            approval_ttl_seconds=body.approval_ttl_seconds,
            updated_by=body.updated_by,
        )

    @app.delete("/v1/tenants/{tenant_id}/policy", status_code=204)
    async def delete_tenant_policy(
        tenant_id: str,
        session: AsyncSession = Depends(get_session),
        actor: str = Depends(current_actor),
    ) -> None:
        await service.delete_tenant_policy(session, tenant_id=tenant_id, actor=actor)

    @app.get(
        "/v1/tenants/{tenant_id}/tool-policies",
        response_model=list[schemas.ToolPolicyView],
    )
    async def list_tool_policies(
        tenant_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> list:
        return await service.list_tool_policies(session, tenant_id)

    @app.put(
        "/v1/tenants/{tenant_id}/tool-policies/{kind}",
        response_model=schemas.ToolPolicyView,
    )
    async def put_tool_policy(
        tenant_id: str,
        kind: str,
        body: schemas.ToolPolicyUpdate,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> dict:
        return await service.upsert_tool_policy(
            session,
            tenant_id=tenant_id,
            kind=kind,
            risk=body.risk,
            updated_by=body.updated_by,
        )

    @app.delete("/v1/tenants/{tenant_id}/tool-policies/{kind}", status_code=204)
    async def delete_tool_policy(
        tenant_id: str,
        kind: str,
        session: AsyncSession = Depends(get_session),
        actor: str = Depends(current_actor),
    ) -> None:
        await service.delete_tool_policy(
            session, tenant_id=tenant_id, kind=kind, actor=actor
        )

    # ── approvals ────────────────────────────────────────────────────────────

    @app.get("/v1/tenants/{tenant_id}/approvals", response_model=list[schemas.ApprovalView])
    async def list_approvals(
        tenant_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> list:
        return await service.list_approvals(session, tenant_id)

    @app.post(
        "/v1/tenants/{tenant_id}/approvals", response_model=schemas.ApprovalView, status_code=201
    )
    async def create_approval(
        tenant_id: str,
        body: schemas.ApprovalCreate,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        return await service.create_approval(
            session,
            tenant_id=tenant_id,
            action=body.action,
            node_id=body.node_id,
            payload=body.payload,
            requested_by=body.requested_by,
            task_id=body.task_id,
            max_uses=body.max_uses,
        )

    @app.post(
        "/v1/tenants/{tenant_id}/approvals/{approval_id}/decide",
        response_model=schemas.ApprovalView,
    )
    async def decide_approval(
        tenant_id: str,
        approval_id: str,
        body: schemas.ApprovalDecision,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        return await service.decide_approval(
            session,
            tenant_id=tenant_id,
            approval_id=approval_id,
            decision=body.decision,
            decided_by=body.decided_by,
            max_uses=body.max_uses,
        )

    @app.post(
        "/v1/tenants/{tenant_id}/approvals/{approval_id}/revoke",
        response_model=schemas.ApprovalView,
    )
    async def revoke_approval(
        tenant_id: str,
        approval_id: str,
        body: schemas.ApprovalRevoke,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> object:
        return await service.revoke_approval(
            session,
            tenant_id=tenant_id,
            approval_id=approval_id,
            revoked_by=body.revoked_by,
        )

    @app.post(
        "/v1/tenants/{tenant_id}/approvals/expire",
        response_model=schemas.ExpiredApprovals,
    )
    async def expire_approvals(
        tenant_id: str,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> dict:
        """Sweep lapsed decisions, so an operator (or a cron) can drive it.

        Every gate check is time-aware on its own, so this only makes the stored
        state match reality sooner.

        The tenant gate comes first and is the same one every sibling tenant route
        uses: sweeping is a write, so an unknown or disabled tenant must be a 404
        here too, not a 200 that reports a sweep of nothing. Without it the
        endpoint is the one tenant path that answers for a tenant that does not
        exist, and on a disabled tenant it would still move that tenant's rows.
        """
        await service.require_tenant(session, tenant_id)
        expired = await service.expire_stale_approvals(session, tenant_id=tenant_id)
        return {"expired": expired}

    # ── audit ────────────────────────────────────────────────────────────────

    @app.get("/v1/tenants/{tenant_id}/audit", response_model=list[schemas.AuditView])
    async def list_audit(
        tenant_id: str,
        limit: int = 200,
        session: AsyncSession = Depends(get_session),
        _actor: str = Depends(current_actor),
    ) -> list:
        return await service.list_audit(session, tenant_id, limit=max(1, min(limit, 1000)))

    # ── node-facing API ──────────────────────────────────────────────────────
    # These routes authenticate with an Ed25519 signature, not the operator
    # bearer token, and derive the tenant from the authenticated node row.

    @app.post("/v1/node/heartbeat", response_model=schemas.HeartbeatResult)
    async def node_heartbeat(
        body: schemas.HeartbeatRequest,
        node: models.Node = Depends(current_node),
        session: AsyncSession = Depends(get_session),
    ) -> dict:
        updated, marked = await service.heartbeat(
            session,
            tenant_id=node.tenant_id,
            node_id=node.id,
            agent_version=body.agent_version,
            restarted=body.restarted,
        )
        return {
            "node_id": updated.id,
            "last_seen_at": updated.last_seen_at,
            "marked_unknown": marked,
        }

    @app.get("/v1/node/tasks", response_model=list[schemas.TaskView])
    async def node_list_tasks(
        node: models.Node = Depends(current_node),
        session: AsyncSession = Depends(get_session),
    ) -> list:
        return await service.node_tasks(session, node)

    @app.get("/v1/node/recoverable", response_model=list[schemas.TaskView])
    async def node_recoverable(
        node: models.Node = Depends(current_node),
        session: AsyncSession = Depends(get_session),
    ) -> list:
        return await service.recoverable_tasks_for_node(session, node)

    @app.post("/v1/node/tasks/{task_id}/status", response_model=schemas.TaskView)
    async def node_task_status(
        task_id: str,
        body: schemas.TaskStatusUpdate,
        node: models.Node = Depends(current_node),
        session: AsyncSession = Depends(get_session),
    ) -> object:
        return await service.transition_node_task(
            session,
            node=node,
            task_id=task_id,
            status=body.status,
            outcome=body.outcome,
            result=body.result,
        )

    @app.websocket("/v1/node/channel")
    async def node_channel(websocket: WebSocket) -> None:
        """The node's outbound channel: assignment, status and disconnect recovery."""
        await channel.run_channel(websocket, session_factory)

    return app


def _constant_time_equal(left: str, right: str) -> bool:
    import hmac

    return hmac.compare_digest(left, right)
