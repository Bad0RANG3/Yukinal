# Yukinal

Yukinal is a desktop operations workspace for inspecting and managing SSH
servers. The React interface remains the product UI; the 2.0 rewrite moves SSH
operations toward a C++ service and Python Agent.

## Rewrite status

The brief's first SSH foundation stage is complete; the broader migration is
still in progress. Current components include:

- a standalone C++ service bound to `127.0.0.1`;
- a Python caller over local HTTP/JSON;
- structured SSH operations: `service_status`, `service_inventory`, and
  bounded SFTP listing, metadata, read, write, exclusive backup creation, removal,
  and link-count probes for supported desktop Agent identities;
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
low-risk. The Python sidecar now round-trips approvals for tools that explicitly
require them, including reject and expiry handling; the current desktop catalog
still exposes read-only tools. Durable task state and broader tool coverage
remain in progress. The existing React/Tauri UI remains intact while those
paths are migrated.

The Python Agent resolves the selected desktop server from SQLite and the OS
credential store before starting a one-target C++ service. The desktop service
panel uses the same short-lived service process for read-only service inventory.
Tauri development and new
desktop builds now select the frozen Python Agent. The Node runtime and bundle
remain staged as a temporary fallback while the remaining desktop migration is
validated.

To run the existing desktop UI with the Python Agent sidecar, create
`agent/.venv` and install `agent[agent,models]`. Then run
`pnpm --filter @yukinal/desktop tauri dev`. The `pnpm desktop:dev:python` command
is an equivalent shortcut. The Tauri host continues to own the process and
execute host-mediated tools.

To create an installer, install `agent[agent,models,package]` into that virtual
environment and run `pnpm package`. The build creates a frozen Python Agent and
the packaged desktop selects it by default.

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
covers SSH success, bad credentials, timeouts, output limits, and unknown or
changed host keys without requiring Docker. When the Agent dependencies are also
installed, `agent_ssh_mock_integration` exercises the Python Agent tool through
the C++ service and checks selected-server binding, the task target allowlist,
and rejection of model-supplied target overrides.

Its only enabled operation is:

```python
from yukinal_ssh import SshServiceClient

client = SshServiceClient(token="same-token-as-YUKINAL_SSH_API_TOKEN")
result = client.service_status("staging-web-01", "nginx.service")
print(result.stdout.strip(), result.exit_code)
```

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

The current desktop app is still built from the React/Tauri baseline. From the
repository root, install the locked dependencies and run the checks/UI with:

```powershell
pnpm install --frozen-lockfile
python -m venv agent/.venv
agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models]"
pnpm check
pnpm --filter @yukinal/desktop tauri dev
```

The package build creates a frozen Python Agent with PyInstaller and includes
available third-party license texts. The old Node runtime remains in the
installer as a transition fallback; removing it follows installed-app parity
and lifecycle acceptance.

The browser preview is available with `pnpm desktop:dev`; it does not connect to
the new SSH service.

## Project docs

- [Product definition](./docs/product.md)
- [Security and data boundaries](./docs/security.md)
- [Rewrite plan and current status](./docs/rewrite-cpp-python.md)
- [Architecture and acceptance criteria](./docs/architecture-and-build-plan.md)
- [Release status](./docs/release.md)
- [Documentation index](./docs/README.md)

The project is MIT-licensed. Third-party dependencies and included assets are
listed in [`NOTICE`](./NOTICE).
