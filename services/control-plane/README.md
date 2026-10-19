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
  models.py                    tenants, nodes, tasks, model configs, approvals, policies, audit
  schemas.py                   request/response models (unknown fields forbidden)
  security.py                  the credential boundary (secret-shaped fields rejected)
  actions.py                   what a kind is (risk), what its tier does, and the fingerprint
  nodeauth.py                  Ed25519 key parsing, canonical request, signatures
  service.py                   domain operations, one audit event per mutation
  channel.py                   the node WebSocket protocol and disconnect recovery
  app.py                       FastAPI factory: routes, auth, error mapping
tests/support.py               the real app on a real SQLite schema
tests/test_control_plane.py    API, schema, migration and boundary tests
tests/test_node_auth.py        registration and signature/replay boundaries
tests/test_node_channel.py     assignment, status and disconnect recovery
tests/test_approvals.py        approval binding and atomic task assignment
tests/test_approval_lifecycle.py  expiry, withdrawal, use counts
tests/test_policy.py           the risk table, per-tenant policy and what it reaches
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

A node signs this canonical string over its own node id, the method, the path
(with query), the timestamp, a random nonce and the SHA-256 of the body:

```
<node-id>\n<timestamp>\n<nonce>\n<METHOD>\n<path[?query]>\n<sha256-hex(body)>
```

and sends `X-Yukinal-Node`, `X-Yukinal-Timestamp`, `X-Yukinal-Nonce` and
`X-Yukinal-Signature`. The node id is signed *and* sent as a header: the header is
unsigned routing data on the wire, so the signature is what binds the request to
the row that id names. Signing it is what makes a header swap fail — with the id
outside the signature, one key registered to a node in each of two tenants would
let the *same* signed bytes be replayed as either node, and only the header would
decide which tenant's rows were reached. A node driver must therefore know its own
node id when it signs; `nodeauth.sign_headers(private_key, node_id=..., ...)` is
the client half of that contract.

The timestamp must be inside a 300-second window, and the nonce is recorded in
`node_nonces` so it can be used exactly once — a replay is `401`. Two bounds keep
that record from becoming a write the caller controls: a nonce longer than 128
characters is refused before it reaches the database (the column is
`varchar(128)`; SQLite would store an over-long value verbatim and PostgreSQL
would raise instead of answering), and each node may hold at most 128 nonces still
inside the signature window. Both refusals are byte-identical to a signature
failure, as is an unknown node, a stale timestamp and a replayed nonce, so the API
never confirms which node ids exist or which check refused the request.

The cap is enforced by a single `INSERT ... SELECT ... WHERE count < cap`, not by
a count followed by an insert. That matters: the claim happens *before* signature
verification, and the earlier count-then-insert shape let every concurrent request
that passed the count first read the same stale value, so the table overshot by
roughly the number of concurrent callers (measured: 188 rows against a cap of 128
at 64-way concurrency, reachable without a valid key). The same test now stops at
exactly 128.

The cap bounds *unauthenticated garbage writes*; it is **not** a per-node rate
limit, but it does behave like a coarse one. A node that keeps more than
`128 / 300 s ≈ 0.43` requests per second inside the window will exhaust it, and —
because the refusal must stay indistinguishable from a bad signature — the node
is told its signature is invalid. Raise `MAX_OUTSTANDING_NONCES_PER_NODE` for a
deployment whose nodes poll faster than that; do not treat the default as a
capacity figure.

One public key backs exactly one node, globally — `uq_nodes_public_key`. A
per-tenant constraint would not be enough for the reason above: the two rows only
have to differ in tenant. Registering a key that is already in use is
`409 conflict`, and the response does not name the node or tenant that holds it.

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

