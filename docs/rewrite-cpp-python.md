# C++ / Python rewrite: milestones 1–3 in progress

> **Current scope:** the standalone C++ SSH service and Python caller are in
> place. The Python Agent Runtime has bounded LangGraph tool execution, stdio and
> Streamable HTTP MCP support, OpenAI-compatible/Anthropic/Gemini adapters, a
> desktop-host bridge for approval, target resolution, and remaining tools, plus tool lifecycle events consumed by the
> existing UI. Tauri development and new package builds now select the Python
> sidecar; installer resources contain Python Agent and C++ SSH assets only.
> The desktop resolver has no legacy Node resource or runtime fallback; old Node paths below refer only to historical migration evidence.
> Rust still hosts native UI IPC, local trust-store updates, and final approvals.
> SSH network operations use C++; configurations outside current C++ coverage fail
> explicitly instead of falling back to the Rust SSH backend. C++ verifies
> configured OpenSSH host CAs and optional KRLs for supported CA algorithms; remote
> HTTPS KRL downloads use the configured SOCKS5/SOCKS5H tunnel and verify the KRL
> server certificate; loopback-only HTTP KRL downloads must bypass the proxy.
> A CA is itself a valid trust source and does not also require a leaf host-key pin. The
> desktop passes proxy configuration as data; C++ parses HTTP/HTTPS and SOCKS5/SOCKS5H
> proxy URLs, verifies HTTPS proxy certificates, applies bypass rules, and establishes
> CONNECT or SOCKS5 tunnels. SOCKS5 resolves target names locally; SOCKS5H delegates
> name resolution to the proxy. SOCKS4, PAC, unsupported CA algorithms, and other
> SSH modes currently fail explicitly until the C++ service supports them.
> Keyboard-interactive terminal prompts are forwarded by the C++ PTY worker to
> the existing desktop prompt UI. Desktop terminals use a dedicated C++ PTY
> child process; configurations outside C++ support fail closed instead of
> opening a Rust SSH terminal. The Python sidecar can call the C++
> `service_status` operation when configured; the desktop Agent routes server
> logs, Docker inspection, systemd inspection, and package inspection through a
> short-lived C++ process using the saved desktop target and identity. The desktop file browser and
> manual transfers use C++ SFTP operations for password, private-key, OpenSSH user-certificate, and
> SSH-agent identities on direct, supported HTTP/HTTPS CONNECT, or SOCKS5/SOCKS5H routes with either a
> confirmed host-key pin or a supported host CA. The same identities use C++ for
> structured SSH operations. Host-key probes use C++ on direct and supported
> HTTP/HTTPS CONNECT and SOCKS5/SOCKS5H routes; the probe reports the certified public-key fingerprint without
> treating it as trusted. Unsupported host-key modes fail explicitly. The desktop
> file browser fails explicitly when its target is outside C++ SFTP support.
> Manual uploads, downloads, recovery cleanup, and native drag-out use bounded C++ SFTP
> streams on supported direct, HTTP/HTTPS CONNECT, or SOCKS5/SOCKS5H routes with either
> a confirmed pin or supported host CA. Unsupported targets fail explicitly instead
> of running Rust SFTP.
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
> C++-supported routes, `systemd.restart`, `docker.restart`, `package.install`,
> and approved `server.exec` now run from Python through structured C++ SSH
> operations. The `server.exec` one-shot target is enabled only after Rust
> validates the plan and consumes the exact approval; `server.exec` now requires
> a C++-supported target and fails explicitly when the target is unsupported.
> The former Rust route was later removed entirely.
> The host checks the durable action plan before
> claiming the call and consuming its ticket. A successful result is cached only
> after C++ reports exit code zero. A completed nonzero `server.exec` returns its
> bounded exit code and output to the Agent for diagnosis or replanning, while the
> failed action remains non-replayable; transport outcomes stay unknown and cannot
> be automatically replayed. SSH operations outside current C++ support fail
> explicitly; the desktop no longer runs a second Rust SSH backend.
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
> copy and leaves the call outcome unknown. File operations fail explicitly when
> the target is outside C++ SFTP support. `filesystem.restore` now also follows
> the Python/C++ path. The host checks task ownership and backup status, Python verifies both the target
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
saved target, selected trust source (confirmed host-key pin or configured host CA),
and credential material from the OS store;
the Python process passes credentials only through the C++ child environment and
cleans up its temporary configuration when the call ends. Desktop
`server.connect`, `server.info`, `server.logs`, and `server.services`, Agent
Docker/systemd/package tools, `server.exec`, file operations, terminal sessions,
and host-key probes now fail explicitly when C++ cannot represent the target.
No production route falls back to remote Rust SSH execution. Rust still owns the
Tauri bridge, local trust-store operations, approval checks, and UI result
persistence.
The Python Agent catalog exposes `docker.restart`, `systemd.restart`, and
`package.install` as structured actions with exact per-call approval. The host
recomputes a fingerprint from the exact raw input and consumes the corresponding
one-time ticket. All three actions are routed through the Python caller to
structured C++ SSH operations; the host records their terminal outcomes in the
durable execution fence. Unsupported targets fail before any remote operation.
`server.exec` remains separately guarded and exposed only when the host supplies
the current remote action-plan binding. The host validates the plan, consumes
the exact approval ticket, then starts a one-shot C++ service config that enables
`server.exec` for that target only. Python sends the raw command plus bounded
workdir/environment parameters; the C++ policy validates those fields and its
SSH service builds the quoted remote shell wrapper. Python returns the validated
result to the host. Persistent service target configs deny it by default. A
target the C++ service reports as unsupported fails explicitly; production
never falls back to Rust SSH execution.
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
File operations fail explicitly when the target is outside C++ SFTP support.
Backup listing queries remain task-scoped at the host database boundary, where
Rust revalidates the durable task, target and plan binding and records the call identity. Python maps
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
`systemd_inspect`, `package_inspect`, `service_status`, `disk_usage`, `service_inventory`,
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
approved restart, package-install, and `server.exec` operations through the C++
SSH service. For `server.exec`, Rust validates the durable plan and exact
approval and grants the one-shot C++ target capability; Python transports raw
bounded parameters, and C++ validates them and builds the remote shell wrapper.
Production reports unsupported targets without invoking the Rust SSH backend.
The desktop host continues to enforce approval, parameter binding, and one-time
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

