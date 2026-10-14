#pragma once

#include <functional>
#include <cstdint>
#include <optional>
#include <string>

#include "yukinal/config.hpp"
#include "yukinal/policy.hpp"
#include "yukinal/ssh.hpp"

namespace yukinal::ssh_service {

using OperationRunner = std::function<ExecResult(const TargetConfig&, const std::string&, int)>;

/// Start the loopback-only HTTP API. Returns false if the socket cannot be bound.
bool serve(const ServiceConfig& config, const std::string& api_token,
           OperationRunner runner = service_status,
           std::optional<std::uint64_t> parent_pid = std::nullopt);

}  // namespace yukinal::ssh_service
