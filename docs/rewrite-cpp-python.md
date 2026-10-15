# C++ / Python rewrite: milestones 1–3 in progress

> **Current scope:** the standalone C++ SSH service and Python caller are in
> place. The Python Agent Runtime has bounded LangGraph tool execution, stdio and
> Streamable HTTP MCP support, OpenAI-compatible/Anthropic/Gemini adapters, a
> desktop-host bridge for approval, target resolution, and remaining tools, plus tool lifecycle events consumed by the
> existing UI. Tauri development and new package builds now select the Python
> sidecar; installer resources contain Python Agent and C++ SSH assets only.
> The desktop resolver has no legacy Node resource or runtime fallback; old Node paths below refer only to historical migration evidence.
> Rust still
> hosts native UI IPC, local trust-store updates, final approvals, and SSH
> compatibility paths for configurations outside current C++ coverage: Host CA,
> HTTPS proxies, unsupported proxy bypass rules, keyboard-interactive auth,
> known-hosts-only trust, and other unsupported SSH modes. Supported desktop
> terminals use a dedicated C++ PTY child process. The Python sidecar can call the C++
> `service_status` operation when configured; the desktop Agent routes server
> logs, Docker inspection, systemd inspection, and package inspection through a
> short-lived C++ process using the saved desktop target and identity. The desktop file browser and
> manual transfers use C++ SFTP operations for password, private-key, OpenSSH user-certificate, and
> SSH-agent identities on direct or supported HTTP CONNECT routes with a confirmed host-key pin and
> without Host CA. The same identities use C++ for
> structured SSH operations. Host-key probes use C++ on direct and supported HTTP
> CONNECT routes; Host CA, unsupported proxy routes, or unsupported host-key formats use Rust. Manual transfers use
> bounded C++ SFTP streams for supported direct or HTTP CONNECT targets with a confirmed host-key
> pin and retain the Rust compatibility path for unsupported targets. PTY and
> Host CA trust still use Rust where unsupported.
> Every C++ operation resolves its `target_id` through the service
> allowlist. The C++ executable is included in desktop resources; when an
> operator supplies `YUKINAL_SSH_SERVICE_CONFIG`, Python starts it, checks its
> loopback health endpoint, and stops it with the Agent process. Automatic
> server-record synchronization and full desktop task/plan lifecycle recovery
> remain unfinished. On startup, a durable task whose exact
> action step was waiting for approval and has no matching host execution-ledger
> entry can reattach to its LangGraph checkpoint and receive a fresh host
> approval ticket; other interrupted actions stay outcome-unknown and are never
> automatically replayed. The Python sidecar supports approval events and
> response RPCs for registered tools that require approval, including reject
> and expiry handling. Its desktop catalog includes read-only tools plus
> plan-bound `server.exec`, `docker.restart`, `systemd.restart`, and
> `package.install` actions only for a remote goal run with an active action step
> that allows them. These actions require exact per-call approval; Rust fingerprints
> the exact host input and consumes a one-time ticket before dispatch. On
> C++-supported routes, `systemd.restart`, `docker.restart`, and
> `package.install` now run from Python through structured C++ SSH operations.
> The host checks the durable action plan before
> claiming the call and consuming its ticket. A successful result is cached only
> after C++ reports exit code zero; nonzero and transport outcomes stay uncertain
> and cannot be replayed. Rust retains the
> compatibility executor only for routes it explicitly reports as unsupported.
> Raw approval input is removed before persistence and UI emission.
> Agent `filesystem.write` and `filesystem.edit` now run as Python desktop tools
> through the C++ SFTP service when the target route supports it. The host checks
> the current plan and exact path/content limits before returning a one-target
> credential context. Edit keeps the complete-read revision, unique byte match,
> regular-file/single-link checks, and C++ guarded atomic replacement. Python
> requires exact per-call approval and records each call fingerprint in a private
> SQLite action ledger before SFTP starts. A completed call returns its cached
> result; an interrupted call stays outcome-unknown and is never replayed.
> `filesystem.backup` now follows the same Python/C++ path: the host creates the
> sibling backup name and validates the plan, Python checks file type, size,
> stability and single-link ownership, and C++ creates the copy exclusively.
> The host then rechecks the plan and stores ownership metadata in its durable
> SQLite ledger. If registration is uncertain, Python preserves the remote
> copy and leaves the call outcome unknown. Unsupported routes use the Rust
> host executor. `filesystem.restore` now also follows the Python/C++ path. The
> host checks task ownership and backup status, Python verifies both the target
> revision and backup digest, and C++ publishes through guarded atomic SFTP;
> the host consumes the backup ledger row only after the replace succeeds.
> `filesystem.backup.cleanup` now follows Python/C++ for supported targets.
> Python verifies each backup's type, size, link count, stability and digest
> before C++ removes it; the host rechecks ownership and consumes each ledger
> row. Approved rotation batches retain per-item partial results. Listing and
> retention metadata stays in the host database, while Python formats scoped
> listings and computes bounded rotation candidates.