The root filesystem query is also parameterless; callers cannot choose a path or
provide a command:

```json
{
  "target_id": "staging-web-01",
  "operation": "disk_usage",
  "parameters": {},
  "timeout_seconds": 10
}
```

Only the documented fields are accepted. `service_status` requires exactly one
validated unit name; `disk_usage`, `connection_check`, and `service_inventory` accept no
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

The desktop passes the selected proxy URL and optional `no_proxy` string to the
C++ service without parsing them in Rust. C++ validates HTTP and HTTPS proxy URLs,
verifies HTTPS proxy certificates, evaluates exact-host, domain-suffix, IPv4
prefix, IPv4/IPv6 CIDR, and `*` bypass patterns. Hostname and wildcard patterns
can include an SSH-port constraint, such as `localhost:22` and `[::1]:2222`.
C++ then performs the CONNECT tunnel.
Proxy credentials are forwarded to the child environment separately from the
target JSON. The C++ service bounds the CONNECT response and shares the SSH
operation deadline.

Desktop host-key probes use a separate short-lived `probe_only` target with no
credential and no trust entry. The service permits that target to perform only
`host_key_probe`; the returned fingerprint remains untrusted until the user
confirms it through the existing UI. For host certificates, C++ reports the
certified public-key fingerprint; the probe itself does not validate trust or
change the configured CA or host-key pin.

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
Python normalization RPC. A target that cannot be represented by the C++ SSH
service now fails explicitly; this route has no Rust collector fallback.

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

