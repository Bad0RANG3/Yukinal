# SSH Service (C++)

The rewrite uses a standalone C++ service. It binds only to `127.0.0.1`, accepts
structured requests, resolves `target_id` from a configured allowlist and
exposes service queries, probe-only host-key discovery, and bounded SFTP list, stat, read, write,
exclusive-create, no-replace publication, remove, guarded atomic replacement, and link-count operations.
Host keys can be checked against an OpenSSH `known_hosts` file, an exact
`SHA256:` pin from the desktop trust store, or an OpenSSH host CA certificate.
Host CA verification checks the certificate signature, host certificate type,
validity interval, configured principal patterns, critical options, and optional
OpenSSH KRL revocations in C++. KRL files must use absolute paths and stay below
16 MiB. Remote KRLs use HTTPS with certificate verification; plain HTTP is
allowed only for loopback URLs. Redirects, URL credentials, queries, and
fragments are rejected. Signed KRLs require a valid configured signer. Authentication supports password,
private key (file or named environment reference), and the local SSH agent.
Agent authentication uses `SSH_AUTH_SOCK` on Unix and libssh2's platform Agent
integration on Windows. The socket path is forwarded only when a target in the
allowlist selects agent authentication.

The service has no general-purpose command endpoint. The bounded `server_exec`
operation is accepted only for a target whose one-shot capability is explicitly
enabled in that service process; command bytes, timeout, output and environment
parameters are validated before SSH execution. The separate `--terminal` worker
mode is used only by the manually opened desktop terminal; it accepts no HTTP
requests. Supported desktop terminal sessions and manual SFTP transfers also
use C++ workers.
The `host_key_probe` operation is restricted to targets configured with
`probe_only: true` and `auth: {"type":"probe"}`; those targets cannot perform
any authenticated operation and do not configure a trust source. The desktop
uses this shape only for its separate user-confirmed host-key probe flow.
SFTP writes are capped at 512 KiB; exclusive recovery-file creation is capped
at 1 MiB and removes a partial file if writing or syncing fails. Guarded replacement
compares size, mtime, and SHA-256; stages beside the target with exclusive create;
preserves and verifies mode, mtime, owner, and group; and publishes with the
OpenSSH POSIX atomic rename extension. It fails closed when that extension is not
available. The desktop
calls the one-path remove operation only after it checks backup ownership and
revision. The link-count operation runs a fixed, quoted `stat` probe. The
desktop routes supported Agent file operations, file browsing, and manual
transfers through C++ for password, private-key, OpenSSH user-certificate, and
agent identities when the target uses a supported host-key trust mode and a
supported direct, HTTP/HTTPS CONNECT, or SOCKS5/SOCKS5H route. SOCKS5 resolves
target names locally; SOCKS5H delegates name resolution to the proxy. The desktop
passes the configured proxy URL through as data; URL parsing, HTTPS certificate
verification, host and optional port-qualified bypass matching, and
CONNECT/SOCKS5 negotiation run in C++. Supported interactive terminal
sessions use the dedicated C++ PTY worker. The C++ terminal worker forwards
keyboard-interactive challenges through the existing desktop prompt UI. SSH
configurations the C++ service cannot represent are rejected by the desktop;
they do not fall back to a Rust SSH connection. When pin or `known_hosts` trust is used with a host
certificate, C++ fingerprints the certified public key, matching the desktop
trust store.

## Execution leases and outbound audit

Every authenticated `POST /v1/ssh/exec` requires an `execution` object in addition
to the existing structured operation fields:

```json
{"id":"unique-random-id-at-least-16-chars","expires_at_ms":1795000000000}
```

The caller supplies an actual future Unix-millisecond expiry, bounded to eleven
minutes. The trusted target's `execution_expires_at_ms`, when present, can only
shorten that lifetime. IDs are single use, including failed calls. Authenticated
`POST /v1/ssh/cancel` with `{"execution_id":"..."}` revokes a running or queued
operation; cancellation before execution creates a permanent tombstone for the
service's lifetime. A socket watchdog closes an active connection on expiry or
revocation. A disconnected SSH channel cannot guarantee termination of a process
that the remote host has deliberately detached; such results remain unknown.
Three execution slots leave one HTTP worker available for cancellation.