## Repository baseline and migration boundary

The repository contains a working React/Tauri product baseline and shared
TypeScript contracts. The previous rewrite draft mixed SSH, SFTP, PTY, database,
credentials, terminal, MCP, approval, Agent, and UI bridge code before the first
SSH service had a verified end-to-end boundary. The implementation below is
being migrated in checked stages. React components and their Tauri calls are
preserved; the Rust/Tauri host remains responsible for the native UI bridge,
OS credential store, and final host authorization while Agent execution moves
to Python and structured Agent SSH calls move to the C++ service.

The Python runtime speaks the desktop host's bidirectional JSON-RPC protocol for
approval, target resolution, and remaining host operations; it also loads low-risk
MCP tools from the host catalog and emits the tool-call/result events consumed by
the existing transcript UI. With the packaged C++ service available, desktop
`server.logs`, `server.services`, Docker read operations, `systemd.inspect`, `package.inspect`, and
`ssh.service_status` use a short-lived, one-target C++ process. Agent
`filesystem.read` also runs through Python and the C++ bounded SFTP read; before
the desktop returns a target to Python, Rust checks the requested path and byte
limit with the existing `AgentReadRequest` policy. Python parses `server.logs`,
Docker list/log/inspect, `systemd.inspect`, and `package.inspect` output into the
existing structured result shapes. Rust supplies the
saved target, confirmed host-key pin, and credential material from the OS store;
the Python process passes credentials only through the C++ child environment and
cleans up its temporary configuration when the call ends. If Rust reports that
the target route is unsupported or the local C++ process cannot start, the same
read-only request falls back to the existing host route. SSH connection and
host-key errors do not trigger a fallback.
The Python Agent catalog exposes `docker.restart`, `systemd.restart`, and
`package.install` as structured actions with exact per-call approval. The host
recomputes a fingerprint from the exact raw input and consumes the corresponding
one-time ticket. All three actions are routed through the Python caller to
structured C++ SSH operations on supported targets; the host records their
terminal outcomes in the durable execution fence. Rust keeps the compatibility
executor for routes it explicitly reports as unsupported.
`server.exec` remains separately guarded and exposed only when the host supplies
the current remote action-plan binding. On C++-supported targets, the host first
consumes the exact approval ticket or reserves the bounded task command grant,
then starts a one-shot C++ service config that enables `server.exec` for that
target only. Persistent service target configs deny it by default. Targets that
still require a Rust-only SSH route retain the existing Rust compatibility path.
The Python sidecar exposes bounded `filesystem.read`; Agent `filesystem.write`,
revision-guarded `filesystem.edit`, `filesystem.backup`, `filesystem.restore`,
and `filesystem.backup.cleanup`
use Python approval, host plan/path checks, a durable exact-call ledger, and the
C++ SFTP transport for supported routes. Backup ownership metadata stays in the
host database; the host chooses its unguessable sibling name and confirms
registration after the remote exclusive create. Restore verifies the current
target revision and the backup's ledger digest before guarded replacement, then
asks the host to consume the available ledger row. Cleanup verifies each exact
backup and lets the host consume each row after C++ removal; cross-task deletion
is accepted only for an exact item in an approved `backup_rotation` step.
Unsupported routes use the existing Rust host executor. Backup listing queries
remain task-scoped at the host database boundary, where Rust revalidates the
durable task, target and plan binding and records the call identity. Python maps
the bounded rows into Agent results and performs retention ordering, age and
keep-count selection over a bounded host scan.
Task-scoped, body-free `investigation.evidence.search`/`compare` also run through
the desktop host.
`pnpm --filter @yukinal/desktop tauri dev` selects this runtime for Tauri
development; `pnpm desktop:dev:python` is an equivalent shortcut and
`pnpm desktop:dev` remains the browser-only UI preview. `pnpm package` freezes
the Python runtime with PyInstaller, stages it under `python-agent/`, and
validates the Python/C++ resource layout before Tauri builds. Node is no longer
downloaded or staged in new installers. The Rust resolver retains a compatibility
branch for older packaged layouts, but the new Tauri installer has not been built
in this environment because Cargo and rustc are unavailable.