On 2026-10-15, approved `server.exec` moved to the same Python/C++ action route.
Rust still validates the exact plan and approval, builds the remote command
wrapper, and grants the one-shot target capability; Python calls the bounded C++
operation and returns its validated output for the host execution fence. The
Rust task command-grant executor remains only as the compatibility path for
unsupported C++ targets. The Python client, Agent route, target capability, and
SSH mock now cover this path. Python Agent discovery ran 116 tests with 4
service-path skips, and CTest passed all six targets. Rust formatting and
compilation remain unverified because Cargo, rustfmt, and rustc are unavailable
on this machine.

On 2026-10-15, the C++ HTTP service began consuming `server.exec` authorization
atomically per configured target before starting SSH. A second or concurrent
request for the same one-shot target is rejected with `operation_not_allowed`,
including when the first SSH attempt fails. The SSH mock now uses separate
one-shot targets for the successful and output-cap cases and verifies replay
rejection. Python syntax parsing and `git diff --check` passed. This revision
was rebuilt with the installed Visual Studio Build Tools environment, and all
six CTest targets passed, including the SSH mock replay check and Agent-to-C++
integration. The initial build failure was only caused by the shell not loading
the MSVC environment; no project or build configuration change was needed. A
repo-wide `pnpm check` passed publication/secret checks, Node command and
packaging tests, documentation links, contract builds, typechecks, 278 Agent
tests, the desktop production bundle, Python sidecar smoke, and all six CTest
targets. In the restricted shell Node 26.5.1 throws
`uv_os_get_passwd returned ENOMEM` from `os.userInfo()` inside `tsx`; rerunning
the gate under the normal user context passed. Rust formatting, lint, Cargo
check, and Rust-to-sidecar tests were skipped because Cargo is not on `PATH`.

Later on 2026-10-15, the installed Rust toolchain was invoked directly and the
VS 2022 environment was loaded. `cargo check --manifest-path
apps/desktop/src-tauri/Cargo.toml --lib --locked` passed. The desktop unit-test
binary linked, but stalled before listing or running tests, including with
`--list`; the focused CA-route test is not counted as passed.

On 2026-10-15, desktop streaming uploads/downloads, staging recovery cleanup,
and native drag-out were switched to the existing C++ SFTP service. Their Rust
SFTP session fallbacks were removed; unsupported targets now fail explicitly.
The Rust change could not be compiled in this environment because Cargo and
rustc are unavailable on `PATH`.

On 2026-10-15, production `server.exec` stopped falling back to the Rust SSH
executor. A target the C++ service cannot represent now fails before consuming
the task command budget; the former path remains compiled only for Rust unit
tests. The supported route sends raw command/workdir/environment parameters to
C++, where policy validation and shell quoting run. The C++ service builds;
Rust formatting and compilation could not be checked because Cargo and rustfmt
are absent from `PATH` and the usual per-user Cargo bin directory is absent.

On 2026-10-15, `server.info`, `server.logs`, and `server.services` also stopped
running Rust SSH commands or collectors. They now require a C++-supported
target, acquire remote output through the C++ service, and keep Python as the
normalization layer. Rust remains the Tauri host bridge for input validation,
authorization, cancellation, and persistence. The Rust edits could not be
compiled here because Cargo and rustc are unavailable on `PATH`.

On 2026-10-15, desktop `server.connect`, snapshot, logs, services, host-key
probes, and Agent Docker/systemd/package operations were made C++-only. Rust
command construction and output parsing for those SSH paths were removed; at
that checkpoint the old Rust session connector remained test-only. The later
same-day cleanup below removed it. Unsupported C++ target configurations fail
explicitly. Rust host-key pin reads/writes remain local trust-store operations.
Compilation was unavailable at this checkpoint.

## Deliberately deferred

