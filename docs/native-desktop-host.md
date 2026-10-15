# C++ desktop host migration

## Target shape

The desktop keeps the existing React/Vite UI. The native desktop host, desktop IPC,
local data and credential boundaries, process supervision, and SSH operations move
to C++. The Python sidecar remains responsible for the Agent workflow, providers,
MCP, and tool orchestration. The final desktop package must not contain a Tauri or
Rust host.

```text
React UI
  │ typed command and event contract
  ▼
C++ desktop host ── SQLite / OS credential store / file and URL dialogs
  ├── C++ SSH service or in-process SSH core
  └── Python Agent sidecar (newline-delimited JSON-RPC)
```

The current Rust desktop host remains available as a temporary fallback while each
capability is migrated and checked. It is not a target dependency and receives no
new product features. Remove it only after the C++ host covers the UI contract and
the installer, persistence, credential, and recovery acceptance checks.

## Native WebView and IPC

The C++ window uses the pinned `webview/webview` C++ library over the operating
system WebView: WebView2 on Windows, WebKit on macOS, and WebKitGTK on Linux. The
React code still owns appearance, navigation, and interaction. Production serves
the bundled `apps/desktop/dist` files from a random port bound only to
`127.0.0.1`; the static server accepts same-origin `GET` and `HEAD` requests and
does not expose backend routes. Development URLs must be HTTP on `127.0.0.1` only.
Native calls use an in-process WebView binding, not an HTTP API.

`apps/desktop/src/lib/ipc.ts` remains the only UI boundary. Its command names,
parameters, responses, and event payloads come from `packages/shared`. The C++ host
validates every call again. Events are subscribed by name and delivered through
the same bridge. Commands return a stable error until their C++ implementation is
migrated; they must not silently fall back to Rust.

## Migration order

1. Add the C++ WebView shell, version handshake, command bridge, event subscriptions,
   and UI loading from loopback development or a bundled directory served on loopback.
2. Move server records, proxy/provider/MCP settings, credentials, and SQLite state
   into C++ storage boundaries while preserving current UI contract shapes.
3. Move server lifecycle, host-key trust, terminal, SFTP and transfer orchestration
   to C++. Continue using the C++ SSH service implementation already in `native/`.
4. Connect the Python sidecar to the C++ host RPC. C++ remains the authority for
   identity, target scope, approvals, and persistence; Python runs model and tool
   workflows.
5. Port task history, schedules, recovery, audit and remaining UI commands. Validate
   each command and event against the shared contracts and existing desktop tests.
6. Build and smoke-test Windows, macOS, and Linux installers. Remove Tauri/Rust and
   the retired TypeScript Agent runtime after parity is demonstrated.

## First native-host slice

The initial C++ shell opens the existing React UI, exposes `core_ping`, routes
external links through the OS without invoking a shell, and provides event
subscription plumbing. Unmigrated commands return `NOT_IMPLEMENTED`; they never
invoke the Tauri host. The application can be launched with `--dev-url
http://127.0.0.1:1420` or `--ui-dir <directory-containing-index.html>`.

The upstream WebView library is MIT licensed and its C++ API supports native
callbacks in both directions: [webview/webview](https://github.com/webview/webview).