## Processes and trust boundaries

```text
Python Agent Runtime
    ├── Tool Registry → schema → per-run tool/plan allowlist → audit
    ├── LangGraph bounded tool workflow
    ├── host.* JSON-RPC bridge → Tauri/Rust host → remaining host tools and MCP
    └── yukinal_ssh caller → one-target C++ service (Agent SSH and file tools)
              │ HTTP/JSON + local bearer token
              ▼
        C++ SSH service (binds only 127.0.0.1)
              │ target_id lookup → operation policy → known_hosts check
              ▼
        libssh2 ── SSH ──► configured server

React/Tauri desktop UI: unchanged; the host executes `host.*` tools, while the
C++ service executes migrated structured SSH and SFTP operations
```

The bridge uses the existing sidecar request path: Python sends
`host.tool.execute` (or `host.mcp.catalog`) over the child process pipe, and the
desktop host replies over that same pipe. Python does not start MCP processes
through this bridge. For `filesystem.write`, `filesystem.edit`,
`filesystem.backup`, `filesystem.restore`, and `filesystem.backup.cleanup`, the
host checks the current plan, validates the path and request limits, and returns
a one-target credential context; Python then starts the C++ service and performs
bounded SFTP. Edit reads and checks the requested
revision in Python before asking C++ to publish with its size/mtime/content
digest guard. Backup reads a stable regular file no larger than 1 MiB, requires
exactly one hard link, and creates the host-derived sibling with exclusive SFTP
creation. A separate host RPC revalidates its action binding before adding the
remote copy's metadata to the ownership ledger. Restore checks the exact available
backup, owning task, current target revision, backup content digest, and both
single-link counts before C++ publishes the backup with a guarded atomic replace.
The host marks the backup as restored only after publication succeeds. Cleanup
checks the ledger-owned revision and byte count, verifies the remote backup's
regular-file type, single-link count, stable metadata and SHA-256 in Python, then
removes it through C++; the host consumes the ledger row only after a confirmed
remove. Other host
tools continue through the host executor. For
investigation calls, Python carries the current durable plan,
step, and evidence binding; high-risk host actions also carry the host-issued
per-call approval ID. The approval event carries raw host input only
across the local pipe; Rust fingerprints it and removes it before persistence or
UI emission, then checks the fingerprint again before dispatch. MCP entries are
exposed to the model only when the host catalog classifies them as low-risk.

The direct C++ caller is enabled only when `YUKINAL_SSH_API_TOKEN` is explicitly
forwarded to the Python Agent sidecar. The C++ configuration's `target_id` must
match the selected desktop server ID. When service autostart is configured, the
Tauri host passes only the config's named token and authentication environment
references to Python; Python gives those values to the C++ child through an
allowlisted environment. Automatic synchronization with saved server records
and OS keychain identities remains unfinished.

## Python Agent Runtime

The `yukinal_agent` package is an independent runtime layer alongside the
stdlib-only `yukinal_ssh` client. The registry rejects duplicate names, checks
each JSON Schema before invoking a handler, and re-evaluates the caller's
allowed tools and target IDs on every invocation. Write/destructive tools
require an approval grant fingerprinted against the exact tool, target and
parameters. The embedding host must provide authenticated identity and
allowlists; model output cannot provide or widen them.