- keeping a persistent C++ SSH service alive across desktop requests;
- automatically synchronizing selected desktop targets and credentials to the
  C++ service configuration;
- expanding C++ support for remaining identity, host-key, and proxy modes that
  currently fail closed;
- full durable task/plan recovery beyond the pending-approval reattach path;
- complete C++ SFTP and PTY support for currently unsupported SSH configurations;
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

On 2026-10-15, the Rust toolchain and MSVC environment were loaded explicitly.
`cargo fmt --all -- --check`, `cargo check --workspace --all-targets`, and
`cargo clippy --workspace --all-targets -- -D warnings` passed. The focused
sidecar PID handshake test passed. The durable Rust-to-Python Agent integration
test was then run with its required environment and exposed a later-stage gap:
the Python runtime does not register `investigation.plan`, so a durable run
fails before reaching its read-only host tool. Its loopback model fixture now
uses the non-streaming OpenAI-compatible JSON response expected by the Python
`ainvoke` path. This Agent workflow gap is outside the brief's first SSH
foundation stage and remains explicitly unverified; it is not counted as a
passing integration test. The C++ build and all six CTest targets passed after
loading the installed Visual Studio environment.

On 2026-10-15, the Python Agent registered `investigation.plan` as a bounded
host tool. The plan ID derives from the durable task and stable LangGraph tool
call ID, while task and runtime fields remain host-owned. After the host records
the plan, LangGraph carries the returned current step and evidence binding into
the next tool call. Python mock coverage now exercises plan creation followed by
a host-authorized SSH status read, and rejects plan steps that name unregistered
tools. The focused host-tool test module passed all 14 tests.

On 2026-10-15, the C++ SSH service began normalizing supported OpenSSH host-key
certificate blobs to their certified public key for SHA-256 fingerprinting and
`known_hosts` checks against entries containing that plain key. Host-key probing
and confirmed key pins can now use the C++ path for certificate-backed hosts.
This does not validate a host CA signature; that policy and KRL handling remain
on the Rust compatibility route. C++ build and all six CTest targets passed,
including the new host-key parser coverage.

Later on 2026-10-15, C++ host CA signature and KRL verification moved into the
service. Desktop routing now accepts a supported configured CA as the target's
trust source without requiring a redundant leaf-key pin. Unsupported CA
algorithms and proxy protocols still use the compatibility path.

Also on 2026-10-15, HTTP/HTTPS proxy URL validation, HTTPS certificate
verification, bypass matching, and CONNECT transport moved into C++. The desktop
passes the selected proxy configuration through the Tauri bridge; other proxy
protocols remain on the compatibility path. The C++ service and Tauri library
compiled after this change. The SSH mock integration passed with a locally trusted
CA, a complete HTTPS CONNECT SSH operation, and rejection of a proxy certificate
whose SAN did not match the configured proxy host.

Also on 2026-10-15, the C++ SSH transport added authenticated SOCKS5 and SOCKS5H
tunnels. SOCKS5 resolves destination names locally, while SOCKS5H sends domain
names to the proxy; both preserve the configured bypass rules and connection
deadline. The Tauri bridge forwards the selected route as configuration data
only; SOCKS handshake, authentication, DNS routing, and tunneling stay in C++.
The Rust HTTP client does not implement SOCKS, and other HTTP traffic retains its
existing HTTP/HTTPS proxy support.
The SSH mock passed authenticated local- and remote-DNS tunnels and rejected a
bad proxy password without exposing it. The C++ test executable passed all 56
tests, all six CTest targets passed, the Python SSH mock passed, 12 `yukinal-net`
tests passed, and `cargo check -p yukinal-desktop --lib` passed.

On 2026-10-15, fixed C++ KRL URL parsing that incorrectly rejected loopback HTTP
URLs before applying the loopback-only HTTP rule. Loopback IPv4 and IPv6
classification no longer depends on platform socket conversion APIs. The C++
test executable passed all 54 tests, and all six CTest targets passed, including
the SSH mock and Agent-to-C++ integration.