`confirmation_read_only: true` permits only the service's explicit read-operation
allowlist and disables every PTY write. The PTY worker accepts `write` frames
only with `source: "user_pty"` from the user-only desktop IPC endpoint. There is
currently no AI PTY write capability: `agent_tool`, missing, and unknown sources
are rejected and close the worker. Adding AI PTY support requires a separately
verified, single-use ChangePlan-step approval for **each** write; a session grant
must never authorize stdin indefinitely.

`stream_audit_file` selects an independent append-only JSON-lines stream audit
(default `var/audit/ssh-stream.jsonl`; desktop targets supply an absolute app-data
path). Records include source (`agent_tool`, `user_pty`, or `system`), target,
session/execution correlation IDs and byte count. Manual PTY stdin records are
metadata-only: even a SHA-256 digest of a single keystroke can be enumerated.
Other outbound records include SHA-256; records contain no raw command, password,
private key or terminal bytes and do not support session replay. Audit failure blocks the write.
These records complement, rather than replace, the redacted tool-call audit.

All shell arguments use the common `shell_quote` primitive even when an
allowlist also validates them. The service uses libssh2, never an `ssh`/`scp`
subprocess, and therefore never consults OpenSSH ControlMaster/ControlPath files.
Explicit nested SSH, multiplexing settings, and socket-access commands are
blocked in `server_exec`. That extra check is not a sandbox for arbitrary
explicitly approved shell programs. The separate configured authentication-agent
socket is used only for authentication, never as a model-selected file path.

SFTP operations resolve their configured roots and requested paths using the
remote SFTP realpath operation before metadata queries or opens. Requests outside
the lexical and resolved boundary, traversal, escaped symlinks, and socket paths
receive the same policy denial, including optional existence queries. New files
require an allowed canonical parent; dangling symlinks are refused. Operations
use the resolved path, and listings return `resolved_path` plus canonical entry
paths. SFTP does not provide an `openat`/directory-descriptor confinement primitive;
the SSH account must not allow an adversary to rename its permitted directories
during an operation. Host-side filesystem isolation remains necessary against
such a concurrent malicious filesystem owner.

## Terminal display and file boundaries

