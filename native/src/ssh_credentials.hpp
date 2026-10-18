#pragma once

#include <string>

namespace yukinal::ssh_service::detail {

// All five authentication/proxy read sites use this resolver. An active host
// scope is a complete replacement; only CLI calls without a scope read getenv.
std::string read_ssh_credential(const std::string& name);

}  // namespace yukinal::ssh_service::detail
