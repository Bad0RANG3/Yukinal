#include "yukinal/desktop_host/transfer_types.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cctype>
#include <sstream>
#include <stdexcept>

#ifdef _WIN32
#include <process.h>
#else
#include <unistd.h>
#endif

namespace yukinal::desktop_host {
namespace {

constexpr std::size_t kMaxRemotePathChars = 4096;
constexpr std::size_t kMaxRenameNameChars = 255;
constexpr const char* kStagingMarker = ".yukinal-";
constexpr const char* kStagingSuffix = ".part";

std::size_t utf8_char_length(const std::string& value) {
  return static_cast<std::size_t>(std::count_if(value.begin(), value.end(),
                                               [](unsigned char byte) {
                                                 return (byte & 0xC0) != 0x80;
                                               }));
}

nlohmann::json nullable_number(const std::optional<std::uint64_t>& value) {
  return value.has_value() ? nlohmann::json(*value) : nlohmann::json(nullptr);
}

nlohmann::json nullable_string(const std::optional<std::string>& value) {
  return value.has_value() ? nlohmann::json(*value) : nlohmann::json(nullptr);
}

bool is_digit_sequence(const std::string& value) {
  return !value.empty() && std::all_of(value.begin(), value.end(), [](unsigned char byte) {
    return byte >= '0' && byte <= '9';
  });
}

}  // namespace

std::string to_string(TransferDirection value) {
  switch (value) {
    case TransferDirection::Upload:
      return "upload";
    case TransferDirection::Download:
      return "download";
  }
  throw std::invalid_argument("unknown transfer direction");
}

std::string to_string(TransferStatus value) {
  switch (value) {
    case TransferStatus::Queued:
      return "queued";
    case TransferStatus::Running:
      return "running";
    case TransferStatus::WaitingConflict:
      return "waitingConflict";
    case TransferStatus::Completed:
      return "completed";
    case TransferStatus::Partial:
      return "partial";
    case TransferStatus::Failed:
      return "failed";
    case TransferStatus::Cancelled:
      return "cancelled";
    case TransferStatus::Interrupted:
      return "interrupted";
  }
  throw std::invalid_argument("unknown transfer status");
}

std::string to_string(ConflictPolicy value) {
  switch (value) {
    case ConflictPolicy::Ask:
      return "ask";
    case ConflictPolicy::Skip:
      return "skip";
    case ConflictPolicy::Overwrite:
      return "overwrite";
  }
  throw std::invalid_argument("unknown conflict policy");
}

std::string to_string(ConflictActionKind value) {
  switch (value) {
    case ConflictActionKind::Skip:
      return "skip";
    case ConflictActionKind::Overwrite:
      return "overwrite";
    case ConflictActionKind::Rename:
      return "rename";
  }
  throw std::invalid_argument("unknown conflict action");
}

std::string to_string(TransferFailureKind value) {
  switch (value) {
    case TransferFailureKind::InvalidInput:
      return "invalidInput";
    case TransferFailureKind::LocalIo:
      return "localIo";
    case TransferFailureKind::RemoteIo:
      return "remoteIo";
    case TransferFailureKind::Verification:
      return "verification";
    case TransferFailureKind::Unsupported:
      return "unsupported";
  }
  throw std::invalid_argument("unknown transfer failure kind");
}

std::optional<TransferDirection> parse_transfer_direction(const std::string& value) {
  if (value == "upload") return TransferDirection::Upload;
  if (value == "download") return TransferDirection::Download;
  return std::nullopt;
}

std::optional<TransferStatus> parse_transfer_status(const std::string& value) {
  if (value == "queued") return TransferStatus::Queued;
  if (value == "running") return TransferStatus::Running;
  if (value == "waitingConflict") return TransferStatus::WaitingConflict;
  if (value == "completed") return TransferStatus::Completed;
  if (value == "partial") return TransferStatus::Partial;
  if (value == "failed") return TransferStatus::Failed;
  if (value == "cancelled") return TransferStatus::Cancelled;
  if (value == "interrupted") return TransferStatus::Interrupted;
  return std::nullopt;
}

std::optional<ConflictPolicy> parse_conflict_policy(const std::string& value) {
  if (value == "ask") return ConflictPolicy::Ask;
  if (value == "skip") return ConflictPolicy::Skip;
  if (value == "overwrite") return ConflictPolicy::Overwrite;
  return std::nullopt;
}

std::optional<ConflictActionKind> parse_conflict_action_kind(const std::string& value) {
  if (value == "skip") return ConflictActionKind::Skip;
  if (value == "overwrite") return ConflictActionKind::Overwrite;
  if (value == "rename") return ConflictActionKind::Rename;
  return std::nullopt;
}

std::optional<TransferFailureKind> parse_transfer_failure_kind(const std::string& value) {
  if (value == "invalidInput") return TransferFailureKind::InvalidInput;
  if (value == "localIo") return TransferFailureKind::LocalIo;
  if (value == "remoteIo") return TransferFailureKind::RemoteIo;
  if (value == "verification") return TransferFailureKind::Verification;
  if (value == "unsupported") return TransferFailureKind::Unsupported;
  return std::nullopt;
}

bool is_terminal(TransferStatus status) {
  switch (status) {
    case TransferStatus::Completed:
    case TransferStatus::Partial:
    case TransferStatus::Failed:
    case TransferStatus::Cancelled:
    case TransferStatus::Interrupted:
      return true;
    case TransferStatus::Queued:
    case TransferStatus::Running:
    case TransferStatus::WaitingConflict:
      return false;
  }
  return true;
}

TransferError::TransferError(TransferErrorCode code, const std::string& message)
    : std::runtime_error(message), code_(code) {}

TransferFailureKind TransferError::failure_kind() const noexcept {
  switch (code_) {
    case TransferErrorCode::InvalidInput:
      return TransferFailureKind::InvalidInput;
    case TransferErrorCode::Local:
    case TransferErrorCode::SourceChanged:
      return TransferFailureKind::LocalIo;
    case TransferErrorCode::Remote:
    case TransferErrorCode::AlreadyExists:
    case TransferErrorCode::Cancelled:
      return TransferFailureKind::RemoteIo;
    case TransferErrorCode::Unsupported:
      return TransferFailureKind::Unsupported;
    case TransferErrorCode::Verification:
      return TransferFailureKind::Verification;
  }
  return TransferFailureKind::LocalIo;
}

nlohmann::json transfer_item_failure_json(const TransferItemFailure& failure) {
  nlohmann::json value = {
      {"item", failure.item},
      {"kind", to_string(failure.kind)},
      {"message", failure.message},
  };
  if (failure.item_index.has_value()) value["itemIndex"] = *failure.item_index;
  if (failure.staging_residue.has_value()) value["stagingResidue"] = *failure.staging_residue;
  return value;
}

nlohmann::json conflict_request_json(const ConflictRequest& request) {
  nlohmann::json actions = nlohmann::json::array();
  for (const auto kind : request.allowed_actions) actions.push_back(to_string(kind));
  return {
      {"itemIndex", request.item_index},
      {"sourceName", request.source_name},
      {"targetName", request.target_name},
      {"existingSize", request.existing_size},
      {"incomingSize", request.incoming_size},
      {"existingModifiedEpochSeconds", nullable_number(request.existing_modified_epoch_seconds)},
      {"allowedActions", std::move(actions)},
  };
}

nlohmann::json transfer_snapshot_json(const TransferSnapshot& snapshot) {
  nlohmann::json failures = nlohmann::json::array();
  for (const auto& failure : snapshot.failures) failures.push_back(transfer_item_failure_json(failure));
  nlohmann::json residue = nlohmann::json::array();
  for (const auto& entry : snapshot.staging_residue) residue.push_back(entry);
  nlohmann::json active_conflict = snapshot.active_conflict.has_value()
                                       ? conflict_request_json(*snapshot.active_conflict)
                                       : nlohmann::json(nullptr);
  return {
      {"transferId", snapshot.transfer_id},
      {"serverId", snapshot.server_id},
      {"direction", to_string(snapshot.direction)},
      {"status", to_string(snapshot.status)},
      {"startedAtEpochMs", snapshot.started_at_epoch_ms},
      {"updatedAtEpochMs", snapshot.updated_at_epoch_ms},
      {"totalFiles", nullable_number(snapshot.total_files)},
      {"completedFiles", snapshot.completed_files},
      {"skippedFiles", snapshot.skipped_files},
      {"totalBytes", nullable_number(snapshot.total_bytes)},
      {"transferredBytes", snapshot.transferred_bytes},
      {"currentItem", nullable_string(snapshot.current_item)},
      {"currentItemBytes", snapshot.current_item_bytes},
      {"currentItemTotalBytes", nullable_number(snapshot.current_item_total_bytes)},
      {"activeConflict", std::move(active_conflict)},
      {"verifiedFiles", snapshot.verified_files},
      {"unverifiedFiles", snapshot.unverified_files},
      {"failures", std::move(failures)},
      {"stagingResidue", std::move(residue)},
  };
}

nlohmann::json transfer_staging_file_json(const TransferStagingFile& staging) {
  nlohmann::json value = {
      {"location", staging.remote ? "remote" : "local"},
      {"path", staging.path},
      {"itemIndex", staging.item_index},
  };
  if (staging.remote) value["serverId"] = staging.server_id;
  return value;
}

std::optional<TransferStagingFile> transfer_staging_file_from_json(
    const nlohmann::json& value) {
  if (!value.is_object()) return std::nullopt;
  const auto location = value.find("location");
  if (location == value.end() || !location->is_string()) return std::nullopt;
  TransferStagingFile staging;
  if (*location == "local") {
    staging.remote = false;
  } else if (*location == "remote") {
    staging.remote = true;
  } else {
    return std::nullopt;
  }
  const auto path = value.find("path");
  if (path == value.end() || !path->is_string()) return std::nullopt;
  staging.path = path->get<std::string>();
  const auto item = value.find("itemIndex");
  if (item == value.end() || !(item->is_number_unsigned() || item->is_number_integer())) {
    return std::nullopt;
  }
  staging.item_index = item->get<std::uint64_t>();
  if (staging.remote) {
    const auto server = value.find("serverId");
    if (server == value.end() || !server->is_string()) return std::nullopt;
    staging.server_id = server->get<std::string>();
  }
  return staging;
}

std::vector<nlohmann::json> transfer_staging_json(
    const std::vector<TransferStagingFile>& staging) {
  std::vector<nlohmann::json> encoded;
  encoded.reserve(staging.size());
  for (const auto& entry : staging) encoded.push_back(transfer_staging_file_json(entry));
  return encoded;
}

std::optional<std::vector<TransferStagingFile>> transfer_staging_from_json(
    const nlohmann::json& value) {
  if (!value.is_array()) return std::nullopt;
  std::vector<TransferStagingFile> staging;
  staging.reserve(value.size());
  for (const auto& entry : value) {
    auto parsed = transfer_staging_file_from_json(entry);
    if (!parsed.has_value()) return std::nullopt;
    staging.push_back(std::move(*parsed));
  }
  return staging;
}

ConflictAction conflict_action_from_json(const nlohmann::json& value) {
  if (!value.is_object()) {
    throw std::invalid_argument("the transfer conflict action is invalid");
  }
  const auto action = value.find("action");
  if (action == value.end() || !action->is_string()) {
    throw std::invalid_argument("the transfer conflict action is invalid");
  }
  if (*action == "skip") return {ConflictActionKind::Skip, {}};
  if (*action == "overwrite") return {ConflictActionKind::Overwrite, {}};
  if (*action != "rename") {
    throw std::invalid_argument("the transfer conflict action is invalid");
  }
  const auto name = value.find("name");
  if (name == value.end() || !name->is_string()) {
    throw std::invalid_argument("the transfer conflict rename is invalid");
  }
  auto rename_to = name->get<std::string>();
  if (rename_to.size() > kMaxRenameNameChars) {
    throw std::invalid_argument("the transfer conflict rename is invalid");
  }
  validate_safe_name(rename_to);
  return {ConflictActionKind::Rename, std::move(rename_to)};
}

namespace {

[[noreturn]] void reject_snapshot() {
  throw std::runtime_error("the stored transfer snapshot is invalid");
}

const nlohmann::json& required_field(const nlohmann::json& value, const char* key) {
  const auto found = value.find(key);
  if (found == value.end()) reject_snapshot();
  return *found;
}

std::string required_string(const nlohmann::json& value, const char* key) {
  const auto& field = required_field(value, key);
  if (!field.is_string()) reject_snapshot();
  return field.get<std::string>();
}

std::uint64_t required_count(const nlohmann::json& value, const char* key) {
  const auto& field = required_field(value, key);
  if (field.is_number_unsigned()) return field.get<std::uint64_t>();
  if (field.is_number_integer()) {
    const auto number = field.get<std::int64_t>();
    if (number >= 0) return static_cast<std::uint64_t>(number);
  }
  reject_snapshot();
}

std::optional<std::uint64_t> nullable_count(const nlohmann::json& value, const char* key) {
  const auto found = value.find(key);
  if (found == value.end() || found->is_null()) return std::nullopt;
  return required_count(value, key);
}

std::optional<std::string> nullable_text(const nlohmann::json& value, const char* key) {
  const auto found = value.find(key);
  if (found == value.end() || found->is_null()) return std::nullopt;
  if (!found->is_string()) reject_snapshot();
  return found->get<std::string>();
}

ConflictRequest conflict_request_from_json(const nlohmann::json& value) {
  if (!value.is_object()) reject_snapshot();
  ConflictRequest request;
  request.item_index = required_count(value, "itemIndex");
  request.source_name = required_string(value, "sourceName");
  request.target_name = required_string(value, "targetName");
  request.existing_size = required_count(value, "existingSize");
  request.incoming_size = required_count(value, "incomingSize");
  request.existing_modified_epoch_seconds =
      nullable_count(value, "existingModifiedEpochSeconds");
  const auto& actions = required_field(value, "allowedActions");
  if (!actions.is_array()) reject_snapshot();
  for (const auto& action : actions) {
    if (!action.is_string()) reject_snapshot();
    const auto kind = parse_conflict_action_kind(action.get<std::string>());
    if (!kind.has_value()) reject_snapshot();
    request.allowed_actions.push_back(*kind);
  }
  return request;
}

TransferItemFailure transfer_item_failure_from_json(const nlohmann::json& value) {
  if (!value.is_object()) reject_snapshot();
  TransferItemFailure failure;
  failure.item_index = nullable_count(value, "itemIndex");
  failure.item = required_string(value, "item");
  const auto kind = parse_transfer_failure_kind(required_string(value, "kind"));
  if (!kind.has_value()) reject_snapshot();
  failure.kind = *kind;
  failure.message = required_string(value, "message");
  failure.staging_residue = nullable_text(value, "stagingResidue");
  return failure;
}

}  // namespace

TransferSnapshot transfer_snapshot_from_json(const nlohmann::json& value) {
  if (!value.is_object()) reject_snapshot();
  TransferSnapshot snapshot;
  snapshot.transfer_id = required_string(value, "transferId");
  snapshot.server_id = required_string(value, "serverId");
  const auto direction = parse_transfer_direction(required_string(value, "direction"));
  if (!direction.has_value()) reject_snapshot();
  snapshot.direction = *direction;
  const auto status = parse_transfer_status(required_string(value, "status"));
  if (!status.has_value()) reject_snapshot();
  snapshot.status = *status;
  snapshot.started_at_epoch_ms = required_count(value, "startedAtEpochMs");
  snapshot.updated_at_epoch_ms = required_count(value, "updatedAtEpochMs");
  snapshot.total_files = nullable_count(value, "totalFiles");
  snapshot.completed_files = required_count(value, "completedFiles");
  snapshot.skipped_files = required_count(value, "skippedFiles");
  snapshot.total_bytes = nullable_count(value, "totalBytes");
  snapshot.transferred_bytes = required_count(value, "transferredBytes");
  snapshot.current_item = nullable_text(value, "currentItem");
  snapshot.current_item_bytes = required_count(value, "currentItemBytes");
  snapshot.current_item_total_bytes = nullable_count(value, "currentItemTotalBytes");
  const auto conflict = value.find("activeConflict");
  if (conflict != value.end() && !conflict->is_null()) {
    snapshot.active_conflict = conflict_request_from_json(*conflict);
  }
  snapshot.verified_files = required_count(value, "verifiedFiles");
  snapshot.unverified_files = required_count(value, "unverifiedFiles");
  const auto& failures = required_field(value, "failures");
  if (!failures.is_array()) reject_snapshot();
  for (const auto& failure : failures) {
    snapshot.failures.push_back(transfer_item_failure_from_json(failure));
  }
  const auto& residue = required_field(value, "stagingResidue");
  if (!residue.is_array()) reject_snapshot();
  for (const auto& entry : residue) {
    if (!entry.is_string()) reject_snapshot();
    snapshot.staging_residue.push_back(entry.get<std::string>());
  }
  return snapshot;
}

TransferSnapshot new_transfer_snapshot(const std::string& transfer_id,
                                       const std::string& server_id,
                                       TransferDirection direction) {
  TransferSnapshot snapshot;
  snapshot.transfer_id = transfer_id;
  snapshot.server_id = server_id;
  snapshot.direction = direction;
  snapshot.status = TransferStatus::Queued;
  snapshot.started_at_epoch_ms = epoch_millis();
  snapshot.updated_at_epoch_ms = snapshot.started_at_epoch_ms;
  return snapshot;
}

std::uint64_t epoch_millis() {
  const auto now = std::chrono::system_clock::now().time_since_epoch();
  const auto millis = std::chrono::duration_cast<std::chrono::milliseconds>(now).count();
  return millis < 0 ? 0 : static_cast<std::uint64_t>(millis);
}

std::string new_transfer_id() {
  static std::atomic<std::uint64_t> sequence{1};
  const auto now = std::chrono::system_clock::now().time_since_epoch();
  const auto nanos = std::chrono::duration_cast<std::chrono::nanoseconds>(now).count();
#ifdef _WIN32
  const auto process = static_cast<unsigned long>(_getpid());
#else
  const auto process = static_cast<unsigned long>(::getpid());
#endif
  const auto current = sequence.fetch_add(1);
  std::ostringstream out;
  out << "tr_" << std::hex << process << '_' << nanos << '_' << std::dec << current;
  return out.str();
}

void validate_safe_name(const std::string& name) {
  const auto reject = []() -> void {
    throw std::invalid_argument("the file name cannot be transferred safely");
  };
  if (name.empty() || name == "." || name == "..") reject();
  for (const unsigned char byte : name) {
    if (byte < 0x20 || byte == 0x7f || byte == '/' || byte == '\\' || byte == ':') reject();
  }
  if (name.back() == ' ' || name.back() == '.') reject();
  // Reserved device names are rejected on every host: a transfer accepted on a
  // POSIX host can still target a Windows volume, and the shared contract must
  // not depend on which platform validated the name.
  const auto dot = name.find('.');
  auto stem = name.substr(0, dot == std::string::npos ? name.size() : dot);
  std::transform(stem.begin(), stem.end(), stem.begin(), [](unsigned char byte) {
    return static_cast<char>(std::toupper(byte));
  });
  static const char* const kReserved[] = {
      "CON",  "PRN",  "AUX",  "NUL",  "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
      "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7",
      "LPT8", "LPT9"};
  if (std::any_of(std::begin(kReserved), std::end(kReserved),
                  [&stem](const char* reserved) { return stem == reserved; })) {
    reject();
  }
}

void validate_transfer_remote_path(const std::string& path) {
  const auto reject = []() -> void {
    throw std::invalid_argument("the remote transfer path is invalid");
  };
  if (path.empty() || utf8_char_length(path) > kMaxRemotePathChars || path.front() != '/') {
    reject();
  }
  if (path.find('\0') != std::string::npos || path.find('\r') != std::string::npos ||
      path.find('\n') != std::string::npos) {
    reject();
  }
  if (path.find('\\') != std::string::npos || path.find("//") != std::string::npos) reject();
  std::size_t from = 0;
  for (;;) {
    const auto slash = path.find('/', from);
    const auto segment = path.substr(from, slash == std::string::npos ? std::string::npos
                                                                       : slash - from);
    if (segment == "." || segment == "..") reject();
    if (slash == std::string::npos) break;
    from = slash + 1;
  }
}

std::string remote_child(const std::string& parent, const std::string& name) {
  validate_safe_name(name);
  validate_transfer_remote_path(parent);
  std::string child;
  if (parent == "/") {
    child = "/" + name;
  } else if (parent.back() == '/') {
    child = parent + name;
  } else {
    child = parent + "/" + name;
  }
  validate_transfer_remote_path(child);
  return child;
}

bool is_owned_staging_name(const std::string& name, const std::string& transfer_id) {
  const std::string marker = kStagingMarker;
  const auto position = name.rfind(marker);
  if (position == std::string::npos || position == 0) return false;
  auto suffix = name.substr(position + marker.size());
  const std::string tail = kStagingSuffix;
  if (suffix.size() <= tail.size() ||
      suffix.compare(suffix.size() - tail.size(), tail.size(), tail) != 0) {
    return false;
  }
  suffix = suffix.substr(0, suffix.size() - tail.size());
  std::vector<std::string> parts;
  std::size_t from = 0;
  for (;;) {
    const auto dash = suffix.find('-', from);
    if (dash == std::string::npos) {
      parts.push_back(suffix.substr(from));
      break;
    }
    parts.push_back(suffix.substr(from, dash - from));
    from = dash + 1;
  }
  if (parts.size() != 3) return false;
  std::string owner = transfer_id;
  owner.erase(std::remove(owner.begin(), owner.end(), '-'), owner.end());
  return parts[0] == owner && is_digit_sequence(parts[1]) && is_digit_sequence(parts[2]);
}

}  // namespace yukinal::desktop_host
