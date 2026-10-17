#pragma once

#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "yukinal/desktop_host/transfer_types.hpp"

namespace yukinal::desktop_host {

// The remote capability seam the transfer manager drives. It is a port of the
// retired Rust `RemoteTransferBackend` trait: a minimal streaming SFTP surface.
// Implementations must use streaming file handles, `lstat` must not follow
// symbolic links, and neither publication mode may be emulated by copying bytes
// into the destination. Every method reports failure by throwing `TransferError`.

enum class RemoteEntryKind { File, Directory, Symlink, Other };

// One path's attributes with `lstat` semantics (a symlink is not followed).
struct RemoteStat {
  RemoteEntryKind kind = RemoteEntryKind::File;
  std::uint64_t size = 0;
  // Remote mtime in Unix seconds. SFTP's granularity is one second, so the
  // overwrite guard is only as precise as that.
  std::optional<std::uint32_t> modified;

  bool operator==(const RemoteStat& other) const {
    return kind == other.kind && size == other.size && modified == other.modified;
  }
};

struct RemoteTransferEntry {
  std::string name;
  RemoteStat stat;
};

// An owned streaming read handle for a remote file.
class TransferReader {
 public:
  virtual ~TransferReader() = default;
  // Reads up to `size` bytes into `data`; returns the count, 0 at end of file.
  virtual std::size_t read(char* data, std::size_t size) = 0;
};

// An owned streaming writer for an exclusively created remote staging file.
// `finish` flushes and closes the handle.
class TransferWriter {
 public:
  virtual ~TransferWriter() = default;
  virtual void write(const char* data, std::size_t size) = 0;
  virtual void finish() = 0;
};

class TransferBackend {
 public:
  virtual ~TransferBackend() = default;

  // Whether the remote supports an explicit atomic overwrite rename. Ordinary
  // SFTP v3 rename does not, so the default is fail-closed.
  [[nodiscard]] virtual bool supports_atomic_replace() const { return false; }

  virtual std::optional<RemoteStat> lstat(const std::string& server_id,
                                          const std::string& path) = 0;
  virtual std::vector<RemoteTransferEntry> list_dir(const std::string& server_id,
                                                    const std::string& path) = 0;
  virtual std::unique_ptr<TransferReader> open_read(const std::string& server_id,
                                                    const std::string& path) = 0;
  virtual std::unique_ptr<TransferWriter> create_exclusive(const std::string& server_id,
                                                           const std::string& path) = 0;
  virtual void create_dir(const std::string& server_id, const std::string& path) = 0;
  virtual void remove_file(const std::string& server_id, const std::string& path) = 0;
  // Atomically creates `destination` only when absent, leaving `source` in place.
  virtual void publish_no_replace(const std::string& server_id, const std::string& source,
                                  const std::string& destination) = 0;
  // Same-directory atomic rename of a regular file over `destination`.
  virtual void rename_replace(const std::string& server_id, const std::string& source,
                              const std::string& destination) = 0;
  // Removes one host-recorded remote staging file only when the backend can prove
  // it is a regular file owned by this transfer. The default fails closed: the SFTP
  // adapter must refuse without an already-authenticated session.
  virtual void remove_recovered_staging(const std::string& server_id,
                                        const std::string& path) {
    (void)server_id;
    (void)path;
    throw TransferError(TransferErrorCode::Unsupported,
                        "the backend cannot safely reconcile remote staging files");
  }
};

// Persistence seam for bounded transfer snapshots and the host-private staging
// inventory. A port of the retired Rust `TransferHistoryStore` trait. Every method
// reports failure by throwing `std::runtime_error`; staging data must never appear
// in an IPC-facing snapshot.
struct RecoveredTransfer;

class TransferHistoryStore {
 public:
  virtual ~TransferHistoryStore() = default;

  virtual void insert(const TransferSnapshot& snapshot,
                      const std::vector<TransferStagingFile>& staging) = 0;
  virtual void update(const TransferSnapshot& snapshot,
                      const std::vector<TransferStagingFile>& staging) = 0;
  virtual std::optional<TransferSnapshot> get(const std::string& transfer_id) = 0;
  virtual std::vector<TransferSnapshot> list(const std::optional<std::string>& server_id,
                                             std::size_t limit) = 0;
  // Atomically transitions every non-terminal stored row to `interrupted` and
  // returns its host-private staging inventory for safe cleanup attempts.
  virtual std::vector<RecoveredTransfer> recover(std::uint64_t now_epoch_ms) = 0;
  virtual std::vector<TransferStagingFile> staging(const std::string& transfer_id) = 0;
};

struct RecoveredTransfer {
  TransferSnapshot snapshot;
  std::vector<TransferStagingFile> staging;
};

}  // namespace yukinal::desktop_host
