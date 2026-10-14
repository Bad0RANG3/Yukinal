#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "yukinal/config.hpp"
#include "yukinal/policy.hpp"

namespace yukinal::ssh_service {

struct ExecResult {
  bool completed = false;
  int exit_code = -1;
  std::string stdout_text;
  std::string stderr_text;
  std::int64_t duration_ms = 0;
  bool output_truncated = false;
  std::string error_code;
  std::string error_message;
  std::vector<std::string> missing_metadata;
};

/// Run a fixed structured SSH operation. No raw command is accepted here.
ExecResult service_status(const TargetConfig& target, const std::string& service,
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
ExecResult sftp_list(const TargetConfig& target, const std::string& path, int timeout_seconds);
ExecResult sftp_stat(const TargetConfig& target, const std::string& path, int timeout_seconds);
ExecResult sftp_lstat_optional(const TargetConfig& target, const std::string& path,
                               int timeout_seconds);
ExecResult sftp_create_dir(const TargetConfig& target, const std::string& path,
                           int timeout_seconds);
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
