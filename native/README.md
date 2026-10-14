# SSH Service (C++)

The first rewrite milestone is a small, standalone C++ service. It binds only to
`127.0.0.1`, accepts structured requests, resolves `target_id` from an operator
configuration, verifies SSH host keys against OpenSSH `known_hosts`, and exposes
one read-only operation: `service_status`.

The service deliberately has no raw-command endpoint, UI bridge, Agent runtime,
MCP support, terminal, or file transfer. Those are later milestones.

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
resources, but starts only when `YUKINAL_SSH_SERVICE_CONFIG` points to an
operator-managed JSON file and the configured API token and credential
environment references are present. The Python Agent checks `/health` before
it becomes ready, then owns the service lifetime. It passes only the
environment variables named in that service config to the C++ child. A
`--parent-pid` monitor also makes the C++ service exit if the Python Agent dies
without a graceful shutdown.

The packaged service still uses a static operator allowlist. Automatic
synchronization from saved desktop server records and OS keychain identities is
not implemented yet.

## Run

Copy `configs/ssh-service.example.json`, set `YUKINAL_SSH_API_TOKEN` to a random
value of at least 32 characters, and fill in an allow-listed target. The target
must specify a known-hosts file and either a private-key path or a name for a
password environment variable. Then run:

```powershell
native/build/Release/yukinal-ssh-service.exe --config path/to/ssh-service.json
```

The service does not print request bodies or credentials. The Python client is
in `agent/`; see its README for a call example.