Audit has an interface, an in-memory test sink and an append-only JSONL sink.
It stores the operator/task/target/tool, argument names and types, timing,
result shape, approval status and stable error code, but omits raw argument and
result values. Audit write failure blocks tool execution.

`MCPClientManager` supports stdio and Streamable HTTP. HTTP credentials and
stdio environment values resolve from explicitly named environment references;
remote cleartext HTTP is limited to loopback. Discovered names are namespaced
and registered atomically. MCP descriptions, annotations and results are
external data. A response is checked before the Tool Registry copies it and is
rejected if it is not JSON-compatible, cyclic, deeper than 32 levels, larger
than 4 MiB, above 100,000 nodes, or above 10,000 content blocks. Discovered
tools require approval unless an administrator's local `read_only_tools`
allowlist classifies the exact tool as read-only.

`AgentWorkflow` is a minimal LangGraph graph that accepts explicit tool calls,
executes them sequentially, stops on the first error, and applies per-tool,
total-time, tool-count and output-byte limits. The production sidecar stores
`ModelDrivenAgent` checkpoints in SQLite below `YUKINAL_DATA_DIR/agent-state`;
stand-alone callers without that data directory retain the in-memory saver.
The persisted state is bound to the run identity, prompt, authorization scope,
and budget. A pending approval can be restored for the same run after the saver
is reopened. Desktop startup reattaches only when the durable run and its one
active action step both say they are waiting for approval and the host execution
ledger has no record for that call; it reissues a fresh host ticket. A matching
host execution record, missing or mismatched checkpoint, or ambiguous step
state fails closed instead of retrying the action. An interrupted action step
without that precise waiting state remains outcome-unknown. This does not yet
recover the full desktop task/plan lifecycle or make interrupted tool
executions exactly-once.
`ModelDrivenAgent` pauses and resumes an exact approved call; rejection and
expiry stop the graph without retrying the requested action.

Service discovery currently targets Unix-like hosts with systemd and Docker.
The service configuration is the allowlist. Requests cannot provide a hostname,
port, username, or credential. The service resolves a
`target_id` to a configured target and accepts only `connection_check`, bounded
`system_snapshot`, `docker_ps`, bounded `docker_logs` and `docker_inspect`,
`systemd_inspect`, `package_inspect`, `service_status`, `service_inventory`,
bounded read-only `server_logs`, probe-only `host_key_probe`, and bounded SFTP
list/stat/read/write/exclusive-create/remove/link-count operations with
validated parameters. `server_exec` is the only command-text operation; it is
denied unless that target has an explicit `allow_server_exec` capability, which
the desktop sets only in its one-shot config after host-side approval and task
policy checks. Service operations map to fixed commands apart from this guarded
capability; SFTP reads are
capped at 1 MiB plus one sentinel byte, normal writes at 512 KiB, and exclusive
recovery copies at 1 MiB. Write bodies use base64 inside a bounded JSON
request. The link-count operation runs one fixed, single-quoted `stat` probe
and returns `null` when the host cannot report a count. The host-gated restart
and package-install commands are built from validated
structured fields and do not accept command text. `docker_restart` accepts a
safe container name and `grace_period_seconds` from 1 to 120; its SSH request
deadline remains 1–30 seconds. `systemd_restart` accepts a safe `.service`
unit and `timeout_seconds` from 1 to 120. `package_install` accepts only `apt`
or `dnf`, a safe package name, an optional validated version, and
`timeout_seconds` from 1 to 600. For the latter two operations, the nested
timeout must equal the request's top-level timeout. The Python Agent runs
approved restart and package-install operations through the C++ SSH service;
the desktop host continues to enforce approval, parameter binding, and one-time
call handling before dispatching these actions.
Every authenticated operation runs only after the target host key matches the
configured OpenSSH `known_hosts` file or exact desktop pin. Unknown and changed
keys fail closed. The separate `probe_only` operation intentionally does not
authenticate or persist trust; its fingerprint is only presented for the
existing user confirmation step.

