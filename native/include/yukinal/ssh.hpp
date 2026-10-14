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
ExecResult host_key_probe(const TargetConfig& target, int timeout_seconds);
ExecResult sftp_list(const TargetConfig& target, const std::string& path, int timeout_seconds);
ExecResult sftp_stat(const TargetConfig& target, const std::string& path, int timeout_seconds);
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
