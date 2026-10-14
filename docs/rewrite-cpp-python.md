# C++ / Python rewrite: milestone 1

> **Current scope:** only the SSH foundation milestone in
> [`ssh_agent_development_brief.md`](../ssh_agent_development_brief.md) is being
> implemented. This is not a rewrite of every product feature and is not a
> claim that the desktop app has switched from Tauri.

## Repository baseline and migration boundary

The repository contains a working React/Tauri product baseline and shared
TypeScript contracts. The previous uncommitted rewrite draft mixed SSH, SFTP,
PTY, database, credentials, terminal, MCP, approval, Agent, and UI bridge code
before the first SSH service had a verified end-to-end boundary. That draft was
replaced with the narrower implementation below. The React components and their
Tauri calls are preserved. The Rust/Tauri code remains available as a legacy
behavior reference until replacement milestones are built and checked.

The new service is intentionally separate from the desktop UI in this first
milestone. The UI switch and broader operation coverage are later work.

## Processes and trust boundaries

```text
Python caller
    │ HTTP/JSON + local bearer token
    ▼
C++ SSH service (binds only 127.0.0.1)
    │ target_id lookup → policy validation → known_hosts check
    ▼
libssh2 ── SSH ──► configured server

React/Tauri desktop UI: unchanged; not yet connected to the new service
```

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
- an OpenSSH-format `known_hosts_file`;
- exactly one auth mode: a `private_key_file` (optional passphrase environment
  reference), or a `password_env` reference.

Set the configured API token environment variable before starting the service.
Never commit credentials or a real production target configuration.

## Python caller

The package in [`agent/`](../agent/) uses only the Python standard library and
supports Python 3.11+. It validates inputs before sending them and validates the
response before returning a `ServiceStatusResult`. Structured remote errors are
raised as `SshServiceError` with a stable `code` and `http_status`.

## Build and verification

```powershell
cmake -S native -B native/build
cmake --build native/build --config Release
ctest --test-dir native/build -C Release --output-on-failure
```

The CTest suite covers config/schema validation, operation policy, Python client
behavior, and a live Python-to-C++ HTTP path including health, allowlist
rejection, origin rejection, and SSH connection failure classification. The
optional OpenSSH container test covers a successful SSH operation, bad
credentials, command timeout/output limit, and unknown/changed host keys.

Local verification on 2026-10-15: all six Python client tests passed. C++
configuration stopped while fetching the pinned source dependencies because
the shell could not connect to GitHub; therefore no C++ build or CTest result is
claimed. The OpenSSH success script is present, but Docker is not installed in
this environment, so that test was not run.

## Deliberately deferred

- connecting the React UI to the new service or replacing its Tauri bridge;
- arbitrary shell commands, write operations, approval workflows, and audits;
- Python Agent orchestration, LangChain/LangGraph, model providers, and MCP;
- SFTP, file transfer, PTY, persistence, and a Qt desktop shell;
- control plane, PostgreSQL, and multi-tenant deployment.

The next milestone should first verify a real read-only SSH success against the
test server and then continue the staged scope in the development brief. Do not
mark later product requirements as delivered by the scaffold or documentation.
