#pragma once

#include <nlohmann/json.hpp>
#include <cstddef>
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

// ── Host-owned filesystem backup paths ────────────────────────────────────────
// A model supplies only the source path; the desktop host supplies a random
// token and this module derives a single sibling destination that only a later
// restore of the same source can consume. Keeping derivation here keeps the
// credential-secret path blocklist in one place.
inline constexpr std::size_t kBackupTokenChars = 32;
inline constexpr std::size_t kMaxAgentBackupBytes = 1024 * 1024;
inline constexpr const char* kBackupMarker = ".yukinal-backup-";

[[nodiscard]] bool is_safe_backup_token(const std::string& token);
// Derives the only path where a source file may be backed up. Throws
// std::runtime_error when the path or token is not acceptable.
[[nodiscard]] std::string backup_path_for(const std::string& path, const std::string& token);
[[nodiscard]] bool is_backup_path_for(const std::string& path, const std::string& backup_path);
// A fresh 32-character lowercase-hex token. Never supplied by the model.
[[nodiscard]] std::string backup_token();
[[nodiscard]] std::string backup_record_id(const std::string& server_id,
                                           const std::string& backup_path);
// Mirrors the retired Rust `AgentBackup`/`AgentRestore`/`AgentCleanupBackup`
// request checks. Throws std::runtime_error (not a secret) on any violation.
void require_agent_backup_request(const std::string& path, const std::string& token);
void require_agent_restore_request(const std::string& path, const std::string& backup_path,
                                   const std::string& expected_revision);
void require_agent_cleanup_request(const std::string& path, const std::string& backup_path,
                                   const std::string& expected_revision);

}  // namespace yukinal::desktop_host
