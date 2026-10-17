#pragma once

#include <cstdint>
#include <nlohmann/json.hpp>
#include <optional>
#include <string>

namespace yukinal::desktop_host {

class DesktopDatabase;
class CredentialStore;

// The bounded outcome of one structured SSH operation. Transport failures keep a
// stable `error_code`; a completed command keeps its exit code and output.
struct SshCommandResult {
  bool completed = false;
  int exit_code = -1;
  std::string stdout_text;
  std::string stderr_text;
  std::string error_code;
  std::string error_message;
};

// Runs the read-only `server_logs` operation through the in-process C++ SSH core
// using the same resolved target and credentials as the Agent host RPC. Throws
// std::runtime_error when the target cannot be represented or this build has no
// SSH core. The caller applies the Python normalizer to the returned output.
[[nodiscard]] SshCommandResult run_server_logs(const DesktopDatabase& database,
                                               const CredentialStore& credentials,
                                               const std::string& server_id,
                                               std::optional<std::uint32_t> since_seconds,
                                               const std::string& unit,
                                               int timeout_seconds);

// Probes the configured host key without authenticating. The returned fingerprint
// is untrusted until the user confirms it.
[[nodiscard]] SshCommandResult run_host_key_probe(const DesktopDatabase& database,
                                                  const CredentialStore& credentials,
                                                  const std::string& server_id,
                                                  int timeout_seconds);

// Runs the read-only `service_inventory` operation. The caller applies the Python
// `server.services.normalize` RPC to the returned output.
[[nodiscard]] SshCommandResult run_service_inventory(const DesktopDatabase& database,
                                                     const CredentialStore& credentials,
                                                     const std::string& server_id,
                                                     int timeout_seconds);

// The remaining bounded read-only Agent tool operations. They use the same
// resolved target and credentials as the Python-driven C++ SSH service; the
// caller applies the matching Python `*.normalize` RPC to the raw output.
[[nodiscard]] SshCommandResult run_docker_ps(const DesktopDatabase& database,
                                             const CredentialStore& credentials,
                                             const std::string& server_id, bool all,
                                             int timeout_seconds);
[[nodiscard]] SshCommandResult run_docker_logs(const DesktopDatabase& database,
                                               const CredentialStore& credentials,
                                               const std::string& server_id,
                                               const std::string& container,
                                               std::uint32_t tail, int timeout_seconds);
[[nodiscard]] SshCommandResult run_docker_inspect(const DesktopDatabase& database,
                                                  const CredentialStore& credentials,
                                                  const std::string& server_id,
                                                  const std::string& container,
                                                  int timeout_seconds);
[[nodiscard]] SshCommandResult run_systemd_inspect(const DesktopDatabase& database,
                                                   const CredentialStore& credentials,
                                                   const std::string& server_id,
                                                   const std::string& service,
                                                   int timeout_seconds);
[[nodiscard]] SshCommandResult run_package_inspect(const DesktopDatabase& database,
                                                   const CredentialStore& credentials,
                                                   const std::string& server_id,
                                                   const std::string& manager,
                                                   const std::string& package,
                                                   int timeout_seconds);
// Reads one remote file for the Agent `filesystem.read` tool: the returned JSON is
// `{path, content, truncated, revision}` where `revision` is the SHA-256 of the
// kept raw bytes. Throws on transport or policy failure.
[[nodiscard]] nlohmann::json run_agent_filesystem_read(const DesktopDatabase& database,
                                                       const CredentialStore& credentials,
                                                       const std::string& server_id,
                                                       const std::string& path,
                                                       std::uint32_t max_bytes,
                                                       int timeout_seconds);

// Validates SSH authentication and host trust for the resolved target.
[[nodiscard]] SshCommandResult run_connection_check(const DesktopDatabase& database,
                                                    const CredentialStore& credentials,
                                                    const std::string& server_id,
                                                    int timeout_seconds);

// Runs the fixed `system_snapshot` command batch with a fresh nonce and parses the
// nonce-delimited output into `{command_id: {exitCode, stdout}}`. The caller applies
// the Python `server.snapshot.normalize` RPC to that map.
[[nodiscard]] nlohmann::json run_system_snapshot(const DesktopDatabase& database,
                                                 const CredentialStore& credentials,
                                                 const std::string& server_id,
                                                 int timeout_seconds);

// UI file browser: one bounded SFTP directory listing, one bounded read, and one
// metadata-plus-read preview. The browser path policy is deliberately not the Agent
// credential blocklist; it only normalizes entry names into absolute paths.
[[nodiscard]] nlohmann::json run_remote_file_list(const DesktopDatabase& database,
                                                  const CredentialStore& credentials,
                                                  const std::string& server_id,
                                                  const std::string& path,
                                                  int timeout_seconds);
[[nodiscard]] nlohmann::json run_remote_file_read(const DesktopDatabase& database,
                                                  const CredentialStore& credentials,
                                                  const std::string& server_id,
                                                  const std::string& path,
                                                  int timeout_seconds);
[[nodiscard]] nlohmann::json run_remote_file_preview(const DesktopDatabase& database,
                                                     const CredentialStore& credentials,
                                                     const std::string& server_id,
                                                     const std::string& path,
                                                     int timeout_seconds);

}  // namespace yukinal::desktop_host
