# Yukinal

Yukinal is a desktop operations workspace for inspecting and managing SSH
servers. The React interface remains the product UI; the 2.0 rewrite moves SSH
operations toward a C++ service and Python Agent.

The current app/package interface version is `1.0.0`; the rewrite is still in progress.

## Rewrite status

The brief's first SSH foundation stage is complete; the broader migration is
still in progress. Current components include:

- a standalone C++ service bound to `127.0.0.1`;
- a Python caller over local HTTP/JSON;
- structured SSH operations: `connection_check`, `system_snapshot`, `docker_ps`,
  `docker_logs`, `docker_inspect`, `systemd_inspect`, `package_inspect`,
  `service_status`, fixed read-only `disk_usage`, `service_inventory`, and the existing host-approved
  `docker_restart`, `systemd_restart`, and `package_install` actions, plus
  bounded SFTP listing, metadata, read, write, exclusive backup creation, removal,
  guarded atomic replacement, and link-count probes for supported desktop identities,
  including OpenSSH user-certificate identities;
- target allowlisting, strict SSH host-key verification, bounded timeouts and
  output, and credentials referenced from environment variables or private-key
  files;
- a Python Tool Registry with JSON Schema validation, per-call tool/target
  authorization, parameter-bound approval grants, and redacted audit events;
- Streamable HTTP and stdio MCP clients, with remotely discovered tools
  defaulting to approval-required;
- a bounded LangGraph workflow with tool-call, total-time, and output-size
  limits.
- a configuration-driven OpenAI-compatible, Anthropic and Gemini model
  factory using official LangChain adapters, including OpenAI Chat Completions
  and Responses API selection;
- a model-driven LangGraph tool loop with tenant/user/target/risk policy,
  exact-argument approval requests, and bounded model output.
- the Python Agent's bounded NDJSON/JSON-RPC transport and desktop initialization,
  health, capability, and tool-catalog methods.

The Python JSON-RPC layer handles initialization, health, tool catalogs,
provider checks and model catalogs, text runs, cancellation, and a bidirectional
JSON-RPC bridge to the desktop host. Python runs can execute the existing
read-only server tools through the host and can use MCP tools the host marks
low-risk. On the desktop, `server.info` gathers its fixed SSH command batch
through the C++ service, normalizes its snapshot in Python, and sends the
validated result through the existing guarded host tool and snapshot database
path. The Overview's direct snapshot refresh uses the same Python normalizer for
C++ targets; unsupported configurations fail explicitly. The approved desktop
actions remain behind host-side plan checks and one-time call grants. Python exposes approval-required host tools
only while a durable remote-goal action step authorizes them; approval requests,
rejection, expiry, and accepted calls round-trip through the desktop host.
On C++-supported targets, `systemd.restart` and `docker.restart` now run as
structured operations through the Python caller; the host checks the durable
plan and consumes the exact one-time approval before returning target credentials.
Durable task recovery and broader tool coverage remain in progress. The
existing React/Tauri UI remains intact while those paths are migrated.

Desktop `filesystem.write`, `filesystem.edit`, `filesystem.backup`,
`filesystem.restore`, and `filesystem.backup.cleanup` now use Python approval and
the C++ SFTP service on supported target routes. The host validates durable
action plans and owns backup metadata; backup creation is bounded to 1 MiB and
uses exclusive sibling-file creation, restore verifies both revisions before
guarded atomic replace, and cleanup verifies the backup digest before removing
the remote copy and consuming its host ledger row. Backup listing and retention
use Python for result formatting and per-path planning over bounded host-owned
ledger queries.

The Python Agent resolves the selected desktop server from SQLite and the OS
credential store before starting a one-target C++ service. The desktop service
panel uses the same short-lived service process for read-only service inventory.
Tauri development and new desktop builds select the frozen Python Agent. The
Node runtime and bundle are no longer included in installer resources, and the
desktop launcher has no Node fallback. Legacy Node source remains in the
repository for protocol and migration checks only.

