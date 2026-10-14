# Yukinal

Yukinal is a desktop operations workspace for inspecting and managing SSH
servers. The React interface remains the product UI; the current 2.0 rewrite is
starting with a narrow C++ SSH service and a Python caller.

## Rewrite status

The current rewrite implements **milestones 1 through 3** from
[`ssh_agent_development_brief.md`](./ssh_agent_development_brief.md):

- a standalone C++ service bound to `127.0.0.1`;
- a Python caller over local HTTP/JSON;
- one read-only structured SSH operation, `service_status`;
- target allowlisting, strict SSH host-key verification, bounded timeouts and
  output, and credentials referenced from environment variables or private-key
  files;
- a Python Tool Registry with JSON Schema validation, per-call tool/target
  authorization, parameter-bound approval grants, and redacted audit events;
- Streamable HTTP and stdio MCP clients, with remotely discovered tools
  defaulting to approval-required;
- a bounded LangGraph workflow with tool-call, total-time, and output-size
  limits.
- a configuration-driven OpenAI/Anthropic model factory using official
  LangChain adapters;
- a model-driven LangGraph tool loop with tenant/user/target/risk policy,
  exact-argument approval requests, and bounded model output.

Approval UI, durable task state, and desktop UI wiring remain later milestones.
The new runtime is invoked by Python for now; the existing React/Tauri
application has not yet been connected to it.

The C++ service is not yet connected to the desktop UI. The existing React
components and their Tauri bridge are preserved. The existing Rust/Tauri
implementation remains in the repository as the current desktop backend while
the replacement is built in small, reviewable stages; this milestone does not
claim full product parity.

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

To include the Paramiko SSH mock in CTest, install the optional test extra
before configuring CMake:

```powershell
python -m pip install -e "agent[agent,test]"
```

CMake registers `ssh_mock_integration` when Paramiko is available. This test
covers SSH success, bad credentials, timeouts, output limits, and unknown or
changed host keys without requiring Docker.

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
pnpm check
pnpm --filter @yukinal/desktop tauri dev
```

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