The API token is read from an environment-variable reference in the config and
must be 32–256 URL-safe characters. The HTTP handler rejects browser `Origin`
headers, does not enable CORS, limits request size, and never logs request
bodies. Credentials are read from a referenced environment variable or a
private-key file, and are not printed to logs or returned in errors.

## C++ service API

### `GET /health`

Returns `{"status":"healthy"}`. This liveness endpoint does not require a
token.

### `POST /v1/ssh/exec`

Requires `Authorization: Bearer <local-token>` and `Content-Type:
application/json`.

Request:

```json
{
  "target_id": "staging-web-01",
  "operation": "service_status",
  "parameters": { "service": "nginx.service" },
  "timeout_seconds": 10
}
```

The desktop inventory request uses the same endpoint with no parameters:

```json
{
  "target_id": "staging-web-01",
  "operation": "service_inventory",
  "parameters": {},
  "timeout_seconds": 10
}
```

Only the documented fields are accepted. `service_status` requires exactly one
validated unit name; `connection_check` and `service_inventory` accept no
parameters. `system_snapshot` accepts only a fresh 32-character lowercase
hexadecimal nonce used to delimit its fixed collector results; `docker_ps`
accepts only a boolean `all` filter. `docker_logs` validates its container
reference and caps its tail to 500 lines; `docker_inspect` accepts only a safe
container reference. `systemd_inspect` accepts only a safe `.service` unit;
`package_inspect` accepts only `apt` or `dnf` and a safe package name.
`server_logs` accepts an optional time
range from 1 second to 7 days and an optional bounded
`.service` name; its result is capped at 500 lines. When either filter is set,
the service does not fall back to unfiltered system log files. SFTP operations
accept only their documented path, byte limit, or base64 content fields.
`sftp_write` accepts at most 512 KiB, `sftp_create_exclusive` at most 1 MiB,
and `sftp_read` at most 1 MiB plus a sentinel byte. Read-only command and SFTP
timeouts are 1–30 seconds; the three structured host-gated actions have the
specific limits described above. `server_exec` requires a target config with
`allow_server_exec: true`, which is disabled by default and is set automatically
only in the desktop's one-shot config after host authorization. Its remote shell
request is bounded to 128 KiB, timeout to 1–120,000 ms, and combined stdout/stderr to the
requested 1–65,536-byte budget. The SSH service enforces these execution limits;
the desktop host remains responsible for exact approval, task scope, critical
command checks, and command budgets.
Service command output is capped at 64 KiB per stream; SFTP responses have
operation-specific limits.

The desktop service panel and supported file browser calls launch a short-lived
one-target C++ service. Its temporary configuration contains only target
metadata and environment-variable names; credentials are passed to the child
process environment and the process is stopped after the request.

Completed request:

```json
{
  "status": "succeeded",
  "target_id": "staging-web-01",
  "operation": "service_status",
  "exit_code": 0,
  "stdout": "active\n",
  "stderr": "",
  "duration_ms": 120,
  "output_truncated": false,
  "error": null
}
```

The HTTP result is successful when SSH returned a known command exit status;
`exit_code` can be nonzero when the service is inactive. Transport failures
return `status: "failed"` and a stable error code such as `connection_failed`,
`host_key_unverified`, `authentication_failed`, `timeout`, `output_limit`, or
`result_unknown`.

## Configuration

Start from [`configs/ssh-service.example.json`](../configs/ssh-service.example.json).
Paths are resolved relative to that JSON file. Each target requires:

- a unique, restricted `target_id`;
- a configured host, port, and SSH username;
- exactly one host-key trust source: an OpenSSH-format `known_hosts_file`, or
  `host_key_fingerprint` containing the desktop pin in `SHA256:<base64>` form;
- exactly one auth mode: `private_key_file` or `private_key_env`, a
  `password_env` reference, or `{ "type": "agent" }`. Private-key auth can
  include `passphrase_env`; OpenSSH user-certificate auth additionally sets
  `public_key_file` to its `*-cert.pub` file. The certificate is public
  material; the private key remains in its configured file or environment
  reference.

