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

The Rust/Tauri desktop host and the legacy Node agent have been removed. The C++
host is the only desktop host; capabilities that have not moved yet return a stable
error instead of falling back to a host that no longer exists. Packaging/installers
are not implemented yet; the command-level gap list and implementation order are in
[the C++ takeover and Rust removal plan](./rust-removal-plan.md).

## Native WebView and IPC

Product decisions and implementation boundaries are defined in the
[Windows desktop window direction](./desktop-window-direction.md).

### Windows window geometry

The Windows host owns the two rounded boundaries. DWM supplies the outer
top-level window corners. The native title/status strip and its window controls
share a 40-logical-pixel height. A fixed 240-DIP native sidebar joins that strip;
the embedded WebView uses the remaining width, with 8-DIP right/bottom insets
and a 14-DIP corner radius. The minimum window size is 1100 × 680. These product
values scale with the window DPI; they are not HIG requirements.

React fills that child window and owns only content layout. It does not draw an
outer window, caption buttons, or a second title bar. Browser development mode
previews content only. Maximizing lets Windows square the outer boundary while
retaining the inset content surface. Window movement, resizing, system menus,
caption actions, and Snap hit testing stay with Win32 and DWM. Native-painted
caption controls use system window commands and `HTMAXBUTTON` hit testing;
they are not HTML controls.

Native acceptance must check drag, edge/corner resize, caption-button hover,
Snap Layouts, minimize/restore, maximize/restore, inactive appearance, and moving
between displays with different DPI. A browser screenshot or build success
does not verify these behaviors.

### Integrated application shell

The native frame is an application control surface. It shares the content's
workspace context rather than maintaining an independent selection or task
model. `WindowFrame` observes the existing workspace store and server query;
the optional Windows `windowShell.updateContext` capability sends the selected
section, server name, connection state, and Agent busy state to the native host.
The title/status strip and the OS window title reflect that same context.
`WindowShellContextSchema` is the presentation contract; the host independently
validates its bounded fields. Credentials, prompts, and approval grants do not
belong in that context.

The native sidebar projects the same server cache and dispatches validated UI
intents for navigation, selection, add/refresh and server context-menu actions.
React re-resolves IDs and calls existing dialogs and application services.
Busy/modal state blocks those intents; deletion retains its confirmation.
Navigation, selected server, task state,
and approvals keep one source of truth. The shell must not acquire a separate
Agent loop or a parallel server-action path. Visual tokens, typography, spacing,
focus, and active/inactive states should evolve with the content design as one
system, while Win32 continues to own native geometry and window management.
The capability is optional: older hosts and browser previews retain the React
sidebar. Shared theme-token transport and native task controls remain future work.

The C++ window uses the pinned `webview/webview` C++ library over the operating
system WebView: WebView2 on Windows, WebKit on macOS, and WebKitGTK on Linux. The
React code still owns appearance, navigation, and interaction. Production serves
the bundled `apps/desktop/dist` files from a random port bound only to
`127.0.0.1`; the static server accepts same-origin `GET` and `HEAD` requests and
does not expose backend routes. Development URLs must be HTTP on `127.0.0.1` only.
Native calls use an in-process WebView binding, not an HTTP API.

`apps/desktop/src/lib/ipc.ts` remains the only UI boundary. Its command names,
parameters, responses, and event payloads come from `packages/shared`. The C++ host
validates the RPC framing and each migrated command's inputs. Events are subscribed
by name and delivered through the same bridge. Commands return a stable error until
their C++ implementation is migrated; they must not silently fall back to Rust.

The C++ host opens or creates the same `yukinal.db` used by the current desktop.
`YUKINAL_DATA_DIR` overrides the platform app-data directory, matching the legacy
host. It applies the ordered schema migrations through version 36, one transaction
per version, refuses newer databases, and makes a checksummed SQLite backup before
migrating a nonempty existing database. If an upgrade fails, it restores that copy
and removes the failed connection's WAL/SHM files. Server CRUD, OS credential
storage, migration execution, and the outbound network-proxy setting now run in
C++; the React UI keeps using the shared IPC contract.

## Migration order

1. Complete the C++ WebView shell, command bridge, event subscriptions, UI loading,
   and the first shared-SQLite read commands.
2. Move server CRUD, OS credential storage, and schema migrations into C++, then
   migrate the remaining proxy/provider/MCP settings and the remaining SQLite
   repositories.
3. Move server lifecycle, host-key trust, terminal, SFTP and transfer orchestration
   to C++. Continue using the C++ SSH service implementation already in `native/`.
4. Connect the Python sidecar to the C++ host RPC. C++ remains the authority for
   identity, target scope, approvals, and persistence; Python runs model and tool
   workflows. This is now started: `native/desktop_host/src/sidecar.cpp` launches
   and supervises the Agent, correlates NDJSON/JSON-RPC frames, streams
   `agent.stream` notifications to the WebView, and answers
   `host.credentials.set/get/reclaim`, `host.mcp.catalog`, and `host.tool.cancel`.
   `agent_spawn/status/kill/logs`, the six `provider_*` commands, and
   `agent_run_start`/`agent_run_stop`/`agent_approval_respond` are routed through
   it. The run commands are forwarded unchanged so the Python Agent Node keeps
   owning durable admission, cancellation and one-time approvals. Remaining host
   RPC handlers (`host.tool.execute`, investigation and MCP) are next.
5. Port task history, schedules, recovery, audit and remaining UI commands. Validate
   each command and event against the shared contracts and existing desktop tests.
6. Build and smoke-test Windows, macOS, and Linux installers (not implemented yet;
   see [the Rust removal plan](./rust-removal-plan.md)).

## Native-host storage and server CRUD

The C++ shell opens the existing React UI, exposes `core_ping`, and routes
external links through the OS without invoking a shell. It also initializes and
migrates the local SQLite database, handles server CRUD and OS credential storage,
reads `server_list` and `workspace_list`, and provides event subscription plumbing.
The outbound network-proxy setting is migrated too: `network_proxy_get` reads the
stored mode and reports whether a credential exists in the OS store, and
`network_proxy_save` keeps the write-only `user:password` credential out of SQLite
and out of every response. Local host-key trust is partially migrated:
`server_host_key_status` and `server_host_key_forget` read and write the same
`known_hosts` v1 file as the connection path, with a changed fingerprint reported
as a mismatch rather than silently replaced. Unmigrated commands return
`NOT_IMPLEMENTED`; they never invoke the Tauri host.
The application can be launched with `--dev-url
http://127.0.0.1:1420` or `--ui-dir <directory-containing-index.html>`. Pass
`--data-dir <absolute-path>` to select a data directory for development.

The local file browser's native path selection now runs in C++. `local_path_pick`
opens the platform file or folder picker and stores the chosen paths in a
short-lived handle table; `local_file_preview` takes only the opaque handle id
back, revalidates that the path is still a regular file (not a symlink), and
returns the shared bounded `FilePreviewResponse` for text, image, or binary
content. Absolute local paths never cross the IPC boundary, and `emit_local_drop`
remains the last piece of this group.

The upstream WebView library is MIT licensed and its C++ API supports native
callbacks in both directions: [webview/webview](https://github.com/webview/webview).
