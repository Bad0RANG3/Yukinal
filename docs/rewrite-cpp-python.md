# C++ / Python rewrite: milestones 1–3 in progress

> **Current scope:** the standalone C++ SSH service and Python caller are in
> place. The Python Agent Runtime has bounded LangGraph tool execution, stdio and
> Streamable HTTP MCP support, OpenAI-compatible/Anthropic/Gemini adapters, a
> read-only bridge to the desktop host, and tool lifecycle events consumed by the
> existing UI. Tauri development and new package builds now select the Python
> sidecar; the old Node resources remain as a transition fallback. Rust still
> hosts native UI IPC and existing platform services. The Python sidecar can call the C++ `service_status` operation when
> configured; its
> target is bound to the selected server ID, which must be present in the C++
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
> and expiry handling; the current desktop catalog still exposes read-only tools.

## Repository baseline and migration boundary

The repository contains a working React/Tauri product baseline and shared
TypeScript contracts. The previous rewrite draft mixed SSH, SFTP, PTY, database,
credentials, terminal, MCP, approval, Agent, and UI bridge code before the first
SSH service had a verified end-to-end boundary. The implementation below is
being migrated in checked stages. React components and their Tauri calls are
preserved; the Rust/Tauri host remains responsible for the native UI bridge,
OS credential store, and final host authorization while Agent execution moves
to Python and structured Agent SSH calls move to the C++ service.

The Python runtime speaks the desktop host's bidirectional JSON-RPC protocol. It
can execute a first set of read-only server tools through the host, load low-risk
MCP tools from the host catalog, and emit the tool-call/result events that the
existing transcript UI displays. When the C++ service token is configured, the
Python sidecar also registers `ssh.service_status`; it binds the call to the
selected `host_target.serverId`, so the model cannot choose a different target.
`pnpm --filter @yukinal/desktop tauri dev` selects this runtime for Tauri
development; `pnpm desktop:dev:python` is an equivalent shortcut and
`pnpm desktop:dev` remains the browser-only UI preview. `pnpm package` freezes
the Python runtime with PyInstaller, stages it under `python-agent/`, and the
Rust resolver prefers it over the retained Node fallback. Installed app and
cross-platform artifact acceptance are still required before removing that
fallback.

## Processes and trust boundaries

```text
Python Agent Runtime
    ├── Tool Registry → schema → per-run read allowlist → audit
    ├── LangGraph bounded tool workflow
    ├── host.* JSON-RPC bridge → Tauri/Rust host → existing SSH and MCP
    └── yukinal_ssh caller (optional service-status tool)
              │ HTTP/JSON + local bearer token
              ▼
        C++ SSH service (binds only 127.0.0.1)
              │ target_id lookup → operation policy → known_hosts check
              ▼
        libssh2 ── SSH ──► configured server

React/Tauri desktop UI: unchanged; the host executes `host.*` tools, while the
C++ service executes the optional read-only SSH status tool
```

The bridge uses the existing sidecar request path: Python sends
`host.tool.execute` (or `host.mcp.catalog`) over the child process pipe, and the
desktop host replies over that same pipe. Python does not open SSH sessions or
start MCP processes through this bridge. The host continues to resolve the
server, enforce its current authorization and execute the operation. Current
Python host tools are read-only; MCP entries are exposed to the model only when
the host catalog classifies them as low-risk.

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
external data. Discovered tools require approval unless an administrator's
local `read_only_tools` allowlist classifies the exact tool as read-only.

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

The service currently targets Linux hosts with systemd. The service configuration is the allowlist. Requests cannot provide a hostname,
port, username, credential, or command. The service resolves a `target_id` to a
configured target and accepts only `operation: "service_status"` with one
validated systemd unit name. It sends the fixed command `systemctl is-active --
<service>` only after the target's host key matches the configured OpenSSH
`known_hosts` file. Unknown and changed keys fail closed.

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

Only the documented fields are accepted. Timeouts are 1–30 seconds, unit names
are restricted to a conservative character set, and stdout/stderr are capped at
64 KiB each.

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
- exactly one auth mode: a `private_key_file`, a `private_key_env`, or a
  `password_env` reference. Private-key environment references can include an
  optional `passphrase_env`.

The desktop pin form lets the service enforce the same exact-key trust decision
as the existing server host-key UI. Private-key environment references let the
desktop pass decrypted keychain material directly to the C++ child without
writing key material into the JSON configuration.

Set the configured API token environment variable before starting the service.
Never commit credentials or a real production target configuration.

## Python caller

The `yukinal_ssh` package in [`agent/`](../agent/) uses only the Python standard
library and supports Python 3.11+. It validates inputs before sending them and
validates the response before returning a `ServiceStatusResult`. Structured
remote errors are raised as `SshServiceError` with a stable `code` and
`http_status`. Install Agent dependencies with `python -m pip install -e
"agent[agent]"` to use LangGraph and MCP.

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
environment. After configuring CMake to use that interpreter, the C++ build
completed and all six CTest targets passed on the current source revision. Full
Python unittest discovery ran 45 tests: 42 passed and 3 service integration
cases were skipped because the service binary path was not configured for that
standalone run. The Agent suite covers live stdio and Streamable HTTP MCP
connections, credential-reference handling, schema/target rejection,
parameter-bound approvals, approval RPC resume/reject, audit redaction,
tool/task/output limits, LangGraph execution, bidirectional host RPC framing,
and redacted tool lifecycle events. The Paramiko SSH mock and
Python-to-C++ HTTP tests also passed in CTest.

## Deliberately deferred

- removing the Node bundle and runtime transition fallback after installed-app parity;
- packaging and supervising the C++ SSH service with the desktop app;
- automatically synchronizing selected desktop targets and credentials to the
  C++ service configuration;
- write-capable tools in the desktop catalog and full desktop task/plan resume;
- arbitrary shell commands outside the existing host's separately guarded task flow;
- SFTP, file transfer, PTY, persistence, and a Qt desktop shell;
- control plane, PostgreSQL, and multi-tenant deployment.

The next milestones are installed-app parity checks, removing the Node fallback,
packaging and supervising the C++ service, durable task/plan state, and automatic
selected-target synchronization to the C++ service configuration, while keeping
host authorization as the final gate. A real SSH success against an OpenSSH
server and installed frozen-Agent lifecycle remain unverified. Do not mark later
product requirements as delivered by the scaffold or documentation.
