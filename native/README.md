# SSH Service (C++)

The rewrite uses a standalone C++ service. It binds only to `127.0.0.1`, accepts
structured requests, resolves `target_id` from a configured allowlist and
exposes service queries plus bounded SFTP list, stat, read, write,
exclusive-create, remove, and link-count operations.
Host keys
can be checked against an OpenSSH `known_hosts` file or an exact `SHA256:` pin
from the desktop trust store. Private keys can come from a file or a named
environment reference, which lets the desktop pass OS-keychain material without
writing it into the service configuration.

The service deliberately has no raw-command endpoint or interactive terminal.
SFTP writes are capped at 512 KiB; exclusive recovery-file creation is capped
at 1 MiB and removes a partial file if writing or syncing fails. The desktop
calls the one-path remove operation only after it checks backup ownership and
revision. The link-count operation runs a fixed, quoted `stat` probe. The
desktop routes supported Agent file reads/writes and backup/cleanup operations
through C++ for password/private-key identities without a host CA. Guarded
atomic edit/restore, transfers, PTY sessions, and other auth/trust modes still
use Rust while their C++ equivalents are migrated.

## Build

```powershell
cmake -S native -B native/build -DCMAKE_BUILD_TYPE=Release
cmake --build native/build --config Release
ctest --test-dir native/build -C Release --output-on-failure
```

CMake fetches pinned source releases of nlohmann/json, cpp-httplib, and libssh2.
On Windows libssh2 uses WinCNG; on macOS/Linux it uses the system OpenSSL
development package and links static OpenSSL archives for the standalone binary.
A C++20 compiler and CMake 3.24+ are required.
On Windows, run the commands from a Visual Studio 2022 Developer Command
Prompt/PowerShell so MSVC and the Windows SDK are available. The 2026-10-15
Windows verification used the NMake generator and passed all six registered
CTest targets with the Agent and optional Paramiko test dependencies installed:
the C++ config/policy/network tests, Python client tests, service HTTP
integration, Python Agent tests, SSH mock integration, and Agent-to-SSH
integration. The real OpenSSH container test is separate and requires Docker.

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
C++ service for supported password/private-key identities. Agent file reads,
writes, backup creation, and cleanup use the same service. Guarded atomic file
replacement, transfers, and interactive PTY sessions remain on the Rust
transport during migration.

## Run

Copy `configs/ssh-service.example.json`, set `YUKINAL_SSH_API_TOKEN` to a random
value of at least 32 characters, and fill in an allow-listed target. The target
must specify exactly one trust source (`known_hosts_file` or
`host_key_fingerprint`) and one authentication source (private-key file,
private-key environment reference, or password environment reference). Then run:

```powershell
native/build/Release/yukinal-ssh-service.exe --config path/to/ssh-service.json
```

The service does not print request bodies or credentials. The Python client is
in `agent/`; see its README for a call example.
