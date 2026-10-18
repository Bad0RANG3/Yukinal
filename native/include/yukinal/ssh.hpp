#pragma once

#include <cstdint>
#include <string>
#include <utility>
#include <vector>

#include "yukinal/config.hpp"
#include "yukinal/policy.hpp"

namespace yukinal::ssh_service {

// Thread-affine lexical scope for one host operation. Does not modify the
// process environment or pass secrets to spawned processes. Nested scopes fully
// replace credentials and restore the previous scope on exit; an empty scope
// blocks CLI environment fallback. Calls without any scope retain CLI behavior.
class ScopedCredentialOverride {
 public:
  using Values = std::vector<std::pair<std::string, std::string>>;
  explicit ScopedCredentialOverride(Values values);
  ~ScopedCredentialOverride();
  ScopedCredentialOverride(const ScopedCredentialOverride&) = delete;
  ScopedCredentialOverride& operator=(const ScopedCredentialOverride&) = delete;
  ScopedCredentialOverride(ScopedCredentialOverride&&) = delete;
  ScopedCredentialOverride& operator=(ScopedCredentialOverride&&) = delete;
 private:
  Values values_;
  const Values* previous_ = nullptr;
};

struct ExecResult {
  bool completed = false;
  int exit_code = -1;
  std::string stdout_text;
  std::string stderr_text;
  std::int64_t duration_ms = 0;
  bool output_truncated = false;
  bool stdout_truncated = false;
  bool stderr_truncated = false;
  std::string error_code;
  std::string error_message;
  std::vector<std::string> missing_metadata;
};

/// Run an allowlisted SSH operation. Raw server.exec is denied unless the
/// target's one-shot host-authorization capability is explicitly enabled.
ExecResult service_status(const TargetConfig& target, const std::string& service,
                          int timeout_seconds);
ExecResult disk_usage(const TargetConfig& target, int timeout_seconds);
ExecResult connection_check(const TargetConfig& target, int timeout_seconds);
ExecResult system_snapshot(const TargetConfig& target, const std::string& nonce,
                           int timeout_seconds);
ExecResult docker_ps(const TargetConfig& target, bool include_all_containers,
                     int timeout_seconds);
ExecResult docker_logs(const TargetConfig& target, const std::string& container,
                       std::uint32_t tail, int timeout_seconds);
ExecResult docker_inspect(const TargetConfig& target, const std::string& container,
                          int timeout_seconds);
ExecResult systemd_inspect(const TargetConfig& target, const std::string& unit,
                           int timeout_seconds);
ExecResult package_inspect(const TargetConfig& target, const std::string& manager,
                           const std::string& package, int timeout_seconds);
ExecResult docker_restart(const TargetConfig& target, const std::string& container,
                          std::uint32_t grace_period_seconds, int timeout_seconds);
ExecResult systemd_restart(const TargetConfig& target, const std::string& unit,
                           int timeout_seconds);
ExecResult package_install(const TargetConfig& target, const std::string& manager,
                           const std::string& package, const std::string& version,
                           int timeout_seconds);
ExecResult service_inventory(const TargetConfig& target, int timeout_seconds);
ExecResult server_logs(const TargetConfig& target,
                       std::optional<std::uint32_t> since_seconds,
                       const std::string& unit, int timeout_seconds);
ExecResult host_key_probe(const TargetConfig& target, int timeout_seconds);
/// Run an interactive PTY over stdin/stdout using the terminal JSON-lines protocol.
int run_terminal_session(const TargetConfig& target, std::uint16_t columns,
                         std::uint16_t rows);
/// Stream one SFTP download or exclusive staging upload over JSON-lines stdin/stdout.
int run_sftp_stream(const TargetConfig& target, const std::string& path, bool upload);
// Metadata operations include a fresh SSH connection. timeout_seconds is an
// overall limit (1..30), with bounded connection/authentication/SFTP phases.
inline constexpr int kSftpMetadataTimeoutSeconds = 30;
ExecResult sftp_list(const TargetConfig& target, const std::string& path, int timeout_seconds);
ExecResult sftp_stat(const TargetConfig& target, const std::string& path, int timeout_seconds);
ExecResult sftp_lstat_optional(const TargetConfig& target, const std::string& path,
                               int timeout_seconds);
ExecResult sftp_create_dir(const TargetConfig& target, const std::string& path,
                           int timeout_seconds);
ExecResult sftp_publish_no_replace(const TargetConfig& target, const std::string& source,
                                   const std::string& destination, int timeout_seconds);
ExecResult sftp_read(const TargetConfig& target, const std::string& path,
                     std::uint32_t max_bytes, int timeout_seconds);
ExecResult sftp_write(const TargetConfig& target, const std::string& path,
                      const std::vector<unsigned char>& content, int timeout_seconds);
ExecResult sftp_create_exclusive(const TargetConfig& target, const std::string& path,
                                 const std::vector<unsigned char>& content, int timeout_seconds);
ExecResult sftp_remove_file(const TargetConfig& target, const std::string& path,
                            int timeout_seconds);
ExecResult sftp_replace_guarded(const TargetConfig& target, const ExecRequest& request);
ExecResult sftp_link_count(const TargetConfig& target, const std::string& path,
                           int timeout_seconds);
ExecResult run_operation(const TargetConfig& target, const ExecRequest& request);

}  // namespace yukinal::ssh_service
