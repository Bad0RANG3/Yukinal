#pragma once

#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "yukinal/desktop_host/transfer_backend.hpp"
#include "yukinal/desktop_host/transfer_types.hpp"

namespace yukinal::desktop_host {

struct TransferManagerImpl;

// Owns transfer task lifetimes and their latest queryable snapshots. It is a port
// of the retired Rust `TransferManager`: each transfer streams one file at a time
// with a fixed 64 KiB copy buffer, at most three run at once, and a full snapshot
// is published at most ten times a second per transfer while lifecycle changes
// publish immediately. The manager is copyable and shares one implementation; all
// blocking I/O happens on per-transfer worker threads, so `get`/`list`/`cancel`
// stay responsive. Admission is bounded before persistence and thread creation:
// at most three running operations plus sixteen waiting transfers.
class TransferManager {
 public:
  static constexpr std::size_t kMaxParallelTransfers = 3;
  static constexpr std::size_t kMaxPendingTransfers = 16;
  static constexpr std::size_t kCopyBufferBytes = 64 * 1024;

  // Starts one admitted job. A launcher must either take ownership and arrange
  // asynchronous execution, or throw before starting it. The default creates a
  // detached thread; this narrow seam also makes allocation failures testable.
  using WorkerLauncher = std::function<void(std::function<void()>)>;

  explicit TransferManager(std::shared_ptr<TransferBackend> backend);
  TransferManager(std::shared_ptr<TransferBackend> backend,
                  std::size_t max_parallel_transfers, WorkerLauncher launcher = {});

  // Create a manager backed by durable host history. Startup recovery is
  // synchronous and fail-closed: active rows become `interrupted` before the
  // manager can accept new work, then only validated local staging files are
  // removed. Remote staging records stay pending until a session is available;
  // startup never auto-connects. Throws `TransferError` on a recovery failure.
  [[nodiscard]] static TransferManager with_history(
      std::shared_ptr<TransferBackend> backend,
      std::shared_ptr<TransferHistoryStore> history, WorkerLauncher launcher = {});

  // Queue local files/directories for upload into one remote directory. Local
  // paths stay inside the host; the worker opens regular files with no-follow
  // semantics and streams them to an exclusive remote staging file. Returns the
  // new transfer id. Throws `TransferError` on invalid input.
  std::string start_upload(const std::string& server_id, const std::string& remote_dir,
                           std::vector<std::filesystem::path> local_paths,
                           ConflictPolicy policy);

  // Queue remote files/directories for download into an existing local directory.
  std::string start_download(const std::string& server_id, std::vector<std::string> remote_paths,
                             std::filesystem::path local_dir, ConflictPolicy policy);

  // Latest stable snapshot for one transfer, or `std::nullopt` when unknown. Can be
  // polled when a progress event was missed.
  [[nodiscard]] std::optional<TransferSnapshot> get(const std::string& transfer_id) const;

  // Merged recent snapshots (jobs plus history), sorted by update time desc, capped
  // at `limit` (1..100). Throws `TransferError` for an out-of-range limit.
  [[nodiscard]] std::vector<TransferSnapshot> list(const std::optional<std::string>& server_id,
                                                   std::size_t limit) const;

  // Request cancellation. The worker stops at the next stream operation and removes
  // its staging file; a failed cleanup is reported in `stagingResidue`. Throws
  // `TransferError` when the transfer is unknown.
  void cancel(const std::string& transfer_id);

  // Resolve the conflict currently shown in the snapshot. Stale item indexes are
  // rejected. Throws `TransferError` on a mismatch.
  void resolve_conflict(const std::string& transfer_id, std::uint64_t item_index,
                        ConflictAction action);

  // Retry remote startup-stage cleanup for servers that are already connected. The
  // backend deliberately refuses to open a new session here.
  void reconcile_remote_staging();

  // Stop admitting work, cancel active transfers, wait for their staging cleanup,
  // and mark unfinished snapshots interrupted.
  void shutdown();

  // Subscribe to published snapshots. Intermediate progress is coalesced while
  // lifecycle changes publish immediately; a lagging listener can recover the latest
  // state with `get`. Returns a token for `unsubscribe`.
  using SnapshotListener = std::function<void(const TransferSnapshot&)>;
  std::size_t subscribe(SnapshotListener listener);
  void unsubscribe(std::size_t subscription);

  // Test/observability hook: admitted starts, running jobs and waiting jobs.
  // Includes persistence in progress; never exceeds the configured limit + 16.
  [[nodiscard]] std::size_t active_worker_count() const;

 private:
  explicit TransferManager(std::shared_ptr<TransferManagerImpl> impl);

  std::shared_ptr<TransferManagerImpl> impl_;
};

}  // namespace yukinal::desktop_host
