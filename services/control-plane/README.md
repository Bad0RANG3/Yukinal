# Control Plane (FastAPI + PostgreSQL)

The phase-4 control plane: a multi-tenant registry and task broker for desktop
hosts and their nodes. It is a separate service and does not change the
single-machine desktop path.

It owns the state the desktop host keeps locally but that a fleet needs shared:
tenants, nodes, tasks, provider/model selections, approvals and an append-only
audit log.

## Stack

- **FastAPI** + **uvicorn** for the HTTP API and the node WebSocket channel.
- **SQLAlchemy 2.0** (async) for the models and the session boundary.
- **Alembic** for schema migrations.
- **Ed25519** (`cryptography`) for node request signatures: the control plane
  stores a node's public key and never a node secret.
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
  actions.py                   which task kinds need approval, and the fingerprint
  nodeauth.py                  Ed25519 key parsing, canonical request, signatures
  service.py                   domain operations, one audit event per mutation
  channel.py                   the node WebSocket protocol and disconnect recovery
  app.py                       FastAPI factory: routes, auth, error mapping
tests/support.py               the real app on a real SQLite schema
tests/test_control_plane.py    API, schema, migration and boundary tests
tests/test_node_auth.py        registration and signature/replay boundaries
tests/test_node_channel.py     assignment, status and disconnect recovery
tests/test_approvals.py        approval binding and atomic task assignment
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
| `YUKINAL_CONTROL_TOKEN` | unset | Shared bearer token for the operator API. |
| `YUKINAL_CONTROL_ALLOW_ANONYMOUS` | `false` | Local-only opt-out of the token requirement. |
| `YUKINAL_CONTROL_BIND_HOST` | `127.0.0.1` | Bind address. Set explicitly to accept node traffic. |
| `YUKINAL_CONTROL_PORT` | `8765` | Bind port. |

## Two audiences, two credentials

**Operators** use the bearer token and the tenant-scoped routes under
`/v1/tenants/{tenant_id}/...`.

**Nodes** prove possession of an Ed25519 key on *every* request. Node-facing
routes live under `/v1/node/...`, take no tenant id, and derive the tenant from
the authenticated node row, so a node can never act on another tenant's data.

A node signs this canonical string over the method, the path (with query), the
timestamp, a random nonce and the SHA-256 of the body:

```
<timestamp>\n<nonce>\n<METHOD>\n<path[?query]>\n<sha256-hex(body)>
```

and sends `X-Yukinal-Node`, `X-Yukinal-Timestamp`, `X-Yukinal-Nonce` and
`X-Yukinal-Signature`. The timestamp must be inside a 300-second window, and the
nonce is recorded in `node_nonces` so it can be used exactly once — a replay is
`401`. An unknown node, a stale timestamp, a bad signature and a replayed nonce
all return the same `401`, so the API never confirms which node ids exist.
`nodeauth.sign_headers()` is the client half of that contract.

Registration accepts the public key as base64, hex, or an
`ssh-ed25519 <base64>` line and stores one canonical base64 form.
`GET /v1/tenants/{id}/nodes/{id}` exposes a `key_fingerprint` so a key can be
identified without echoing it.

## The node channel

A node connects out to `WS /v1/node/channel` with that same signed handshake.
On connect the control plane assigns every queued task for that node — moving it
to `running` is the moment work leaves the queue — and then the node reports
outcomes:

```
server -> node   ready {assigned, waiting}, assigned {task}, synced {assigned, waiting},
                 ack {taskId,status}, bye {marked}, error {code,message}
node -> server   status {taskId,status,outcome?,result?}, sync, close, pong
```

`waiting` lists queued work the node may not run yet, with a `reason` and the
action `fingerprint` that needs approving, so a stalled queue is reported rather
than looking idle.

If the socket drops while a task is running, the task becomes `unknown` with
reason `node_disconnected`, and `close` (a graceful shutdown) does the same with
reason `node_closed`. Either way the answer is "outcome unknown", never "failed"
and never an automatic replay: a node that reconnects may still report what
actually happened. The disconnect marking runs in a cancellation-shielded scope,
so a server shutdown cannot silently skip it. `GET /v1/node/recoverable` lists
the unresolved tasks for a human decision.

## Approvals gate effectful work