Also on 2026-10-15, added direct C++ coverage for KRL HTTP loopback address
classification, including IPv4 `127/8`, IPv6 `::1`, malformed addresses, and
non-loopback addresses. The rebuilt C++ test executable passed all 55 tests, and
all six CTest targets passed.

On 2026-10-15, aligned the Python desktop SSH caller with the target payloads
already emitted by Tauri. It now accepts either a confirmed host-key pin or a
configured Host CA, plus the C++ service's typed or URL-based HTTP, HTTPS, SOCKS5,
and SOCKS5H proxy forms with string or array `no_proxy` rules. This lets the
Python Agent keep supported CA and proxy targets on its Python-to-C++ path instead
of rejecting the payload and handing the operation back to the desktop host. The
focused process suite passed all six tests, and the Agent SSH mock passed selected
target status and snapshot operations through an authenticated SOCKS5H tunnel.

On 2026-10-15, moved HTTPS Host CA KRL retrieval through authenticated SOCKS5
and SOCKS5H routes into the C++ service. The KRL HTTPS certificate and hostname
are verified before the bounded response is parsed. Port-qualified `NO_PROXY`
rules are evaluated against the KRL URL's host and port, so a KRL-specific
bypass does not accidentally use the SSH destination port. The SSH mock verifies
SOCKS5 local DNS, SOCKS5H remote DNS, URL-port bypass, and rejection of a KRL
server certificate with the wrong hostname; all six CTest targets passed.

On 2026-10-15, aligned C++ and Python validation for port-qualified `no_proxy`
patterns. C++ now checks both the host pattern and SSH port before bypassing the
proxy; the Python desktop caller accepts the same syntax, including bracketed
IPv6 and all-host port rules. The native C++ suite, all six CTest targets, the
focused Python process suite, and the Agent-to-C++ SSH mock passed.

On 2026-10-15, removed the retired Rust SSH crate and every test-only Rust SSH
transport path from the desktop host. `server.exec` tests no longer exercise a
second backend; unsupported routes fail closed in test and production builds.
The Tauri host remains responsible for UI IPC, local credentials, approvals,
and persistence, while SSH transport and SFTP operations use C++ and Agent
normalization stays in Python. Rust formatting, desktop test compilation, the
core and host-trust suites, and all six CTest targets passed after the removal.

On 2026-10-15, the Python `ModelDrivenAgent` began applying the ADR 0004
internal/provider tool-name mapping: registered dot-namespaced names
(`server.info`) are declared to the model as `server__info`, model tool calls are
mapped back to the internal name before registry lookup, provider-name
collisions are rejected at construction, and `ToolMessage` frames carry the
provider spelling the model used. Before this, dotted names went straight to
OpenAI-compatible endpoints, which reject them. The Rust cross-language test now
registers its loopback provider in the real `YUKINAL_DATA_DIR/yukinal.db`
provider store and sends `providerId`/`model` instead of an unsupported
request-scoped `providerConfig`, so the durable read exercises the production
provider path end to end. Evidence/artifact persistence for investigation steps
is still not implemented in the Python runtime and is no longer asserted as
passing by that test. Verification on this revision: the C++ build and all ten
CTest targets passed; the Python Agent suite passed 88 tests; the Rust workspace
tests passed, including `real_sidecar_runs_a_durable_read_through_provider_and_host`;
`cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`
passed. The `yukinal-desktop --lib` test binary still cannot launch in this
Windows environment (`STATUS_ENTRYPOINT_NOT_FOUND`, an embedded-manifest/`comctl32`
limitation that predates this change), so the desktop crate's own unit tests
remain unrun here.

