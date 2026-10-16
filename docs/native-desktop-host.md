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

### Windows window geometry

The Windows host owns the outer boundary. DWM supplies the top-level window
corners and system caption buttons. The caption occupies the top 40 logical
pixels, scaled by the window DPI. On a wide three-column workspace, a capable
WebView2 runtime lets the WebView extend into that area so the existing AI
toolbar sits inside the caption. The host excludes the native button strip and
restored-window top resize edge from the WebView region. Compact windows and
runtimes without this capability keep the WebView below the native caption.

React gives only the central AI workspace a 16-logical-pixel rounded inner frame,
using the shared panel radius (twice the control radius), inset
12 pixels from the content's bottom. In the three-column desktop layout, its
surface starts 12 logical pixels below the window's top. In the caption overlay,
React paints the complete inner frame. The older-runtime fallback joins React's
straight top edges to a Win32-painted rounded cap at the WebView boundary.
The fused outer and AI surfaces use neutral gray values on both sides of this
boundary, since the WebView's monochrome filter does not affect native painting.
The left navigation and right dashboard sit directly on the window background.
Both use `clamp(280px, 100px + 11vw, 360px)`, recalculating equally on resize and
growing gradually in a maximized window. The centered
AI inner frame fills its column and adapts to the available width.
Its native caption cap follows the measured frame edges. Compact navigation
still switches at the existing breakpoints, and phone-width layouts use the
available inner-frame width.
These are product values, rather than HIG requirements. React does not draw an outer window,
caption buttons, or a second title bar. Browser development mode previews content
only, with a 12-pixel top inset. Compact layouts retain the complete native caption
so it can share space with Windows controls. Maximizing lets Windows square the outer boundary while retaining the
AI workspace's inner frame. Window movement, resizing, system menus,
caption actions, and Snap hit testing stay with Win32 and DWM. The sidebar owns
the single visible application brand; panel names are not repeated in the caption.
The AI welcome and input form share a starting area slightly above center. Once a conversation
has messages, the transcript scrolls above the bottom input area. Transcript and
input widths share a maximum 760-pixel reading measure, bounded by the frame
in narrower windows; readiness notices stay beside the input they explain.
The dark shell uses separate window, content, and raised-control surfaces. The
composer shares the AI surface's #282828 background and has no glow, gradient,
or shadow in resting, focused, or attachment-drag states. Nested server resources
use a recessed surface. The outer-to-AI boundary uses only a faint edge and
soft contact shadow, including its native caption cap. The welcome title is centered above the input; the
28/14 title-to-body ratio and 8/24 grouping rhythm are calibrated from the Apple
Music Windows v1.1 reference, not claimed as Apple implementation values.
Empty-workspace task starters populate an empty draft even before the Agent is
ready; sending still requires a ready runtime and model. They do not submit, replace attachments, or change permissions. Empty
dashboard guidance describes available information without fabricating metrics.
Caption hit testing falls back to DWM's current button bounds when the default
handler reports a client hit. The conversion uses the outer window's screen
origin, including maximized invisible borders, and keeps native non-client
button codes so Windows still owns button behavior and Snap Layouts.

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
The OS window title reflects that same context; the caption does not repeat it.
`WindowShellContextSchema` is the presentation contract; the host independently
validates its bounded fields. Credentials, prompts, and approval grants do not
belong in that context.

As functions move into the frame, native controls must dispatch explicit actions
to the existing application services. Navigation, selected server, task state,
and approvals keep one source of truth. The shell must not acquire a separate
Agent loop or a parallel server-action path. Visual tokens, typography, spacing,
focus, and active/inactive states should evolve with the content design as one
system, while Win32 continues to own native geometry and window management.
`windowShell.updateSurface` is a separate presentation channel. A ResizeObserver
keeps the cap aligned with the actual AI panel; a hidden or compact workspace
clears it. Its strict shared schema and independent native checks accept only
bounded CSS geometry and opaque colors. Native buttons keep their existing
surface. After a successful surface update, the optional
`windowShell.titlebarOverlay` capability moves the WebView into the caption and
`WindowFrame` applies the matching layout. The left brand moves into the caption beside the AI toolbar, and navigation
moves upward with it; the right panel retains its content inset. The same AI toolbar and application handlers remain
in use; there is no second set of native business controls.

The Windows build pins WebView2 SDK 1.0.2903.40. At runtime the host queries
`ICoreWebView2Settings9` and enables `IsNonClientRegionSupportEnabled` before
navigation. CSS `app-region: drag` gives the AI header native dragging, system
menus, and double-click maximize/restore; interactive descendants explicitly use
`no-drag`. Unsupported runtimes retain the separate-caption fallback. Native
window commands do not travel through renderer IPC.

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
   workflows.
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

The upstream WebView library is MIT licensed and its C++ API supports native
callbacks in both directions: [webview/webview](https://github.com/webview/webview).
