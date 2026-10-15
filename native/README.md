# SSH Service (C++)

The rewrite uses a standalone C++ service. It binds only to `127.0.0.1`, accepts
structured requests, resolves `target_id` from a configured allowlist and
exposes service queries, probe-only host-key discovery, and bounded SFTP list, stat, read, write,
exclusive-create, remove, guarded atomic replacement, and link-count operations.
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

The HTTP service deliberately has no raw-command endpoint. Its separate `--terminal`
worker mode is used only by the manually opened desktop terminal; it is not an
Agent operation and accepts no HTTP requests. Supported desktop terminal
sessions and manual SFTP transfers also use C++ workers.
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
keyboard-interactive challenges through the existing desktop prompt UI. Rust
remains the compatibility path for other SSH configurations the C++ service
cannot yet represent. When pin or `known_hosts` trust is used with a host
certificate, C++ fingerprints the certified public key, matching the desktop
trust store.

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
integration. The real OpenSSH container test is separate and requires Docker.
The SSH mock also exercises HTTPS CONNECT through a locally trusted CA and
confirms that a certificate with a mismatched proxy hostname is rejected. It
verifies authenticated SOCKS5 and SOCKS5H tunnels, including local versus proxy
DNS resolution and rejection of an incorrect credential.

`pnpm build:ssh-service` builds only the C++ executable and stages it under
`apps/desktop/src-tauri/resources/ssh-service/`; `pnpm package` and the direct
Tauri build hook do this automatically. The executable is included in the app
resources. The Python Agent can use an operator-managed config; the desktop
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
