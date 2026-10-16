#pragma once

#include <cstdint>
#include <nlohmann/json.hpp>
#include <optional>
#include <stdexcept>
#include <string>
#include <vector>

namespace yukinal::desktop_host {

// A typed port of the retired Rust `crate::filesystem::transfer::types` module:
// the shared `TransferSnapshot` contract plus the pure helpers the transfer
// manager and its SFTP adapter both need. Nothing here touches the database or
// the network, so every function is directly unit-testable.
//
// `TransferSnapshot` is serialized to the exact 19-field camelCase contract in
// `packages/shared/src/schemas/file.ts`. The host-private staging inventory is a
// separate structure and must never appear in an IPC snapshot.

enum class TransferDirection { Upload, Download };
enum class TransferStatus {
  Queued,
  Running,
  WaitingConflict,
  Completed,
  Partial,
  Failed,
  Cancelled,
  Interrupted,
};
enum class ConflictPolicy { Ask, Skip, Overwrite };
enum class ConflictActionKind { Skip, Overwrite, Rename };
enum class TransferFailureKind { InvalidInput, LocalIo, RemoteIo, Verification, Unsupported };

std::string to_string(TransferDirection value);
std::string to_string(TransferStatus value);
std::string to_string(ConflictPolicy value);
std::string to_string(ConflictActionKind value);
std::string to_string(TransferFailureKind value);

std::optional<TransferDirection> parse_transfer_direction(const std::string& value);
std::optional<TransferStatus> parse_transfer_status(const std::string& value);
std::optional<ConflictPolicy> parse_conflict_policy(const std::string& value);
std::optional<ConflictActionKind> parse_conflict_action_kind(const std::string& value);
std::optional<TransferFailureKind> parse_transfer_failure_kind(const std::string& value);

bool is_terminal(TransferStatus status);

// The one error type the transfer subsystem throws, mirroring the retired Rust
// `TransferError`. `failure_kind()` maps it onto the failure recorded in a
// snapshot. `Local`/`SourceChanged` are local I/O, `Remote`/`AlreadyExists` are
// remote I/O and cancellation is reported as remote I/O, matching Rust.
enum class TransferErrorCode {
  InvalidInput,
  Local,
  Remote,
  AlreadyExists,
  Unsupported,
  SourceChanged,
  Verification,
  Cancelled,
};

class TransferError : public std::runtime_error {
 public:
  TransferError(TransferErrorCode code, const std::string& message);
  [[nodiscard]] TransferErrorCode code() const noexcept { return code_; }
  [[nodiscard]] TransferFailureKind failure_kind() const noexcept;

 private:
  TransferErrorCode code_;
};

struct ConflictRequest {
  std::uint64_t item_index = 0;
  std::string source_name;
  std::string target_name;
  std::uint64_t existing_size = 0;
  std::uint64_t incoming_size = 0;
  std::optional<std::uint64_t> existing_modified_epoch_seconds;
  std::vector<ConflictActionKind> allowed_actions;
};

struct TransferItemFailure {
  std::optional<std::uint64_t> item_index;
  std::string item;
  TransferFailureKind kind = TransferFailureKind::InvalidInput;
  std::string message;
  std::optional<std::string> staging_residue;
};

// One resolved conflict decision. `rename_to` is set only for `Rename`.
struct ConflictAction {
  ConflictActionKind kind = ConflictActionKind::Skip;
  std::string rename_to;
};

struct TransferSnapshot {
  std::string transfer_id;
  std::string server_id;
  TransferDirection direction = TransferDirection::Upload;
  TransferStatus status = TransferStatus::Queued;
  std::uint64_t started_at_epoch_ms = 0;
  std::uint64_t updated_at_epoch_ms = 0;
  std::optional<std::uint64_t> total_files;
  std::uint64_t completed_files = 0;
  std::uint64_t skipped_files = 0;
  std::optional<std::uint64_t> total_bytes;
  std::uint64_t transferred_bytes = 0;
  std::optional<std::string> current_item;
  std::uint64_t current_item_bytes = 0;
  std::optional<std::uint64_t> current_item_total_bytes;
  std::optional<ConflictRequest> active_conflict;
  std::uint64_t verified_files = 0;
  std::uint64_t unverified_files = 0;
  std::vector<TransferItemFailure> failures;
  std::vector<std::string> staging_residue;
};

// Host-only ownership marker for one staging file. `remote` selects the
// `location:"remote"` variant (which also carries `serverId`); otherwise it is
// the `location:"local"` variant. Local paths are private to the host.
struct TransferStagingFile {
  bool remote = false;
  std::string path;
  std::string server_id;
  std::uint64_t item_index = 0;
};

// Exact IPC serialization of the shared snapshot contract.
nlohmann::json transfer_snapshot_json(const TransferSnapshot& snapshot);
nlohmann::json conflict_request_json(const ConflictRequest& request);
nlohmann::json transfer_item_failure_json(const TransferItemFailure& failure);

// The host-private staging inventory, round-trippable.
nlohmann::json transfer_staging_file_json(const TransferStagingFile& staging);
std::optional<TransferStagingFile> transfer_staging_file_from_json(
    const nlohmann::json& value);
std::vector<nlohmann::json> transfer_staging_json(
    const std::vector<TransferStagingFile>& staging);
std::optional<std::vector<TransferStagingFile>> transfer_staging_from_json(
    const nlohmann::json& value);

// Parses one `file_transfer_resolve_conflict` action. Any contract violation
// throws `std::invalid_argument`.
ConflictAction conflict_action_from_json(const nlohmann::json& value);

// A fresh queued snapshot with `startedAtEpochMs == updatedAtEpochMs == now`.
TransferSnapshot new_transfer_snapshot(const std::string& transfer_id,
                                       const std::string& server_id,
                                       TransferDirection direction);

std::uint64_t epoch_millis();
std::string new_transfer_id();

// Rejects a file name that cannot be transferred safely (empty, `.`/`..`,
// separators or control characters, a Windows reserved device name, or a
// trailing space/dot). Throws `std::invalid_argument`.
void validate_safe_name(const std::string& name);

// Rejects a remote path that is not an absolute, bounded, traversal-free POSIX
// path. Throws `std::invalid_argument`.
void validate_transfer_remote_path(const std::string& path);

std::string remote_child(const std::string& parent, const std::string& name);

// Whether `name` is a host-owned staging basename for `transfer_id`
// (`<target>.yukinal-<owner>-<item>-<attempt>.part`). Used before unlinking a
// path read back from the local database.
bool is_owned_staging_name(const std::string& name, const std::string& transfer_id);

}  // namespace yukinal::desktop_host
