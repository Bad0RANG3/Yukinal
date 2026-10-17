# Control Plane (FastAPI + PostgreSQL)

The phase-4 control plane: a multi-tenant registry and task broker for desktop
hosts and their nodes. It is a separate service and does not change the
single-machine desktop path.

It owns the state the desktop host keeps locally but that a fleet needs shared:
tenants, nodes, tasks, provider/model selections, approvals and an append-only
audit log.

## Stack

- **FastAPI** + **uvicorn** for the HTTP API.
- **SQLAlchemy 2.0** (async) for the models and the session boundary.
- **Alembic** for schema migrations.
- **PostgreSQL** (`asyncpg`) in production. Tests and local development use
  **SQLite** (`aiosqlite`) so no database server is required.

## Layout

```
pyproject.toml                 package metadata and dependencies
alembic.ini                    migration configuration (URL comes from the env)
migrations/                    Alembic environment and revisions
src/yukinal_control_plane/
  config.py                    environment settings, fail-closed token check
  db.py                        async engine/session and the declarative base
  models.py                    tenants, nodes, tasks, model configs, approvals, audit
  schemas.py                   request/response models (unknown fields forbidden)
  security.py                  the credential boundary (secret-shaped fields rejected)
  service.py                   domain operations, one audit event per mutation
  app.py                       FastAPI factory: routes, auth, error mapping
tests/test_control_plane.py    end-to-end API, schema, migration and boundary tests
```

## Run

```bash
python -m venv .venv
.venv/bin/pip install -e ".[test]"          # add "postgres" for a real PostgreSQL
export YUKINAL_CONTROL_DATABASE_URL="postgresql+asyncpg://user:pass@host/yukinal"
export YUKINAL_CONTROL_TOKEN="$(openssl rand -hex 32)"

.venv/bin/alembic upgrade head               # create the schema
.venv/bin/python -m yukinal_control_plane    # serve on 127.0.0.1:8765
```

The process refuses to start when the schema is not migrated, and it refuses to
start when neither `YUKINAL_CONTROL_TOKEN` nor the explicit local-only
`YUKINAL_CONTROL_ALLOW_ANONYMOUS=1` is set. It binds to loopback by default.

### Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `YUKINAL_CONTROL_DATABASE_URL` | `sqlite+aiosqlite:///./yukinal-control.db` | SQLAlchemy async URL. Production uses `postgresql+asyncpg://`. |
| `YUKINAL_CONTROL_TOKEN` | unset | Shared bearer token for every `/v1` call. |
| `YUKINAL_CONTROL_ALLOW_ANONYMOUS` | `false` | Local-only opt-out of the token requirement. |
| `YUKINAL_CONTROL_BIND_HOST` | `127.0.0.1` | Bind address. Set explicitly to accept node traffic. |
| `YUKINAL_CONTROL_PORT` | `8765` | Bind port. |

## What the API enforces

- **Tenant isolation.** Every node, task, approval and model config is scoped to
  a tenant. A cross-tenant identifier is `404`, never `403`, so the API never
  confirms that a row exists in another tenant.
- **One active task per node.** A partial unique index (`status IN ('queued',
  'running')`) makes the database, not just the handler, the concurrency
  control. Terminal tasks accumulate freely.
- **A task state machine.** `queued → running → succeeded/failed/cancelled`, with
  `running → unknown` on a node restart. An invalid transition is `409`.
- **Outcome-unknown on disconnect.** A heartbeat with `restarted: true` moves
  every still-`running` task on that node to `unknown` (reason
  `node_restarted`); `GET .../recoverable` lists them for a human decision.
  Nothing is replayed automatically.
- **A credential boundary.** The API stores credential references, never
  credential material. `ModelConfigCreate` forbids unknown fields (so `apiKey`
  is a `422`), and task payloads are scanned recursively for secret-shaped keys
  (`400 secret_rejected`).
- **An audit trail.** Every mutation writes one `audit_events` row; `GET
  /v1/tenants/{id}/audit` returns the tenant's events, newest first.

`GET /healthz` is unauthenticated. `GET /v1/metrics` returns tenant/node/task/
approval counts.

## Verified and unverified

Verified locally against SQLite, with the real FastAPI app, the real SQLAlchemy
models and the real Alembic migration (19 tests, plus a real uvicorn boot of
`python -m yukinal_control_plane` answering `/healthz`, `401` without a token,
and a tenant create + metrics round-trip):

```bash
.venv/bin/python -m unittest discover -s tests -p "test_*.py"
```

Not yet verified, and not implemented:

- **PostgreSQL itself.** `asyncpg` installs and the schema is dialect-neutral,
  but no PostgreSQL server exists in this environment, so the migration and the
  queries have not been run against PostgreSQL. JSON becomes `JSON` rather than
  `JSONB`, and `pool_pre_ping`/connection-pool behaviour under PostgreSQL
  restarts is untested.
- **The node channel.** Nodes reach the control plane over the same HTTP API
  here. The intended active HTTPS/WSS channel with node authentication (mutual
  TLS or signed node identities) is not implemented; the `publicKey` recorded at
  registration is stored but not yet used to authenticate a node.
- **Scale-out concurrency.** The partial index is enforced by the database, but
  multi-worker behaviour and request-level locking have not been exercised.
- **Metrics export.** `/v1/metrics` is a small JSON summary, not a Prometheus
  exporter.
- **Authorization roles.** A single shared token authenticates callers; there are
  no per-tenant roles yet.
