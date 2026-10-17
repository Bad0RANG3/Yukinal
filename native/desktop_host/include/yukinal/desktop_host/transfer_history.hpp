#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "yukinal/desktop_host/transfer_backend.hpp"

namespace yukinal::desktop_host {

class DesktopDatabase;

// The durable `TransferHistoryStore` the host's transfer manager runs on top of
// `DesktopDatabase`. The snapshot is stored as its IPC contract JSON so the C++
// request path and the retired Rust host agree byte-for-byte, while the
// host-private staging inventory stays in a separate column. Every decode is
// strict: a stored row that no longer matches the snapshot schema throws
// `std::runtime_error` and is never silently dropped. The database outlives the
// store; the pointer is borrowed exactly like `IpcRouter::database_`.
class DatabaseTransferHistoryStore : public TransferHistoryStore {
 public:
  explicit DatabaseTransferHistoryStore(DesktopDatabase* database);

  void insert(const TransferSnapshot& snapshot,
              const std::vector<TransferStagingFile>& staging) override;
  void update(const TransferSnapshot& snapshot,
              const std::vector<TransferStagingFile>& staging) override;
  [[nodiscard]] std::optional<TransferSnapshot> get(const std::string& transfer_id) override;
  [[nodiscard]] std::vector<TransferSnapshot> list(
      const std::optional<std::string>& server_id, std::size_t limit) override;
  [[nodiscard]] std::vector<RecoveredTransfer> recover(std::uint64_t now_epoch_ms) override;
  [[nodiscard]] std::vector<TransferStagingFile> staging(
      const std::string& transfer_id) override;

 private:
  DesktopDatabase* database_;
};

}  // namespace yukinal::desktop_host
