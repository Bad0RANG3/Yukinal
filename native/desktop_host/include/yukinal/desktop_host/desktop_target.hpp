#pragma once

#include <nlohmann/json.hpp>
#include <string>

namespace yukinal::desktop_host {

class DesktopDatabase;
class CredentialStore;

// Builds the one-target `{target, credentials}` payload that the Python Agent
// Node hands to the C++ SSH service. The target carries only metadata and
// environment-variable names; the secret values live in the `credentials` map
// and never touch the JSON configuration or any log. Throws std::runtime_error
// with a stable, non-secret message when the server cannot be represented.
[[nodiscard]] nlohmann::json resolve_ssh_service_target(const DesktopDatabase& database,
                                                        const CredentialStore& credentials,
                                                        const std::string& server_id);

// Probe-only variant: no SSH identity and no configured trust source. It is used
// for the host-key confirmation flow and is never persisted by this resolver.
[[nodiscard]] nlohmann::json resolve_ssh_service_probe_target(
    const DesktopDatabase& database, const CredentialStore& credentials,
    const std::string& server_id);

// Filesystem targets extend the base target with the host-validated operation
// metadata. The path policy (absolute path, bounded length, credential-secret
// blocklist) is enforced here, before the Python Agent starts the C++ service.
[[nodiscard]] nlohmann::json resolve_ssh_service_filesystem_read_target(
    const DesktopDatabase& database, const CredentialStore& credentials,
    const nlohmann::json& request);
[[nodiscard]] nlohmann::json resolve_ssh_service_filesystem_write_target(
    const DesktopDatabase& database, const CredentialStore& credentials,
    const nlohmann::json& request);
[[nodiscard]] nlohmann::json resolve_ssh_service_filesystem_edit_target(
    const DesktopDatabase& database, const CredentialStore& credentials,
    const nlohmann::json& request);

}  // namespace yukinal::desktop_host
