#include "yukinal/desktop_host/transfer_history.hpp"

#include <limits>
#include <stdexcept>
#include <utility>

#include "yukinal/desktop_host/database.hpp"

namespace yukinal::desktop_host {
namespace {

// The host-private staging inventory crosses the repository as a JSON array, the
// same shape `DesktopDatabase` validates and persists.
nlohmann::json staging_array(const std::vector<TransferStagingFile>& staging) {
  nlohmann::json array = nlohmann::json::array();
  for (const auto& entry : staging) array.push_back(transfer_staging_file_json(entry));
  return array;
}

std::vector<TransferStagingFile> decode_staging(const nlohmann::json& value) {
  auto parsed = transfer_staging_from_json(value);
  if (!parsed.has_value()) {
    throw std::runtime_error("a stored file transfer staging inventory is invalid");
  }
  return std::move(*parsed);
}

TransferSnapshot decode_snapshot(const nlohmann::json& value) {
  return transfer_snapshot_from_json(value);
}

std::int64_t checked_epoch_ms(std::uint64_t value) {
  if (value > static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max())) {
    throw std::runtime_error("the file transfer recovery timestamp is too large");
  }
  return static_cast<std::int64_t>(value);
}

}  // namespace

DatabaseTransferHistoryStore::DatabaseTransferHistoryStore(DesktopDatabase* database)
    : database_(database) {
  if (database_ == nullptr) {
    throw std::invalid_argument("the transfer history store requires a database");
  }
}

void DatabaseTransferHistoryStore::insert(const TransferSnapshot& snapshot,
                                          const std::vector<TransferStagingFile>& staging) {
  database_->insert_file_transfer(transfer_snapshot_json(snapshot), staging_array(staging));
}

void DatabaseTransferHistoryStore::update(const TransferSnapshot& snapshot,
                                          const std::vector<TransferStagingFile>& staging) {
  database_->update_file_transfer(transfer_snapshot_json(snapshot), staging_array(staging));
}

std::optional<TransferSnapshot> DatabaseTransferHistoryStore::get(
    const std::string& transfer_id) {
  const auto snapshot = database_->get_file_transfer(transfer_id);
  if (!snapshot.has_value()) return std::nullopt;
  return decode_snapshot(*snapshot);
}

std::vector<TransferSnapshot> DatabaseTransferHistoryStore::list(
    const std::optional<std::string>& server_id, std::size_t limit) {
  if (limit == 0 || limit > 100) {
    throw std::invalid_argument("the file transfer history limit must be between 1 and 100");
  }
  const auto listed =
      database_->list_file_transfers(server_id, static_cast<int>(limit)).at("transfers");
  std::vector<TransferSnapshot> snapshots;
  snapshots.reserve(listed.size());
  for (const auto& entry : listed) snapshots.push_back(decode_snapshot(entry));
  return snapshots;
}

std::vector<RecoveredTransfer> DatabaseTransferHistoryStore::recover(
    std::uint64_t now_epoch_ms) {
  const auto recovered =
      database_->recover_file_transfers(checked_epoch_ms(now_epoch_ms)).at("records");
  std::vector<RecoveredTransfer> records;
  records.reserve(recovered.size());
  for (const auto& entry : recovered) {
    RecoveredTransfer record;
    record.snapshot = decode_snapshot(entry.at("snapshot"));
    record.staging = decode_staging(entry.at("staging"));
    records.push_back(std::move(record));
  }
  return records;
}

std::vector<TransferStagingFile> DatabaseTransferHistoryStore::staging(
    const std::string& transfer_id) {
  const auto staging = database_->file_transfer_staging(transfer_id);
  if (!staging.has_value()) return {};
  return decode_staging(*staging);
}

}  // namespace yukinal::desktop_host