Agent authentication uses `SSH_AUTH_SOCK` on Unix and libssh2's platform Agent
integration on Windows. The socket path is forwarded only when an allow-listed
target selects agent authentication. For certificate identities, the C++
service reads the configured public certificate line and passes it to libssh2
alongside the private key; the desktop does not copy certificate bytes into
the secret store. The desktop pin form lets the service
enforce the same exact-key trust decision as the existing server host-key UI.
Private-key environment references let the desktop pass decrypted keychain
material directly to the C++ child without writing key material into the JSON
configuration.

The desktop may also add a target-level `proxy` object with `type: "http_connect"`,
proxy `host` and `port`, an optional `auth_env`, and supported `no_proxy` host
patterns. Exact hosts, domain suffixes, IPv4 prefixes, IPv4 and IPv6 CIDR ranges,
and `*` are evaluated by
the C++ service against its single target; unsupported patterns retain the Rust
path. Proxy credentials are forwarded to the child environment separately from
the target JSON. HTTPS proxies also stay on the Rust path. The C++ service bounds the
CONNECT response and shares the SSH operation deadline.

Desktop host-key probes use a separate short-lived `probe_only` target with no
credential and no trust entry. The service permits that target to perform only
`host_key_probe`; the returned fingerprint remains untrusted until the user
confirms it through the existing UI. Targets configured with a host CA retain
the Rust probe path because it reports the certificate's underlying
public-key fingerprint.

Set the configured API token environment variable before starting the service.
Never commit credentials or a real production target configuration.

## Python caller

The `yukinal_ssh` package in [`agent/`](../agent/) uses only the Python standard
library and supports Python 3.11+. It validates inputs before sending them and
validates the response before returning a `ServiceStatusResult`. Structured
remote errors are raised as `SshServiceError` with a stable `code` and
`http_status`. Install Agent dependencies with `python -m pip install -e
"agent[agent]"` to use LangGraph and MCP.

The desktop Agent routes `server.info` command acquisition through the C++ SSH
service using a fixed command set and nonce-delimited output. Python validates
the markers, parses the seven snapshot sections, computes health and collector
results, and sends normalized data through the existing guarded
`host.tool.execute` route. The host validates the typed snapshot shape, adds its
own target identity and collection timestamp, and persists the existing UI
contract to SQLite. The Overview's direct snapshot request uses this same
Python normalization RPC. Legacy SSH-only configurations still use the Rust
collector fallback while their transport path is migrated.

## Build and verification

```powershell
cmake -S native -B native/build
cmake --build native/build --config Release
ctest --test-dir native/build -C Release --output-on-failure
```

The CTest suite covers config/schema validation, operation policy, Python client
behavior, and a live Python-to-C++ HTTP path including health, allowlist
rejection, origin rejection, and SSH connection failure classification. Install
`agent[agent,test]` before CMake configuration to register Agent Runtime/MCP
tests, the Paramiko SSH mock for success, bad credentials, command timeout,
output limit, and unknown/changed host keys, and an Agent-to-C++ mock path that
checks selected-server binding, task target authorization, and model target
override rejection. The separate OpenSSH container test checks the SSH cases
against OpenSSH.

Local verification on 2026-10-15: Windows 11, Visual Studio 2022 17.14.39,
CMake 4.4.4, and Python 3.14.8 successfully configured and built the C++ service
and unit-test executable. With the Visual Studio developer environment active,
the verified commands were:

```powershell
cmake -S native -B native/build-nmake -G "NMake Makefiles"
cmake --build native/build-nmake --config Release
ctest --test-dir native/build-nmake -C Release --output-on-failure
```

CTest passed all six registered targets: C++ config/policy/network tests, Python client
tests, the live Python-to-C++ HTTP integration tests, Agent Runtime tests, the
Paramiko SSH mock integration, and the Agent-to-C++ mock integration. The HTTP
path verified health, allowlist rejection, browser-origin rejection, and SSH
connection-failure classification. A deliberately stalled DNS resolver
returned at its operation deadline, and the SSH mock path verified successful
read-only execution plus bounded command timeout, auth, output-cap, and
host-key rejection. Agent tests verified selected-server binding and rejected a
task-disallowed target and a model-supplied target override. The separate
OpenSSH test container was not run because Docker is unavailable, so
compatibility with an OpenSSH server remains unverified.