The desktop connection editor persists `truecolor` (default `true`) and
`sftpAllowedRoots` (default `["."]`, the remote account's home directory).
The service target uses `truecolor` and `sftp_allowed_roots` with the same defaults.
Existing databases migrate to these restricted defaults; explicitly add absolute
roots such as `/srv/app` before using file tools outside the account's home.
SFTP canonicalizes each path and its configured roots remotely before access.
Directory listings return canonical paths, including empty directories.

PTY requests keep `TERM=xterm-256color`. With `truecolor` enabled, the SSH channel
sends a `COLORTERM=truecolor` environment request before starting the shell. The
server must allow it with `AcceptEnv COLORTERM` in `sshd_config`. An OpenSSH client
connecting independently also needs `SendEnv COLORTERM`; Yukinal sends the channel
request directly and does not read `ssh_config`. Servers may decline the variable;
256-colour compatibility remains available. The switch only controls advertising:
xterm renders `38;2;r;g;b` / `48;2;r;g;b` truecolor sequences by default.

Remote output passes a stateful SGR allowlist before terminal rendering, both in
the native worker and the UI. Cursor movement, erase-screen, mode changes,
OSC hyperlinks/clipboard/title and DCS strings are discarded, even when split
across frames. Tabs, newlines and carriage returns are retained for text layout.
This intentionally limits full-screen TUIs and cursor-based shell redisplay;
`ls --color`, ordinary `git status` and `git --no-pager diff` remain coloured.
AI Markdown removes all ANSI rather than interpreting it.

The terminal uses the application's surface/text colours with a complete
16-colour dark or light palette, cursor and selection colours. Theme changes
replace the whole palette. Monospaced fonts disable ligatures; pane/window/font
changes run FitAddon and send the resulting columns/rows through the native
PTY resize request. The current transport is C++/libssh2, not Tauri/russh.
Only trusted desktop keyboard/paste writes carry `source: "user_pty"`; model
tools cannot reuse that PTY. Agent commands use separately approved exec calls.

## Build

```powershell
cmake -S native -B native/build -DCMAKE_BUILD_TYPE=Release
cmake --build native/build --config Release
ctest --test-dir native/build -C Release --output-on-failure
```

The default CMake build fetches pinned source releases of nlohmann/json,
cpp-httplib, and libssh2. Windows also builds pinned OpenSSL 3.5.9 static crypto
and TLS libraries and configures libssh2 to use them. This enables Ed25519
alongside RSA and ECDSA, verifies host certificates, downloads HTTPS KRLs, and
keeps OpenSSL inside the standalone service.
The Windows build requires a full Perl distribution with `IPC::Cmd` and
`Locale::Maketext::Simple` (Strawberry Perl works; Git for Windows' minimal
Perl is incomplete) and a Visual Studio 2022 Developer Command
Prompt/PowerShell with MSVC, NMake, and the Windows SDK. OpenSSL is built
without assembly optimizations so NASM is not required. Set
`-DYUKINAL_BUNDLE_WINDOWS_OPENSSL=OFF` only when you intentionally want the
legacy WinCNG backend. On macOS/Linux CMake uses the system OpenSSL development
package and links static OpenSSL archives for the standalone binary. A C++20
compiler and CMake 3.24+ are required.
If the full Perl distribution is not on `PATH`, pass its executable with
`-DYUKINAL_PERL_EXECUTABLE=/path/to/perl.exe` or set the same environment
variable when running `pnpm build:ssh-service`. The build script also accepts
`CMAKE_EXECUTABLE` when multiple CMake installations are on `PATH`.
The 2026-10-15
Windows verification used the NMake generator and passed all six registered
CTest targets with the Agent and optional Paramiko test dependencies installed:
the C++ config/policy/network tests, Python client tests, service HTTP
integration, Python Agent tests, SSH mock integration, and Agent-to-SSH
integration. (2026-10-18: that list was superseded. The current suite registers
**13** targets and the Python client/service-integration modules are no longer
separate targets — they run inside `python_agent_tests`, whose glob now covers
every `agent/tests/test_*.py`. See `docs/acceptance-handoff.md` for the current
list.) Ubuntu CI additionally enables the real OpenSSH container test,
which checks host-key and authentication failures, bounded command execution,
`hardlink@openssh.com:1` publication, and collision-safe no-replace behavior.
The local Windows run did not execute that container test because Docker is not
installed in this environment.
The SSH mock also exercises HTTPS CONNECT through a locally trusted CA and
confirms that a certificate with a mismatched proxy hostname is rejected. It
verifies authenticated SOCKS5 and SOCKS5H tunnels, including local versus proxy
DNS resolution and rejection of an incorrect credential.

`pnpm build:ssh-service` builds only the C++ executable and stages it under
`apps/desktop/resources/ssh-service/`. `pnpm package` copies it into the staged
distributable (under `ssh-service/`) together with its OpenSSL licence text. The Python Agent can use an operator-managed config; the desktop
Agent and service panel also create a one-target config from the selected
server and keychain identity. Both start the C++ child with only the referenced
credential environment values. A `--parent-pid` monitor also makes the C++
service exit if its parent dies without a graceful shutdown.

The Python Agent and desktop service panel create a short-lived one-target
allowlist from the selected desktop server record and OS keychain identity. The
desktop file browser routes listing, metadata, and bounded reads through the
C++ service for supported identities. Agent file reads, writes, backup
creation, cleanup, guarded edits, and restores use the same service. Manual
transfers use bounded C++ SFTP streams, and interactive PTY sessions use the
dedicated C++ worker when the selected SSH configuration is supported.

## Run

Copy `configs/ssh-service.example.json`, set `YUKINAL_SSH_API_TOKEN` to a random
value of at least 32 characters, and fill in an allow-listed target. The target
must specify exactly one trust source (`known_hosts_file`,
`host_key_fingerprint`, or `host_certificate_authority`) and one authentication source (private-key file,
private-key environment reference, password environment reference, or
`{"type":"agent"}`). Then run:

```powershell
native/build/Release/yukinal-ssh-service.exe --config path/to/ssh-service.json
```

The service does not print request bodies or credentials. The Python client is
in `agent/`; see its README for a call example.

Host CA targets use this trust object instead of `known_hosts_file` or
`host_key_fingerprint`:

```json
{
  "ca_public_key": "ssh-ed25519 AAAA... optional-comment",
  "principals": ["*.example.test"],
  "revocation_list_path": "/etc/ssh/host-revoked.krl",
  "revocation_list_signers": ["ssh-ed25519 AAAA... signer-comment"]
}
```

Use either `revocation_list_path` or `revocation_list_url`. A KRL URL must use
HTTPS; plain HTTP is permitted only for a loopback URL. Every KRL signature must
verify, and at least one signer must match the host CA or a configured KRL signer.