A task kind is dispatched only if it is a known read-only observation, or if an
*approved* decision matches its exact action. The kinds tracked as observations
are the host's own plan-bound reads (`server.info`, `server.logs`, `docker.ps`,
`docker.inspect`, `filesystem.read`, `systemd.inspect`, `package.inspect`,
`filesystem.backup.list`, `filesystem.backup.retention`, …); everything else —
including an unrecognised kind, and every `mcp.*` tool, whose risk the control
plane cannot resolve — needs an approval. That is the closed default.

An approval is requested with the action, the node and the payload; the server
computes the fingerprint from them (`sha256` of the canonical
`{kind, nodeId, payload}`, sorted keys, no whitespace) and stores the visible
input alongside it. The caller cannot supply a fingerprint, so it cannot name one
that does not match what it asked for. A decision applies only to the matching
fingerprint, so approving `systemctl restart nginx` on node A does not authorise
`rm -rf /` on node B, or the same command on a different node.

The gate is checked twice: when the task is created (the queue means "ready to
dispatch", so a task that can never run would otherwise hold the node's single
active slot) and again on the `queued → running` edge, which catches an approval
that lapsed in between. A refusal is `409 approval_required` and names the
fingerprint to approve. On the channel the same rule holds: a task that is not
cleared is not handed over, and `ready`/`synced` report it under `waiting` with
its reason and fingerprint instead of looking like an idle node.

Assignment itself is a conditional update (`... WHERE id = ? AND status =
'queued'`), so two workers that both selected the same queued task cannot both
hand it to a node: the loser's update matches no row. Reading the status into
Python and writing it back would let both through, which is the multi-worker
race.

## What the API enforces

- **Tenant isolation.** Every node, task, approval and model config is scoped to
  a tenant. A cross-tenant identifier is `404`, never `403`, so the API never
  confirms that a row exists in another tenant.
- **Approval before an effectful action.** See above: the approval is bound to
  the exact action by fingerprint, and both creation and dispatch check it.
- **One active task per node.** A partial unique index (`status IN ('queued',
  'running')`) makes the database, not just the handler, the concurrency
  control, and assignment claims the row with a conditional update. Terminal
  tasks accumulate freely.
- **A task state machine.** `queued → running → succeeded/failed/cancelled`, with
  `running → unknown` on a restart or a disconnect. An invalid transition is
  `409`.
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
models and the real Alembic migrations (68 tests):

```bash
.venv/bin/python -m unittest discover -s tests -p "test_*.py"
```

Also verified against a real uvicorn process, not the test transport: `/healthz`
returned `ok`; an unauthenticated request was `401`; a signed `POST
/v1/node/heartbeat` over a real socket was accepted; a real WebSocket client
completed the handshake, received its assignment, reported `succeeded` and got an
`ack`; and a client that dropped its socket mid-task left the task `unknown`
with reason `node_disconnected` and visible in `/recoverable`. The approval gate
was driven the same way: an effectful task was refused with the fingerprint to
approve, a pending approval still did not clear it, the matching approved action
was then dispatched and completed, a different payload was refused, and a task
whose approval was withdrawn before dispatch was reported under `waiting` and
left `queued`.

Not yet verified, and not implemented:

- **PostgreSQL itself.** `asyncpg` installs and the schema is dialect-neutral,
  but no PostgreSQL server exists in this environment, so the migrations and the
  queries have not been run against PostgreSQL. JSON becomes `JSON` rather than
  `JSONB`, and `pool_pre_ping`/connection-pool behaviour under PostgreSQL
  restarts is untested.
- **Transport security.** Nodes authenticate with request signatures, not mutual
  TLS, and the server binds plain HTTP/WS. TLS termination and `wss://` are a
  deployment concern that has not been exercised.
- **Two live workers.** Assignment is a conditional update and replay protection
  is database-backed, so both are correct across processes by construction, but
  no second worker process has been run against one database.
- **Authorization roles.** One shared operator token can act on any tenant; there
  are no per-tenant roles or scopes. In particular the same caller can request an
  approval and decide it.
- **Approval lifecycle.** A decision is bound to its fingerprint and is one-shot,
  but approvals never expire on their own, cannot be revoked, and the *requirement*
  is a fixed table rather than a per-tenant or per-environment policy. The
  `expired` state exists but nothing moves an approval into it.
- **Liveness.** There is no server-initiated ping timer; a node's `last_seen_at`
  is refreshed by its own messages, and a quiet node is noticed when its socket
  drops.
- **Metrics export.** `/v1/metrics` is a small JSON summary, not a Prometheus
  exporter.