Verification on 2026-10-15 used Python 3.14.8 with LangGraph 1.2.15,
MCP Python SDK 2.3.0 and jsonschema 4.26.0 in the ignored `agent/.venv`
environment. The C++ build and all six CTest targets passed on the preceding
verified revision. On the current Python tool-routing changes, full Agent
discovery ran 74 tests: 71 passed and 3 service integration cases were skipped
because no service binary path was configured for that standalone run. The
focused SSH client suite passed 16 tests, and the desktop SSH routing and process
suite passed 13 tests. CTest was rerun for this round; all six targets passed. An
earlier full Python
unittest discovery ran 45 tests: 42 passed and 3 service integration cases were
skipped because the service binary path was not configured for that standalone
run. Rust host bridge unit tests were added but could not be run in this shell
because Cargo and rustc are unavailable. The Agent suite covers live stdio and Streamable HTTP MCP
connections, credential-reference handling, schema/target rejection,
parameter-bound approvals, approval RPC resume/reject, audit redaction,
tool/task/output limits, LangGraph execution, bidirectional host RPC framing,
and redacted tool lifecycle events. The Paramiko SSH mock and
Python-to-C++ HTTP tests also passed in CTest.

On 2026-10-15, after routing `server.info` through the C++ snapshot batch, full
Python unittest discovery ran 76 tests: 73 passed and 3 service integration
cases were skipped because no service binary path was configured for that
standalone run. The focused SSH client suite passed 18 tests and the desktop SSH
routing suite passed 8 tests. CTest passed all six registered targets. A Rust
host-tool test now covers snapshot parsing and persistence, but could not be run
because Cargo and rustc are unavailable in this environment.

On 2026-10-15, after routing approved `server.exec` calls through the one-shot
C++ service on supported targets, the Windows C++ build succeeded and all six
CTest targets passed. The SSH mock covers successful command output, combined
stdout/stderr truncation, and the persistent-config default-deny behavior.
Python syntax checks and `git diff --check` passed. The Rust host bridge changes
could not be compiled here because Cargo and rustc are unavailable.

On 2026-10-15, after registering the full filesystem tool set in Python,
`pnpm check` passed, including all six CTest targets, workspace typechecks and
tests, the desktop bundle, and the Python sidecar smoke. Standalone Python Agent
discovery ran 92 tests: 88 passed and 4 service integration cases were skipped
because that standalone invocation had no C++ binary path; the CTest run passed
the configured Python Agent, C++ HTTP, SSH mock, and Agent-to-C++ integration
targets. `git diff --check` passed. Rust formatting, clippy, Cargo check, and
Rust-to-sidecar integration were skipped because Cargo is not installed on
`PATH`, so the desktop host bridge and its updated tool-catalog assertion remain
uncompiled in this environment.

On 2026-10-15, `filesystem.write` moved to a Python desktop tool that validates
the current host plan and path limits before sending the write through the
one-target C++ SFTP service. The Python SQLite ledger caches completed writes
and blocks retries after an unknown outcome; unsupported routes still use the
Rust host executor. Full Python Agent discovery ran 96 tests: 92 passed and 4
service integration cases were skipped because no C++ binary path was configured
for that standalone run. The sidecar smoke, focused ledger/SSH routing tests
(12), Python syntax compilation, documentation link check, and `git diff --check`
passed. `pnpm check` passed its hygiene, secret, documentation, contract build,
typecheck, and packaging stages, then stopped in the JavaScript unit tests:
Node 26.5.1 failed in `os.userInfo()` with `uv_os_get_passwd returned ENOMEM`;
the same error reproduces with `node -e`. No native CMake test build is
configured in this workspace. The new Rust host resolver could not be compiled
because Cargo is unavailable on `PATH`. After that broader run, the host-side
independent plan recheck and the Python edit migration were added. The latest
Python files pass `py_compile`, the documentation link check and `git diff --check`;
the Python test suite was not rerun for this edit, and the new Rust code remains
uncompiled because Cargo is unavailable.