To run the existing desktop UI with the Python Agent sidecar, create
`agent/.venv` and install `agent[agent,models]`. Then run
`pnpm --filter @yukinal/desktop tauri dev`. The `pnpm desktop:dev:python` command
is an equivalent shortcut. `pnpm agent:dev` starts the same source Python Agent,
and `pnpm smoke:sidecar` exercises its stdio protocol. The Tauri host continues
to own the process and execute host-mediated tools.

To create an installer, install `agent[agent,models,package]` into that virtual
environment and run `pnpm package`. The build creates a frozen Python Agent and
the packaged desktop selects it by default.

For the C++ and Python service test suite, install `agent[agent,models,test]`,
configure and build the native service, then run `pnpm check`; the check gate
detects the configured CMake build and runs its CTest suite. For example:

```powershell
python -m pip install -e "agent[agent,models,test]"
cmake -S native -B native/build-check -DYUKINAL_FETCH_DEPS=ON -DYUKINAL_BUILD_TESTS=ON
cmake --build native/build-check --config Release --parallel
$env:YUKINAL_SSH_CMAKE_BUILD_DIR = "native/build-check"
pnpm check
```

The new milestone boundary, API contract, build instructions, and remaining
work are documented in [`docs/rewrite-cpp-python.md`](./docs/rewrite-cpp-python.md).

## Build the new SSH service

Requirements: CMake 3.24+, a C++20 compiler, Python 3.11+, and (on macOS/Linux)
OpenSSL development headers. On the first CMake configure, the build fetches
pinned source versions of libssh2, nlohmann/json, and cpp-httplib.

```powershell
cmake -S native -B native/build
cmake --build native/build --config Release
ctest --test-dir native/build -C Release --output-on-failure
```

Copy [`configs/ssh-service.example.json`](./configs/ssh-service.example.json),
replace the target fields, create its OpenSSH `known_hosts` entry, and set
`YUKINAL_SSH_API_TOKEN` to a random URL-safe value 32–256 characters long. The
configuration keeps SSH credentials as a private-key path or a reference to an
environment variable; it never stores a password value.

For host certificates, `host_certificate_authority` can replace the pinned key
or `known_hosts` trust source. The C++ service verifies the host CA signature,
certificate validity and principal patterns, and optional local or HTTPS OpenSSH
KRL revocations before authenticating. HTTPS KRL downloads verify the server
certificate; plain HTTP is restricted to loopback URLs.

Desktop SSH targets also follow the app's selected proxy route. The C++ service
accepts an optional target `proxy` with the configured HTTP or HTTPS proxy `url`,
an optional `auth_env` reference, and `no_proxy` host patterns. The C++ service
parses and validates the URL, verifies HTTPS proxy certificates, and establishes
the CONNECT tunnel.
Supported bypass patterns include exact hosts, domain suffixes (`.corp` or
`*.corp`), IPv4 prefixes such as `10.*`, IPv4 CIDR ranges such as
`10.0.0.0/8`, IPv6 CIDR ranges such as `2001:db8:abcd::/48`, and `*`. Hostname
and wildcard patterns can be restricted to an SSH port, for example `localhost:22`,
`*.corp:2200`, or `[::1]:2222`; `*:2222` bypasses the proxy for every host on
that port. Proxy credentials belong in the environment variable, not in the JSON
file. Unsupported proxy protocols use
the compatibility route; invalid HTTP(S) proxy URLs or bypass patterns fail
configuration validation in the C++ service.

The Python client can be installed for development with:

```powershell
python -m pip install -e "agent[agent]"
```

To use the C++ service from the existing UI through the Python sidecar,
build the service and run it in one PowerShell session with the same token as
the JSON config. Set the matching environment values in a second session before
starting the Python desktop. Set the service target's `target_id` to the selected
desktop server's opaque `serverId` (for example, `srv_stage1`). The model does
not supply this ID; the Agent binds the request to the selected server.

```powershell
$env:YUKINAL_SSH_API_TOKEN = "<random URL-safe token>"
native/build/Release/yukinal-ssh-service.exe --config path/to/ssh-service.json
```

In the second PowerShell session:

```powershell
$env:YUKINAL_SSH_API_TOKEN = "<same token>"
$env:YUKINAL_SSH_API_URL = "http://127.0.0.1:8765"
pnpm desktop:dev:python
```