On 2026-10-15, the C++ desktop host took over the local host-key trust reads and
forgets: `server_host_key_status` and `server_host_key_forget` use a new
`native/desktop_host/src/host_trust.cpp` store that reproduces the `crates/host-trust`
`v1:host:port:SHA256:...` format (header, sorted lines, atomic replace with
Windows sharing-violation retry). A pending pin is reported, a changed presented
key is a `mismatch` rather than a silent overwrite, and forgetting an unpinned
host reports `forgotten: false` instead of claiming a removal. Probe and trust
(the SSH handshake and the one-time probe ticket) remain on the Rust path. The
C++ build and all ten CTest targets passed, including the new pin/status/forget
round trip in `desktop_host_database_tests`. The command-level C++/Rust parity and
deletion plan is tracked in `docs/rust-removal-plan.md`.

On 2026-10-15, the Rust/Tauri desktop host was removed at the user's explicit
request, before parity was reached. `crates/`, `apps/desktop/src-tauri/`,
`Cargo.toml`/`Cargo.lock`/`rust-toolchain.toml`/`.cargo/`, the retired legacy Node
agent (`apps/agent/`) and its smoke were deleted; the React UI talks only to the
C++ host through `window.__YUKINAL_HOST__`, with no Tauri fallback. `pnpm check`
drops cargo and packaging steps and passes end to end (CTest 10/10, shared 125,
desktop 251, sidecar smoke). Commands the C++ host has not implemented keep
returning `NOT_IMPLEMENTED`; packaging/installers are not implemented. From here
the work is implementation against the shared contract, guided by
`docs/rust-removal-plan.md`; the pinned reference implementations remain in git
history.

On 2026-10-16, the C++ desktop host gained the first Python Agent sidecar bridge.
`native/desktop_host/src/sidecar.cpp` spawns the Agent (`python -m
yukinal_agent.sidecar` or `YUKINAL_AGENT_ENTRY`), supervises it, speaks the 24 MiB
NDJSON/JSON-RPC protocol, correlates responses by id, re-publishes `agent.stream`
notifications through the WebView event bridge, and dispatches the Agent's
`host.*` requests. The router answers `host.credentials.set/get/reclaim` against
the OS credential store, returns an empty `host.mcp.catalog`, and acknowledges
`host.tool.cancel`. On top of that bridge the host now implements
`agent_spawn`, `agent_status`, `agent_kill`, `agent_logs`, and all six
`provider_*` commands; API keys never leave the C++ credential boundary. The new
`desktop_host_sidecar_tests` target runs the real Python Agent and verifies the
handshake, `system.ping`, provider save/list/activate/delete credential rotation
and reclamation, and the router forwarding path. `pnpm desktop:dev:cpp` detects
`agent/.venv` and forwards `YUKINAL_AGENT_PYTHON`/`YUKINAL_AGENT_SOURCE` to the
host. CTest passed all eleven targets; a Windows-only build issue was fixed by
setting `CREATE_UNICODE_ENVIRONMENT`, without which `CreateProcessW` rejected any
non-empty environment block with `ERROR_INVALID_PARAMETER`. Agent runs,
`host.tool.execute`, MCP catalogs, and investigation forwarding remain.

On 2026-10-16, the C++ host began forwarding the Agent run commands over the
sidecar bridge. `agent_run_start`, `agent_run_stop`, and
`agent_approval_respond` now map to the Python Agent Node's `agent.run.start`,
`agent.run.stop`, and `agent.approval.respond` methods. The host forwards the
request payload unchanged because the Python runtime owns durable `messageId`
admission, cancellation, and one-time approval state; the host keeps a long
timeout only for a synchronous start while its dedicated sidecar host worker
continues to answer `host.*` calls. `desktop_host_sidecar_tests` now checks that
`resume: false` admits a message without executing it, that a synchronous
`resume: true` surfaces the Agent's missing-provider `INVALID_PARAMS` error, and
that an unknown run and a stale approval answer `stopped: false` and
`accepted: false` instead of `NOT_IMPLEMENTED`. The canonical `native/build-nmake`
Release build and all eleven CTest targets passed; the documents were updated to
match. The separate `native/build-cpp-host` Debug tree aborts in its older desktop
host tests and was not used for this verification. `host.tool.execute`, MCP
catalogs, and investigation forwarding remain the next bridge milestones.

