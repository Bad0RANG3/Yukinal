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
development package. A C++20 compiler and CMake 3.24+ are required.
On Windows, run the commands from a Visual Studio 2022 Developer Command
Prompt/PowerShell so MSVC and the Windows SDK are available. Local verification
on 2026-10-15 used the NMake generator and passed all four CTest targets with
the optional Paramiko test dependency installed. The separate OpenSSH container
test still requires Docker.

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