The Tauri host forwards the SSH token only to the Python Agent sidecar when the
service is configured. The C++ SSH service still needs to be started separately.

To include the Paramiko SSH mock in CTest, install the optional test extra
before configuring CMake:

```powershell
python -m pip install -e "agent[agent,test]"
```

CMake registers `ssh_mock_integration` when Paramiko is available. This test
covers SSH success, HTTP CONNECT success and proxy-auth rejection, bad SSH
credentials, timeouts, output limits, and unknown or changed host keys without
requiring Docker. When the Agent dependencies are also installed,
`agent_ssh_mock_integration` exercises the Python Agent tool through the C++
service and checks selected-server binding, the task target allowlist, and
rejection of model-supplied target overrides.

Its only enabled operation is:

```python
from yukinal_ssh import SshServiceClient

client = SshServiceClient(token="same-token-as-YUKINAL_SSH_API_TOKEN")
result = client.service_status("staging-web-01", "nginx.service")
print(result.stdout.strip(), result.exit_code)
```

## Common connection errors

- `host_key_unverified`: the server's key does not match the saved pin or
  `known_hosts` entry. Confirm the server identity through the desktop trust
  flow before retrying.
- `authentication_failed`: check the selected SSH identity, key file or
  credential-store reference, and any private-key passphrase.
- `proxy_auth_required`: the HTTP proxy rejected its Basic credential or the
  configured `auth_env` is missing. Store `user:password` in that environment
  variable; do not place it in the service JSON.
- `proxy_connect_failed`: the proxy did not open a CONNECT tunnel. Check the
  proxy host/port and whether it allows CONNECT to the SSH server port.
- `timeout` or `resolver_busy`: retry after checking DNS, network reachability,
  and the operation timeout. DNS lookups have a bounded worker pool.

The Agent Runtime setup, MCP configuration and direct workflow example are in
[`agent/README.md`](./agent/README.md). The combined extras above let CTest
include its MCP/LangGraph tests and the Paramiko SSH mock.

For real SSH protocol checks against the isolated OpenSSH container, configure
with `-DYUKINAL_ENABLE_SSH_CONTAINER_TEST=ON` or run
`python scripts/test_ssh_container.py --service-binary <path-to-executable>`.
The test checks a successful read-only call, bad credentials, timeout/output
limits, and unknown/changed host keys. It requires Docker, `ssh-keygen`, and
`ssh-keyscan`; it generates temporary keys and removes its test container and
image afterward.

## Existing desktop UI

The React UI is shared by the existing Tauri shell and the new C++ WebView host.
The C++ host currently provides the window, IPC bridge, and read-only server and
workspace listings from the existing SQLite database. Writes and operational
commands are still migrating, so the packaged default remains Tauri until parity
is verified. See [the C++ desktop host migration plan](./docs/native-desktop-host.md).

The current Tauri app can still be launched from the repository root with:

```powershell
pnpm install --frozen-lockfile
python -m venv agent/.venv
agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models]"
pnpm check
pnpm --filter @yukinal/desktop tauri dev
```

To run the React UI in the C++ desktop host, use `pnpm desktop:dev:cpp`. The
command builds the shared contracts and C++ host, then starts Vite and the native
window. It accepts `YUKINAL_DESKTOP_HOST_BUILD_DIR` and `YUKINAL_DATA_DIR` for
local build and data-directory overrides.

The package build creates a frozen Python Agent with PyInstaller and includes
available third-party license texts. Installer resources contain the Python
Agent, C++ SSH service, and NOTICE; the legacy Node runtime is not packaged.

The browser preview is available with `pnpm desktop:dev`; it does not connect to
the new SSH service.

## Project docs

- [Product definition](./docs/product.md)
- [Security and data boundaries](./docs/security.md)
- [Rewrite plan and current status](./docs/rewrite-cpp-python.md)
- [C++ desktop host migration](./docs/native-desktop-host.md)
- [Architecture and acceptance criteria](./docs/architecture-and-build-plan.md)
- [Release status](./docs/release.md)
- [Documentation index](./docs/README.md)

The project is MIT-licensed. Third-party dependencies and included assets are
listed in [`NOTICE`](./NOTICE).