On 2026-10-15, `filesystem.backup` moved to Python/C++ for supported desktop SSH
routes. Python checks a stable regular file, its 1 MiB cap and single-link count,
then creates the host-selected sibling exclusively; Rust revalidates the action
binding and stores ownership metadata. Full Python Agent discovery ran 97 tests:
93 passed and 4 C++ service integration cases were skipped because no service
binary path was configured. `py_compile`, the documentation link checker
(15 Markdown files, 48 links), and `git diff --check` passed. The Rust host bridge
could not be compiled because Cargo and rustc are unavailable on `PATH`.

On 2026-10-15, `filesystem.restore` moved to Python/C++ for supported desktop SSH
routes. The Python tool verifies the current target revision, backup size and
digest, regular-file types, and single-link counts before the C++ guarded atomic
replace. The host revalidates the action binding, task ownership and available
backup row, then conditionally consumes that row after replacement. The focused
SSH routing suite ran 11 tests successfully. Full Python Agent discovery ran 98
tests: 94 passed and 4 C++ service integration cases were skipped because no
service binary path was configured. `py_compile`, the documentation link checker
(15 Markdown files, 48 links), and `git diff --check` passed. Rust host bridge
compilation could not run because Cargo and rustc are unavailable on `PATH`.

On 2026-10-15, `filesystem.backup.cleanup` moved to Python/C++ for supported
desktop SSH routes. The host validates exact plan bindings and ledger ownership,
including approved cross-task rotation items. Python checks the backup digest,
size, file type, stability and single-link count before deletion, then asks the
host to consume the exact row. The batch path preserves ordered `removed`,
`skipped`, and `failed` outcomes. Validation results are recorded after the
implementation run: `py_compile`, the documentation link check, and
`git diff --check` passed. The focused Python test module could not import
because `jsonschema` is not installed in the active environment. The Rust host
bridge could not be formatted or compiled because Cargo, rustfmt, and rustc are
not available on `PATH`.

On 2026-10-15, `filesystem.backup.list` and `filesystem.backup.retention` moved
to Python tool handlers. Rust now exposes only bounded, task/server-scoped ledger
queries, after validating the durable task, target, plan and call identity.
Python validates the returned rows, formats task-owned listings, and calculates
per-path retention candidates with the same UTC and tie-breaking rules.
Python Agent discovery ran 101 tests successfully, with 4 skipped; the focused
host-tools module passed all 12 tests. `py_compile`, the documentation link
checker (15 Markdown files, 48 links), and `git diff --check` passed. The Rust
host bridge could not be formatted or compiled because Cargo, rustfmt, and rustc
are not available on `PATH`.

On 2026-10-15, `package.install` moved to the shared Python/C++ effectful-action
route. Rust revalidates the durable plan, exact approval and package parameters,
then records the terminal result in the host execution fence; unsupported SSH
routes retain the existing Rust executor. Added Python coverage for structured
request bounds and the approved desktop route. Full Python Agent discovery ran
112 tests successfully with 4 service-path skips; CTest passed all six targets,
including the C++ SSH mock and Agent-to-C++ integration.

## Deliberately deferred

- keeping a persistent C++ SSH service alive across desktop requests;
- automatically synchronizing selected desktop targets and credentials to the
  C++ service configuration;
- moving Rust-only Host CA, HTTPS proxy, unsupported proxy bypass, keyboard-interactive,
  and other compatibility SSH routes into the C++ service;
- full durable task/plan recovery beyond the pending-approval reattach path;
- full C++ SFTP and PTY coverage for compatibility-only SSH configurations;
- control plane, PostgreSQL, and multi-tenant deployment.

The next milestones are new-layout Tauri installer parity checks, durable
task/plan state, and automatic selected-target synchronization to the C++
service configuration, while keeping
host authorization as the final gate. A real SSH success against an OpenSSH
server remains unverified because Docker is unavailable. The frozen Python
Agent build and installed-resource layout smoke passed on Windows 11 / Python
3.14.8 with a restricted `PATH`, including provider/MCP imports and the JSON-RPC
stdio lifecycle. The full Tauri installer smoke and Rust-to-Python host
integration remain unverified because Cargo and rustc are unavailable in this
environment.
Do not mark later product requirements as delivered by the scaffold or
documentation.