On 2026-10-16, the C++ desktop host migrated the local file browser's path
selection and preview. A new `native/desktop_host/src/local_paths.cpp` owns a
short-lived handle table behind `local_path_pick` and `local_file_preview`: 15
minute TTL, 2048 handles, 256 picked paths, and rejection of symlinks or paths
that changed type after selection. `local_file_preview` revalidates the handle,
reads at most 1 MiB plus one sentinel byte through a no-follow open, and builds
the shared `FilePreviewResponse` (`text`, `image`, or `binary`, with incomplete
trailing UTF-8 still shown as its readable prefix). Windows shows the native file
and folder dialogs (`GetOpenFileNameW`/`SHBrowseForFolderW`) and the host now
links `comdlg32`/`ole32`; other platforms fail explicitly. `desktop_host_tests`
covers text/binary/image classification, truncation, incomplete UTF-8,
directory rejection, TTL expiry, and unknown handles. During verification an
MSVC dependency-tracking gap left older desktop test objects linked against the
resized `IpcRouter`; forcing those objects to rebuild fixed the resulting
ABI-mismatch crashes. The canonical `native/build-nmake` Release build and all
eleven CTest targets passed, and `pnpm check` was green. `emit_local_drop`,
transfer-history queries, the remaining host RPCs, and MCP/investigation
forwarding remain.

On 2026-10-16, after merging the Windows desktop shell PR, the C++ desktop host
took over the file-transfer history reads. `DesktopDatabase::list_file_transfers`
and `get_file_transfer` decode the bounded `file_transfers` ledger, verify that
each stored row matches its snapshot (transfer id, server id, direction and
status), and surface the result through `file_transfer_list` and
`file_transfer_get`. Because this host owns no transfer manager, a persisted row
that still claims to be in progress is reconciled to `interrupted` and written
back once the read statement is closed; terminal rows are unchanged.
`desktop_host_database_tests` covers ordering, server filtering, the bounded
limit, both transports, reconciliation and persistence, and the unknown-transfer
case, while `desktop_host_tests` covers the parameter validation. The canonical
`native/build-nmake` Release build passed all twelve CTest targets (including the
merged `window_context_tests`), and `pnpm check` was green. `emit_local_drop`, the
remaining host RPCs, and MCP/investigation forwarding remain.

On 2026-10-16, the C++ desktop host gained its SSH target resolver and MCP server
settings. `native/desktop_host/src/desktop_target.cpp` turns a saved server plus
its identity, OS-credential secret, host-key pin or CA, and proxy route into the
one-target `{target, credentials}` payload the Python Agent hands to the C++ SSH
service (`host.ssh.service_target`), plus the Agent file variants
(`host.ssh.filesystem_read_target`/`write`/`edit`) with the retired Rust path
policy: absolute bounded paths, a credential/process-secret blocklist, a
64-hex revision shape, and 512 KiB write/edit caps. Secret values travel only in
the `credentials` map. Separately, `mcp_server_list`, `mcp_server_save`, and
`mcp_server_delete` now read and write the `mcp_servers` table; static HTTP auth
header secrets enter the OS credential store and are reclaimed on delete, while
OAuth is explicitly refused until the MCP supervisor lands. `desktop_host_database_tests`
covers the resolver payloads, the path-policy rejections, and the MCP CRUD
round trip. The canonical `native/build-nmake` Release build passed all twelve
CTest targets. The in-process SSH commands (`server_logs`/`snapshot`/`services`),
MCP supervision and catalog, `host.tool.execute`, investigation, and installer
work remain; real SSH validation is blocked because the test host is filtered.
