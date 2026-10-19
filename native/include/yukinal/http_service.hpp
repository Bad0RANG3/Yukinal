#pragma once

#include <functional>
#include <cstdint>
#include <optional>
#include <string>

#include "yukinal/config.hpp"
#include "yukinal/policy.hpp"
#include "yukinal/ssh.hpp"

namespace yukinal::ssh_service {

using OperationRunner = std::function<ExecResult(const TargetConfig&, const ExecRequest&)>;

/// HTTP status code the `/v1/ssh/exec` endpoint reports for a failed `ExecResult`.
/// Exposed (rather than kept in the service's anonymous namespace) so the mapping
/// -- including the fail-closed `denied_by_policy` -> 403 rule -- is unit-testable.
[[nodiscard]] int error_status_for(const ExecResult& result);

/// Start the loopback-only HTTP API. Returns false if the socket cannot be bound.
bool serve(const ServiceConfig& config, const std::string& api_token,
           OperationRunner runner = run_operation,
           std::optional<std::uint64_t> parent_pid = std::nullopt);

}  // namespace yukinal::ssh_service