A task kind is dispatched only if the tenant's policy lets its tier through
without a decision, or if an *approved* decision matches its exact action. By
default the kinds that run without one are the host's own plan-bound reads
(`server.info`, `server.logs`, `docker.ps`, `docker.inspect`, `filesystem.read`,
`systemd.inspect`, `package.inspect`, `filesystem.backup.list`,
`filesystem.backup.retention`, …); everything else — including an unrecognised
kind, and every `mcp.*` tool, whose risk the control plane cannot resolve — needs
an approval. That is the closed default, and a tenant may change it (see *the
tenant's policy* below).

An approval is requested with the action, the node and the payload; the server
computes the fingerprint from them (`sha256` of the canonical
`{kind, nodeId, payload}`, sorted keys, no whitespace) and stores the visible
input alongside it. The caller cannot supply a fingerprint, so it cannot name one
that does not match what it asked for. A decision applies only to the matching
fingerprint, so approving `systemctl restart nginx` on node A does not authorise
`rm -rf /` on node B, or the same command on a different node.

The gate is checked twice: when the task is created (the queue means "ready to
dispatch", so a task that can never run would otherwise hold the node's single
active slot) and again on **every edge whose target is `running`** — `queued →
running` and the `unknown → running` resume alike, because a resumption puts the
same effect back on the same node, and it catches a decision that lapsed or was
withdrawn in between, or a policy that changed under the task. A refusal is
`409 approval_required` and names the reason (`approval_expired`,
`approval_revoked`, …) and the fingerprint to approve; a task the policy refuses
outright is `403 action_denied`, because no decision could clear it. A refused
edge changes nothing at all: the status stays where it was and `attempts` is not
incremented. On the channel the same rule holds: a task that is not cleared is
not handed over, and `ready`/`synced` report it under `waiting` with its reason
and fingerprint instead of looking like an idle node.

Assignment itself is a conditional update (`... WHERE id = ? AND status =
'queued'`), so two workers that both selected the same queued task cannot both
hand it to a node: the loser's update matches no row. Reading the status into
Python and writing it back would let both through, which is the multi-worker
race.

## The life of an approval

A decision that never lapses and can be spent any number of times is not an
approval of an action, it is a standing grant. So an approved decision carries a
window and a budget, and stops authorising work three ways:

- **It lapses.** Approving sets `expires_at` to the decision time plus the TTL
  (900 seconds). Every check compares the clock itself, so an unswept row is
  already refused; the sweep exists so the *stored* state tells the truth, and it
  is what makes `expired` a real state rather than a label. The approval listing
  sweeps its tenant, and `POST .../approvals/expire` lets an operator or a cron
  drive it explicitly (idempotent, returns how many moved).
- **It is withdrawn.** `POST .../approvals/{id}/revoke` moves a `pending` or
  `approved` decision to `revoked`. Withdrawal does not rewrite history: the task
  keeps its record of what authorised it and simply refuses to start.
- **It runs out.** A request asks for `maxUses` (default 1, ceiling 100) and a
  decider may narrow that but never widen it. Creating a task spends one use
  atomically (`... WHERE uses < max_uses ... RETURNING`), so the spend is a
  conditional update for the same reason the task claim is.

Spending is deliberately *not* re-checked at dispatch: a one-shot decision would
otherwise fail the dispatch of the one task it authorised. What dispatch checks
is that the decision is still `approved` and unexpired. The task records
`approval_id`, so that check follows the authorisation rather than re-deriving
it, and the ledger says which decision paid for which task.

Timestamps are always serialised as explicit UTC instants (`...Z`). SQLite has no
timezone type and returns the naive wall clock it stored even for a
`timezone=True` column, so without that normalisation the same field would come
back different depending on whether it had been read from the database or echoed
from memory.

## The tenant's policy

The code above answers two questions that are deliberately separate. What a kind
*is* is a fact; what happens to it is a decision. The control plane writes the
decision down per tenant, in the same vocabulary as the shared risk layer
(`packages/shared/src/types/risk.ts`), so the two cannot drift into two languages.

A kind's **risk** comes from, in order: the host's own declaration for its own
built-in tools, this tenant's declaration, or the closed default (`high`). The
host declares two sets and both are mirrored exactly — its plan-bound
observations read, and its named state-changing tools (`server.exec`,
`docker.restart`, `systemd.restart`, `package.install`, `filesystem.write`,
`filesystem.edit`, `filesystem.backup`, `filesystem.backup.cleanup`,
`filesystem.restore`) always change the target. Everything else, every `mcp.*`
kind included, is something the control plane cannot resolve the way the host's
MCP supervisor can, so it is `high` until a tenant says otherwise.

That risk collapses into a permission tier (`read`/`write`/`dangerous`) and the
tenant's **tier modes** decide the outcome:

| mode | meaning |
| ---- | ------- |
| `auto` | runs without a decision |
| `ask`  | needs an approved decision |
| `deny` | refused outright, at creation and at dispatch |

`GET /v1/tenants/{id}/policy` reports the effective policy (with `stored: false`
and the production defaults when the tenant has none); `PUT` sets it. Naming an
`environment` derives its tier table — `local` and `production` ask for a write,
`development` and `staging` let it through, and an unknown environment is treated
as production so it can never be *more* permissive — while sending `tierModes`
explicitly is how a tenant departs from that table without changing what the
environment means. `DELETE` drops the row so the tenant falls back to the
defaults. The same `PUT` carries `approvalTtlSeconds` (60–86400), which is the
window a decision gets in that tenant.

`GET /v1/tenants/{id}/tool-policies` lists every kind the control plane can name
with its risk, tier, mode and where the risk came from (`host`, `declared` or
`default`); `PUT .../tool-policies/{kind}` declares one and `DELETE` clears it.
Declaring a kind is how a tenant says it trusts a `mcp.*` tool the control plane
does not ship. Two things cannot be declared away: the host's named effectful
tools, whose requirement is checked *before* any declaration in the decision
itself (not only in the endpoint that refuses to store one), and the `dangerous`
tier, which is never `auto`. So there is no configuration, however written, that
lets `server.exec` run without a decision.

Tier modes are stored resolved rather than derived on read: changing a built-in
default later cannot silently re-open a tenant that had already chosen.

## What the API enforces

- **Tenant isolation.** Every node, task, approval and model config is scoped to
  a tenant. A cross-tenant identifier is `404`, never `403`, so the API never
  confirms that a row exists in another tenant.
- **A disabled tenant is closed everywhere.** `tenants.disabled` takes a tenant
  out of service, and *every* route under `/v1/tenants/{tenant_id}` answers `404
  not_found` for it — the same answer a missing tenant gets, so the surface cannot
  be used to probe which tenants exist. The check lives in the shared resolvers
  (`require_tenant`, and `require_node`/`require_task`, which call it first), not
  in individual handlers, so a route added later inherits it rather than having to
  remember it. A disabled tenant's nodes are refused too: node authentication
  answers the same `401` as an unknown node.
- **Approval before an effectful action.** See above: the approval is bound to
  the exact action by fingerprint, it has a window and a use budget, and both
  creation and dispatch check it.
- **A tenant's policy decides, and a denial is absolute.** Which tier needs a
  decision and for how long are per tenant, and a tier set to `deny` refuses work
  at creation and stops a task already authorised, because no decision can clear a
  denial. Stored modes are the authority; the API refuses a declaration that would
  lower the host's own effectful set, and refuses `dangerous: auto`.
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
models and the real Alembic migrations (148 tests):

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
left `queued`. A decision's window and its use budget were driven the same way:
approving set a window, a task spent the single use, a second task was then
refused while the task that was already authorised still started, a backdated
decision was swept to `expired` (idempotently) and stopped its queued task with
`waiting.reason = approval_expired`, a withdrawal did the same with
`approval_revoked`, and a decider narrowing `maxUses` from 3 to 1 left exactly one
task authorisable. The tenant policy was driven the same way too: an undeclared
`mcp.*` kind was refused under the closed default, a declaration of `read` was
accepted and let it run with no approval recorded, redeclaring the host's
`server.exec` was `409`, the tenant's own TTL put a 120-second window on the next
decision, `dangerous: auto` was `422`, and setting `dangerous: deny` refused a new
`server.exec` with `403 action_denied` *and* stopped the one already authorised,
which the channel then reported as `waiting.reason = policy_denied`.

The hardening claims above were measured, not only asserted. Reverting just those
edits in a copy of the tree and re-running the same probes gave: the same signed
headers reached a task list in the *other* tenant with only the unsigned
`X-Yukinal-Node` header changed (`true` before, `401` after), and registering one
key in two tenants was `201` twice before against `409` after; a resume of a
denied action returned `200` and moved the task to `running` with `attempts` 1 → 2
before, against `403 action_denied` with the status still `unknown` and `attempts`
still 1 after; 6 of the 24 `{tenant_id}` routes (`GET`/`POST` on a node,
`recoverable`, `GET`/`POST` on a task, `POST .../approvals`) still answered a
disabled tenant with `200`/`201` in a copy of this tree with exactly the four edits
above reverted, against none after — and the same probe against `HEAD`, which
predates the previous round of fixes as well, reports 9; a 100000-character nonce
was stored (`MAX(LENGTH(nonce)) = 100000`) and the request was `200` before,
against `401` with zero rows after; and 200 garbage-signature requests wrote 200
`node_nonces` rows before, against exactly 128 after (the cap), with a correctly
signed 129th request from the same node refused for the same reason.

Each item was also reverted on its own, with the rest of the tree left in place, so
the tests that pin it can be named rather than inferred: item 1 turns 5 tests red
(`test_canonical_request_binds_the_node_id`,
`test_a_signature_for_one_node_id_does_not_verify_for_another`,
`test_a_signature_cannot_be_moved_to_another_node_by_its_header`,
`test_a_public_key_cannot_back_a_node_in_two_tenants`,
`test_migration_matches_the_models`); item 2 turns exactly the 2 resume tests red;
item 3 turns 7 rows of the disabled-tenant sweep red — the 6 the probe reaches plus
`POST .../tasks`, which returns `201` in the test because its node has no active
task while the probe's fixture node is already busy; item 4 turns the 2 nonce tests
red. Reverting everything at once turns 43 red, because dropping the node id from
the signing helper breaks every caller of it — which is why the per-item runs keep
each signature intact and change only the behaviour under test.

## Upgrading an existing deployment

Two changes in this revision are not backwards compatible, and both are
deliberate:

1. **The signature now covers the node id.** A node driver that signed the old
   canonical string must be updated in the same step as the server, otherwise
   every request it makes is `401`. The server cannot accept both strings: the
   old one is exactly the ambiguity the node id closes. Roll the server and the
   node drivers together.
2. **`nodes.public_key` is globally unique**, so a deployment that registered one
   key on two nodes cannot install the constraint until that is resolved.
   `b7c41d0e9a2f` checks for duplicates *before* altering anything and refuses
   with a `SHA256:<16 chars>` fingerprint per duplicated key (up to 10) rather
   than failing inside an `ALTER TABLE` and leaving the schema half-changed — a
   failed run leaves the version at `fc2e168fa85c`, so re-running is safe. Re-point
   the extra node (register its own key, or delete it) and re-run the upgrade.
   The migration deliberately does not pick a winner: which of the two rows is the
   real node is a fact only the operator has.

## Not yet verified, and not implemented

- **PostgreSQL itself.** `asyncpg` installs and the schema is dialect-neutral,
  but no PostgreSQL server exists in this environment, so the migrations and the
  queries have not been run against PostgreSQL. JSON becomes `JSON` rather than
  `JSONB`, and `pool_pre_ping`/connection-pool behaviour under PostgreSQL
  restarts is untested.
- **Transport security.** Nodes authenticate with request signatures, not mutual
  TLS, and the server binds plain HTTP/WS. TLS termination and `wss://` are a
  deployment concern that has not been exercised.
- **Two live workers.** Assignment, use spending and replay protection are all
  conditional updates or database-backed, so they are correct across processes by
  construction, but no second worker process has been run against one database.
- **Authorization roles.** One shared operator token can act on any tenant; there
  are no per-tenant roles or scopes. In particular the same caller can request an
  approval and decide it, the same caller can decide and revoke it, and the same
  caller can rewrite the policy those decisions are made under — a policy change
  is not itself an approved action.
- **Per-kind risk the control plane does not ship.** A `mcp.*` kind runs without a
  decision only because a tenant declared it safe; there is no catalogue that
  resolves it from the MCP server the way the host's supervisor does, so the
  declaration is the whole answer.
- **Liveness.** There is no server-initiated ping timer; a node's `last_seen_at`
  is refreshed by its own messages, and a quiet node is noticed when its socket
  drops.
- **Metrics export.** `/v1/metrics` is a small JSON summary, not a Prometheus
  exporter.
