#pragma once

#include <cstdint>
#include <string>

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
};

/// Run a fixed structured SSH operation. No raw command is accepted here.
ExecResult service_status(const TargetConfig& target, const std::string& service,
                          int timeout_seconds);
ExecResult service_inventory(const TargetConfig& target, int timeout_seconds);
ExecResult sftp_list(const TargetConfig& target, const std::string& path, int timeout_seconds);
ExecResult run_operation(const TargetConfig& target, const ExecRequest& request);

}  // namespace yukinal::ssh_service
