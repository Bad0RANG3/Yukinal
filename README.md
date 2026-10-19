<div align="center">

<img src="./apps/desktop/src/assets/brand-mark.png" alt="Yukinal" width="96" height="96" />

# Yukinal

**AI-native remote development & infrastructure workspace.**

Inspect and manage SSH servers through an operator workspace that pairs a
controlled AI Agent with a hardened, structured SSH service.

[**English**](README.md) · [中文](README.zh-CN.md)

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Version](https://img.shields.io/badge/interface-2.0.0--rc.6-informational.svg)](./packages/shared/src/version.ts)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-6f42c1.svg)](#build-and-run)
[![Python](https://img.shields.io/badge/python-3.11%2B-3776ab.svg)](./agent/README.md)
[![Node](https://img.shields.io/badge/node-%3E%3D24-339933.svg)](package.json)
[![Dependencies](https://img.shields.io/badge/status-rewrite%20in%20progress-orange.svg)](./docs/rewrite-cpp-python.md)

</div>

---

> **Status:** Yukinal 2.0 is an in-progress rewrite that moves SSH operations
> behind a standalone C++ service and a Python Agent while the React interface
> stays the product UI. The current app/package interface version is `2.0.0-rc.6`.
> The Rust/Tauri host and the legacy Node agent have been removed; the C++
> WebView host is now the only desktop host. A CI candidate pipeline builds Windows
> NSIS, macOS DMG, and Debian Linux installers; signing, notarization, and live
> provider/SSH/file-transfer acceptance are still pending. See
> [rewrite status](./docs/rewrite-cpp-python.md) and
> [release status](./docs/release.md).

## Contents

- [What is Yukinal?](#what-is-yukinal)
- [Architecture](#architecture)
- [Repository layout](#repository-layout)
- [Feature status](#feature-status)
- [Build and run](#build-and-run)
- [Configuration](#configuration)
- [Common connection errors](#common-connection-errors)
- [Security model](#security-model)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License](#license)

## What is Yukinal?

Yukinal is a desktop operations workspace for teams that manage a handful of
Linux servers. You describe a goal; the Agent decomposes it, calls the tools it
is allowed to use, reads back the evidence, and reports what actually happened —
while an engineer watches, approves, or takes over.

The product is built around three user paths:

| Path | From | Success looks like |
| --- | --- | --- |
| **Goal-driven AI operations** | Select a server, state a goal | The Agent gathers evidence, runs authorized, structured operations, verifies the remote state, and shows each step |
| **Observability & takeover** | Open a server workspace | Freshness-labelled health, services, logs, files, and task progress; manual takeover when needed |
| **Local ↔ remote files** | Drag from the OS file manager, or browse in the workspace | Streaming upload/download with progress, conflicts, and integrity checks |

The model proposes plans and selects tools. **Authorization, approval, and
execution boundaries are enforced by program code, never by the prompt.**

## Architecture

```text
┌──────────────────────────────────────────────────────────────────┐
│  React desktop UI (TypeScript + Vite)                            │
│  runs inside the C++ WebView host                                 │
└───────────────▲──────────────────────────────────────────────────┘
                │ host IPC (JSON)
┌───────────────┴──────────────────────────────────────────────────┐
│  C++ desktop host  (native/desktop_host)                          │
│  window · IPC router · SQLite · OS credential store · approvals   │
└───────┬───────────────────────────────────────────────┬──────────┘
        │ NDJSON JSON-RPC 2.0 (stdio sidecar)           │ loopback HTTP
┌───────▼──────────────────────────────┐        ┌────────▼──────────┐
│  Python Agent  (agent/)               │        │  C++ SSH service  │
│  LangGraph · Tool Registry · Policy   │───────▶│  (native/src)     │
│  MCP clients · ModelFactory           │  HTTP  │  127.0.0.1 only   │
│  Audit · Approvals · Checkpoints      │        │  structured ops   │
└───────────────────────────────────────┘        └─────────┬─────────┘
                                                           │ SSH / SFTP / PTY
                                                    ┌──────▼──────┐
                                                    │  target(s)  │
                                                    └─────────────┘
```

An optional **control plane** (`services/control-plane`, FastAPI + PostgreSQL)
adds a multi-tenant registry and task broker; nodes connect out to it over
signed requests and a WebSocket channel. It does not change the single-machine
desktop path.

## Repository layout

| Path | What it is |
| --- | --- |
| [`apps/desktop/`](./apps/desktop) | React desktop UI (the product interface) |
| [`native/`](./native) | C++ SSH/SFTP/PTY service and the C++ desktop host |
| [`agent/`](./agent) | Python Agent runtime and the `yukinal_ssh` caller |
| [`services/control-plane/`](./services/control-plane) | Optional FastAPI + PostgreSQL control plane |
| [`packages/shared/`](./packages/shared) | Cross-layer TypeScript contracts, schemas, and IPC fixtures |
| [`packages/agent-sdk/`](./packages/agent-sdk), [`packages/provider-sdk/`](./packages/provider-sdk) | Client SDKs for the Agent transport and provider catalogues |
| [`docs/`](./docs) | Product, security, rewrite, and release documentation |
| [`scripts/`](./scripts) | Build, check, packaging, and smoke-test tooling |
| [`configs/`](./configs) | Example service configuration |

## Feature status

| Area | State |
| --- | --- |
| Standalone C++ SSH service on `127.0.0.1` with structured operations | ✅ Implemented |
| Host-key verification (`known_hosts`, SHA-256 pin, or host CA + KRL) | ✅ Implemented |
| Proxy routes (HTTP/HTTPS CONNECT, SOCKS5/SOCKS5H) and bypass rules | ✅ Implemented |
| Bounded SFTP list/stat/read/write/backup, guarded atomic replace | ✅ Implemented |
| C++ desktop host: window, IPC router, SQLite, credentials, approvals | ✅ Implemented |
| Python Agent: LangGraph loop, Tool Registry, policy, redacted audit | ✅ Implemented |
| MCP clients (stdio + Streamable HTTP) with approval-required defaults | ✅ Implemented |
| Model factory: OpenAI-compatible, Anthropic, Gemini | ✅ Implemented |
| Durable approvals with per-call fingerprint binding and expiry | ✅ Implemented |
| Control plane: tenants, nodes, tasks, approvals, policy, audit | ✅ Implemented (SQLite-verified) |
| Durable task/plan recovery | 🚧 Partial |
| Windows NSIS, macOS DMG, Debian Linux installers | 🧪 Candidate CI build and install smoke |
| Code signing, notarization, real provider/SSH and GUI acceptance | ❌ Pending |

Full detail and evidence live in the [rewrite log](./docs/rewrite-cpp-python.md)
and [release status](./docs/release.md).

## Build and run

**Prerequisites**

- **Node.js ≥ 24** and **pnpm 11.8.0** (`corepack enable` pins it via
  `package.json`).
- **Python 3.11+**.
- **CMake 3.24+** and a **C++20** compiler (MSVC/NMake or MinGW on Windows;
  a system compiler plus OpenSSL headers on macOS/Linux).
- On Windows, a full **Perl** distribution with `IPC::Cmd` (Strawberry Perl
  works) and a Visual Studio 2022 developer shell.

**1. Install workspace dependencies**

```bash
pnpm install --frozen-lockfile
```

**2. Set up the Python Agent**

```bash
python -m venv agent/.venv
# Windows
agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models]"
# macOS / Linux
agent/.venv/bin/python -m pip install -e "agent[agent,models]"
```

**3. Build the C++ SSH service**

```bash
cmake -S native -B native/build -DCMAKE_BUILD_TYPE=Release
cmake --build native/build --config Release
ctest --test-dir native/build -C Release --output-on-failure
```

**4. Run the full check gate**

```bash
pnpm check
```

`pnpm check` runs repository hygiene and secret scanning, the docs-link gate,
the CTest suite (C++ service/host + Python Agent), the control-plane tests when
its venv exists, TypeScript build/typecheck/tests, the desktop bundle, and the
Python sidecar smoke test. It also guards its own configuration: an explicitly
set `YUKINAL_SSH_CMAKE_BUILD_DIR` that cannot run tests stops the gate with the
repairing `cmake` command instead of skipping the native suite behind a green
line. It does **not** run a linter, formatter, or static analysis (no
clang-format/clang-tidy, eslint, ruff, or mypy), so style and static issues still
need human review.

**5. Run the desktop**

```bash
pnpm desktop:dev:cpp   # C++ WebView host + Vite (the real desktop)
pnpm desktop:dev       # browser-only UI preview (no SQLite/SSH/PTY)
```

The native development window stays connected to Vite: saving React or CSS files
updates the open window automatically. Its AI header shows a development-only
live-preview indicator and a manual refresh button. Manual refresh is disabled
while an AI run or unsent input is present. Native C++ changes require rebuilding
and reopening the host. Set `YUKINAL_DESKTOP_DEV_PORT` to choose another local port
when the default port is occupied.

`pnpm package` stages a checksummed distributable payload. The release-candidate
workflow wraps it as a Windows NSIS installer, macOS DMG, and Debian Linux package,
then installs, launches, removes, and checks data retention on each hosted runner:

```bash
pnpm build:ssh-service
pnpm build:python-agent
pnpm package           # stages target/release/bundle/<name>/ + manifest
pnpm package -- --verify
```

## Configuration

Copy [`configs/ssh-service.example.json`](./configs/ssh-service.example.json),
fill in an allow-listed target, add the host key to `known_hosts` (or pin its
fingerprint / configure a host CA), and set `YUKINAL_SSH_API_TOKEN` to a random
URL-safe value of 32–256 characters.

Each target declares **exactly one** trust source (`known_hosts_file`,
`host_key_fingerprint`, or `host_certificate_authority`) and **exactly one**
authentication source (a private-key file, a private-key environment reference,
a password environment reference, or `{"type":"agent"}`). No password value is
ever stored in the JSON.

```powershell
$env:YUKINAL_SSH_API_TOKEN = "<random URL-safe token>"
native/build/Release/yukinal-ssh-service.exe --config path/to/ssh-service.json
```

To drive the service from the UI, set the matching token/URL in a second shell
and start the desktop:

```powershell
$env:YUKINAL_SSH_API_TOKEN = "<same token>"
$env:YUKINAL_SSH_API_URL   = "http://127.0.0.1:8765"
pnpm desktop:dev:cpp
```

The desktop host forwards the SSH token only to the Python Agent sidecar, and
only when the service is configured. The C++ SSH service is started separately.

## Common connection errors

| Error | Meaning | Next step |
| --- | --- | --- |
| `host_key_unverified` | The server key doesn't match the saved pin or `known_hosts` entry | Confirm the server identity through the desktop trust flow |
| `authentication_failed` | Bad identity, key file, or passphrase | Re-check the selected SSH identity and credential reference |
| `proxy_auth_required` | The proxy rejected its Basic credential | Store `user:password` in the referenced env var, never in JSON |
| `proxy_connect_failed` | The proxy refused the CONNECT tunnel | Check the proxy host/port and whether it allows CONNECT to the SSH port |
| `timeout` / `resolver_busy` | Slow DNS or network | Retry after checking reachability; DNS uses a bounded worker pool |

## Security model

- **Default deny.** Unconfigured hosts, users, tools, and operations are
  refused.
- **Target allowlisting.** The model never supplies the host; targets resolve
  through a controlled `target_id` allowlist.
- **Host-key verification is mandatory.** Unknown or changed keys fail closed.
- **Approval is bound to the exact action.** A fingerprint covers target, tool,
  and full arguments; changed arguments require a new approval, and approvals
  carry a window and a use budget.
- **Bounded execution.** Timeouts, output sizes, concurrency, and tool-call
  counts are capped; sensitive values are redacted from logs and audit.
- **Untrusted input.** Agent and MCP output are treated as data, never as
  instructions that can override policy.

The full boundary is documented in [security and data boundaries](./docs/security.md).

## Documentation

| Doc | Read it for |
| --- | --- |
| [Documentation index](./docs/README.md) | The map of all project docs |
| [Product definition](./docs/product.md) | Users, scenarios, and feature trade-offs |
| [Security & data boundaries](./docs/security.md) | What the Agent may do and when it needs approval |
| [Rewrite plan & current status](./docs/rewrite-cpp-python.md) | The C++/Python rewrite stage and interfaces |
| [C++ takeover & Rust removal plan](./docs/rust-removal-plan.md) | From-scratch C++ host capability guidance |
| [C++ desktop host migration](./docs/native-desktop-host.md) | Desktop host architecture |
| [Architecture & acceptance criteria](./docs/architecture-and-build-plan.md) | Goals and release acceptance conditions |
| [Release status](./docs/release.md) | Install, build, test, and packaging status |
| [Changelog](./docs/changelog.md) | Version history |
| [Agent README](./agent/README.md) | Agent runtime, MCP, and provider setup |
| [SSH service README](./native/README.md) | Building and running the C++ service |
| [Control plane README](./services/control-plane/README.md) | Multi-tenant registry and task broker |

## Contributing

Contributions are welcome. Start with the [contributing guide](./CONTRIBUTING.md)
and the [documentation index](./docs/README.md), then open a focused PR that
keeps code, tests, and docs in sync. Report security issues privately via the
[security policy](./SECURITY.md).

## License

Yukinal is released under the [MIT License](./LICENSE). Third-party
dependencies and included assets are listed in [NOTICE](./NOTICE).
