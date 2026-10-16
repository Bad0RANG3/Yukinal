#include "yukinal/desktop_host/transfer_manager.hpp"

#include "yukinal/desktop_host/content_revision.hpp"
#include "yukinal/desktop_host/transfer_local.hpp"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <filesystem>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <system_error>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

namespace yukinal::desktop_host {
namespace {

// ── Small helpers ────────────────────────────────────────────────────────────

std::string trim(const std::string& value) {
  const auto first = value.find_first_not_of(" \t\r\n");
  if (first == std::string::npos) return {};
  const auto last = value.find_last_not_of(" \t\r\n");
  return value.substr(first, last - first + 1);
}

std::string path_to_utf8(const std::filesystem::path& path) {
  const auto value = path.u8string();
  return std::string(value.begin(), value.end());
}

std::filesystem::path path_from_utf8(const std::string& value) {
  const auto* begin = reinterpret_cast<const char8_t*>(value.data());
  return std::filesystem::path(std::u8string(begin, begin + value.size()));
}

// The final `/`-separated component, as Rust's `rsplit('/').next()`. An empty
// result means the path ended in a separator (or was empty).
std::string basename_of(const std::string& path) {
  const auto slash = path.rfind('/');
  return slash == std::string::npos ? path : path.substr(slash + 1);
}

// The parent of a validated remote path. `/` for a top-level child; the path
// without its trailing component otherwise.
std::string parent_of(const std::string& path) {
  const auto slash = path.rfind('/');
  if (slash == std::string::npos || slash == 0) return "/";
  return path.substr(0, slash);
}

void touch(TransferSnapshot& snapshot) { snapshot.updated_at_epoch_ms = epoch_millis(); }

// Truncate to at most `max_chars` Unicode scalar values, matching Rust's
// `chars().take(n)` rather than a byte slice that could split a code point.
std::string truncate_utf8(const std::string& value, std::size_t max_chars) {
  std::size_t chars = 0;
  std::size_t index = 0;
  while (index < value.size() && chars < max_chars) {
    const auto byte = static_cast<unsigned char>(value[index]);
    std::size_t length = 1;
    if (byte >= 0xF0) {
      length = 4;
    } else if (byte >= 0xE0) {
      length = 3;
    } else if (byte >= 0xC0) {
      length = 2;
    }
    index += std::min(length, value.size() - index);
    ++chars;
  }
  return value.substr(0, index);
}

// The pure contract helpers in `transfer_types` throw `std::invalid_argument`;
// the manager works in `TransferError`, so wrap them at this boundary.
std::string safe_name(const std::string& name) {
  try {
    validate_safe_name(name);
  } catch (const std::invalid_argument& error) {
    throw TransferError(TransferErrorCode::InvalidInput, error.what());
  }
  return name;
}

std::string remote_child_checked(const std::string& parent, const std::string& name) {
  try {
    return remote_child(parent, name);
  } catch (const std::invalid_argument& error) {
    throw TransferError(TransferErrorCode::InvalidInput, error.what());
  }
}

void validate_remote_path(const std::string& path) {
  try {
    validate_transfer_remote_path(path);
  } catch (const std::invalid_argument& error) {
    throw TransferError(TransferErrorCode::InvalidInput, error.what());
  }
}

bool remote_path_is_valid(const std::string& path) {
  try {
    validate_transfer_remote_path(path);
    return true;
  } catch (...) {
    return false;
  }
}

// ── A cancellable counting semaphore ─────────────────────────────────────────

class Semaphore {
 public:
  explicit Semaphore(std::size_t permits) : available_(permits) {}

  // Acquire a permit, or return false when `cancel` is set first. The wait is
  // polled so a queued transfer can observe cancellation without a second wakeup
  // path from `cancel`.
  bool acquire(const std::shared_ptr<std::atomic<bool>>& cancel) {
    std::unique_lock<std::mutex> lock(mutex_);
    for (;;) {
      if (available_ > 0) {
        --available_;
        return true;
      }
      if (cancel->load()) return false;
      cv_.wait_for(lock, std::chrono::milliseconds(20),
                   [&] { return available_ > 0 || cancel->load(); });
    }
  }

  void release() {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      ++available_;
    }
    cv_.notify_one();
  }

 private:
  std::mutex mutex_;
  std::condition_variable cv_;
  std::size_t available_;
};

// ── Snapshot publishing ──────────────────────────────────────────────────────

class Publisher {
 public:
  std::size_t subscribe(TransferManager::SnapshotListener listener) {
    std::lock_guard<std::mutex> lock(mutex_);
    const std::size_t id = next_id_++;
    listeners_.emplace(id, std::move(listener));
    return id;
  }

  void unsubscribe(std::size_t id) {
    std::lock_guard<std::mutex> lock(mutex_);
    listeners_.erase(id);
  }

  void notify(const TransferSnapshot& snapshot) {
    std::vector<TransferManager::SnapshotListener> listeners;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      listeners.reserve(listeners_.size());
      for (auto& entry : listeners_) listeners.push_back(entry.second);
    }
    for (auto& listener : listeners) {
      try {
        listener(snapshot);
      } catch (...) {
        // A listener must never take down a worker.
      }
    }
  }

 private:
  std::mutex mutex_;
  std::size_t next_id_ = 1;
  std::unordered_map<std::size_t, TransferManager::SnapshotListener> listeners_;
};

struct ConflictDecision {
  std::uint64_t item_index = 0;
  ConflictAction action;
};

struct Job {
  std::shared_ptr<class SnapshotCell> snapshot;
  std::shared_ptr<std::atomic<bool>> cancel = std::make_shared<std::atomic<bool>>(false);
  std::mutex decision_mutex;
  std::condition_variable decision_cv;
  std::deque<ConflictDecision> decisions;
};

// A snapshot plus its host-private staging inventory and persistence throttle.
// `write()` hands back a guard whose destructor publishes and/or persists based on
// the publish policy, so callers mutate and simply let the guard fall out of scope.
class SnapshotGuard;

class SnapshotCell {
 public:
  SnapshotCell(TransferSnapshot snapshot, std::shared_ptr<Publisher> publisher,
               std::shared_ptr<TransferHistoryStore> history)
      : snapshot_(std::move(snapshot)),
        history_(std::move(history)),
        publisher_(std::move(publisher)),
        last_time_(std::chrono::steady_clock::now() - std::chrono::seconds(1)),
        last_status_(snapshot_.status) {}

  SnapshotCell(const SnapshotCell&) = delete;
  SnapshotCell& operator=(const SnapshotCell&) = delete;

  [[nodiscard]] TransferSnapshot read() {
    std::lock_guard<std::mutex> lock(mutex_);
    return snapshot_;
  }

  SnapshotGuard write();
  SnapshotGuard write_force_persist();

  // Insert the initial queued row. Throws `TransferError` if history rejects it.
  void persist_initial() {
    if (!history_) return;
    std::lock_guard<std::mutex> lock(mutex_);
    const auto staging = staging_copy();
    try {
      history_->insert(snapshot_, staging);
    } catch (const std::exception& error) {
      throw TransferError(TransferErrorCode::Local,
                          std::string("could not persist transfer history: ") + error.what());
    }
  }

  void add_staging(TransferStagingFile artifact);
  void remove_staging(const TransferStagingFile& artifact);

  // Publish and persist a terminal lifecycle change without throttling. If the
  // durable ledger cannot record it, the live view is explicitly failed so a
  // success is never shown for a state a restart would reconcile differently.
  void finalize_status(TransferStatus requested) {
    TransferSnapshot copy;
    {
      std::unique_lock<std::mutex> lock(mutex_);
      snapshot_.current_item.reset();
      snapshot_.current_item_bytes = 0;
      snapshot_.current_item_total_bytes.reset();
      snapshot_.active_conflict.reset();
      snapshot_.status = requested;
      touch(snapshot_);
      if (history_) {
        const auto staging = staging_copy();
        try {
          history_->update(snapshot_, staging);
        } catch (...) {
          snapshot_.status = TransferStatus::Failed;
          snapshot_.failures.push_back(TransferItemFailure{
              std::nullopt, "transfer history", TransferFailureKind::LocalIo,
              "the final transfer state could not be saved; verify the destination before retrying",
              std::nullopt});
          touch(snapshot_);
          try {
            history_->update(snapshot_, staging);
          } catch (...) {
          }
        }
      }
      {
        std::lock_guard<std::mutex> publish_lock(publish_mutex_);
        last_time_ = std::chrono::steady_clock::now();
        last_status_ = snapshot_.status;
      }
      copy = snapshot_;
    }
    publish(copy);
  }

  void publish_initial() { publish(read()); }

 private:
  friend class SnapshotGuard;

  std::vector<TransferStagingFile> staging_copy() {
    std::lock_guard<std::mutex> lock(staging_mutex_);
    return staging_;
  }

  void persist_now() {
    if (!history_) return;
    std::lock_guard<std::mutex> lock(mutex_);
    const auto staging = staging_copy();
    try {
      history_->update(snapshot_, staging);
    } catch (const std::exception& error) {
      throw TransferError(TransferErrorCode::Local,
                          std::string("could not persist transfer state: ") + error.what());
    }
  }

  void persist_quiet(const TransferSnapshot& snapshot) {
    if (!history_) return;
    const auto staging = staging_copy();
    try {
      history_->update(snapshot, staging);
    } catch (...) {
      // The throttled progress write is best-effort; a terminal write is not.
    }
  }

  void publish(const TransferSnapshot& snapshot) const {
    if (publisher_) publisher_->notify(snapshot);
  }

  std::mutex mutex_;
  TransferSnapshot snapshot_;
  std::mutex staging_mutex_;
  std::vector<TransferStagingFile> staging_;
  std::shared_ptr<TransferHistoryStore> history_;
  std::shared_ptr<Publisher> publisher_;
  std::mutex publish_mutex_;
  std::chrono::steady_clock::time_point last_time_;
  TransferStatus last_status_;
};

class SnapshotGuard {
 public:
  SnapshotGuard(SnapshotCell* cell, std::unique_lock<std::mutex> lock, bool force)
      : cell_(cell), lock_(std::move(lock)), force_(force) {}
  SnapshotGuard(const SnapshotGuard&) = delete;
  SnapshotGuard& operator=(const SnapshotGuard&) = delete;
  ~SnapshotGuard() { commit(); }

  TransferSnapshot* operator->() const { return &cell_->snapshot_; }
  TransferSnapshot& operator*() const { return cell_->snapshot_; }

 private:
  void commit() {
    if (committed_) return;
    committed_ = true;
    const TransferStatus status = cell_->snapshot_.status;
    bool should_publish = false;
    bool should_persist = false;
    {
      std::lock_guard<std::mutex> publish_lock(cell_->publish_mutex_);
      const auto now = std::chrono::steady_clock::now();
      if (cell_->last_status_ != status ||
          now - cell_->last_time_ >= std::chrono::milliseconds(100)) {
        cell_->last_time_ = now;
        cell_->last_status_ = status;
        should_publish = true;
        should_persist = true;
      } else {
        should_persist = force_;
      }
    }
    if (should_persist) cell_->persist_quiet(cell_->snapshot_);
    TransferSnapshot copy;
    if (should_publish) copy = cell_->snapshot_;
    lock_.unlock();
    if (should_publish) cell_->publish(copy);
  }

  SnapshotCell* cell_;
  std::unique_lock<std::mutex> lock_;
  bool force_;
  bool committed_ = false;
};

SnapshotGuard SnapshotCell::write() {
  std::unique_lock<std::mutex> lock(mutex_);
  return SnapshotGuard(this, std::move(lock), false);
}

SnapshotGuard SnapshotCell::write_force_persist() {
  std::unique_lock<std::mutex> lock(mutex_);
  return SnapshotGuard(this, std::move(lock), true);
}

void SnapshotCell::add_staging(TransferStagingFile artifact) {
  {
    std::lock_guard<std::mutex> lock(staging_mutex_);
    staging_.push_back(std::move(artifact));
  }
  {
    auto guard = write_force_persist();
    touch(*guard);
  }
  persist_now();
}

void SnapshotCell::remove_staging(const TransferStagingFile& artifact) {
  {
    std::lock_guard<std::mutex> lock(staging_mutex_);
    staging_.erase(
        std::remove_if(staging_.begin(), staging_.end(),
                       [&](const TransferStagingFile& current) {
                         return current.remote == artifact.remote &&
                                current.path == artifact.path &&
                                current.server_id == artifact.server_id &&
                                current.item_index == artifact.item_index;
                       }),
        staging_.end());
  }
  {
    auto guard = write_force_persist();
    touch(*guard);
  }
  persist_now();
}

}  // namespace

// The shared manager state. Defined here (not in the header) so the transfer
// bookkeeping types stay private to this translation unit.
struct TransferManagerImpl {
  TransferManagerImpl(std::shared_ptr<TransferBackend> backend_value, std::size_t max_parallel,
                      std::shared_ptr<TransferHistoryStore> history_value)
      : backend(std::move(backend_value)),
        history(std::move(history_value)),
        permits(max_parallel < 1 ? 1 : max_parallel) {
    publisher = std::make_shared<Publisher>();
  }

  std::shared_ptr<TransferBackend> backend;
  std::shared_ptr<TransferHistoryStore> history;
  Semaphore permits;
  std::shared_ptr<Publisher> publisher;
  std::mutex pending_mutex;
  std::unordered_map<std::string, std::vector<TransferStagingFile>> pending_recovery;
  std::mutex jobs_mutex;
  std::unordered_map<std::string, std::shared_ptr<Job>> jobs;
  std::atomic<bool> shutting_down{false};
  std::atomic<std::size_t> active_workers{0};
  std::mutex workers_mutex;
  std::condition_variable workers_cv;
};

namespace {

std::shared_ptr<Job> find_job(TransferManagerImpl& inner, const std::string& id) {
  std::lock_guard<std::mutex> lock(inner.jobs_mutex);
  const auto found = inner.jobs.find(id);
  return found == inner.jobs.end() ? nullptr : found->second;
}

void insert_job(TransferManagerImpl& inner, const std::string& id, std::shared_ptr<Job> job) {
  if (inner.shutting_down.load()) {
    throw TransferError(TransferErrorCode::Cancelled, "the transfer manager is shutting down");
  }
  std::lock_guard<std::mutex> lock(inner.jobs_mutex);
  inner.jobs[id] = std::move(job);
}

void remove_terminal_job(TransferManagerImpl& inner, const std::string& id) {
  if (!inner.history) return;
  std::lock_guard<std::mutex> lock(inner.jobs_mutex);
  inner.jobs.erase(id);
}

// ── Snapshot mutation helpers ────────────────────────────────────────────────

void push_failure(const std::shared_ptr<SnapshotCell>& snapshot, TransferItemFailure failure) {
  auto guard = snapshot->write();
  if (failure.staging_residue) guard->staging_residue.push_back(*failure.staging_residue);
  guard->failures.push_back(std::move(failure));
  guard->current_item.reset();
  guard->current_item_bytes = 0;
  guard->current_item_total_bytes.reset();
  touch(*guard);
}

TransferItemFailure failure_for(std::optional<std::uint64_t> item_index, const std::string& item,
                                const TransferError& error,
                                std::optional<std::string> staging_residue) {
  return TransferItemFailure{item_index, item, error.failure_kind(), error.what(),
                             std::move(staging_residue)};
}

void note_staging_residue(const std::shared_ptr<SnapshotCell>& snapshot, std::uint64_t item_index,
                          const std::string& path, const TransferError& error) {
  push_failure(snapshot, failure_for(item_index, path, error, path));
}

std::uint64_t allocate_file(const std::shared_ptr<SnapshotCell>& snapshot, std::uint64_t bytes) {
  auto guard = snapshot->write();
  const std::uint64_t index = guard->total_files.value_or(0);
  guard->total_files = index + 1;
  guard->total_bytes = guard->total_bytes.value_or(0) + bytes;
  touch(*guard);
  return index;
}

void begin_item(const std::shared_ptr<SnapshotCell>& snapshot, const std::string& label,
                std::uint64_t total_bytes) {
  auto guard = snapshot->write();
  guard->current_item = label;
  guard->current_item_bytes = 0;
  guard->current_item_total_bytes = total_bytes;
  touch(*guard);
}

void complete_item(const std::shared_ptr<SnapshotCell>& snapshot, bool verified) {
  auto guard = snapshot->write();
  guard->completed_files = guard->completed_files + 1;
  if (verified) {
    guard->verified_files = guard->verified_files + 1;
  } else {
    guard->unverified_files = guard->unverified_files + 1;
  }
  guard->current_item.reset();
  guard->current_item_bytes = 0;
  guard->current_item_total_bytes.reset();
  touch(*guard);
}

void skip_item(const std::shared_ptr<SnapshotCell>& snapshot) {
  auto guard = snapshot->write();
  guard->skipped_files = guard->skipped_files + 1;
  guard->current_item.reset();
  guard->current_item_bytes = 0;
  guard->current_item_total_bytes.reset();
  touch(*guard);
}

void push_recovery_failure(TransferSnapshot& snapshot, std::uint64_t item_index, std::string item,
                           TransferFailureKind kind, std::string message) {
  item = truncate_utf8(item, 256);
  message = truncate_utf8(message, 512);
  const bool duplicate =
      std::any_of(snapshot.failures.begin(), snapshot.failures.end(),
                  [&](const TransferItemFailure& failure) {
                    return failure.item_index == item_index && failure.item == item &&
                           failure.message == message;
                  });
  if (!duplicate) {
    snapshot.staging_residue.push_back(item);
    snapshot.failures.push_back(TransferItemFailure{item_index, item, kind, message, item});
  }
  touch(snapshot);
}

// ── Remote path helpers ──────────────────────────────────────────────────────

// Walks every component of `path`, refusing symlinks, and optionally requiring the
// final component to be a directory. Returns the final attribute set.
RemoteStat ensure_remote_path_chain(TransferBackend& backend, const std::string& server_id,
                                    const std::string& path, bool expect_directory) {
  validate_remote_path(path);
  std::vector<std::string> components;
  std::size_t from = 0;
  for (;;) {
    const auto slash = path.find('/', from);
    const auto part =
        path.substr(from, slash == std::string::npos ? std::string::npos : slash - from);
    if (!part.empty()) components.push_back(part);
    if (slash == std::string::npos) break;
    from = slash + 1;
  }
  std::string current = "/";
  std::optional<RemoteStat> last;
  for (std::size_t index = 0; index < components.size(); ++index) {
    current = remote_child_checked(current, components[index]);
    const auto stat = backend.lstat(server_id, current);
    if (!stat) {
      throw TransferError(TransferErrorCode::Remote,
                          "remote path does not exist: " + current);
    }
    const bool is_final = index + 1 == components.size();
    if (stat->kind == RemoteEntryKind::Symlink) {
      throw TransferError(TransferErrorCode::InvalidInput,
                          "symbolic links cannot be traversed during transfer: " + current);
    }
    if (!is_final && stat->kind != RemoteEntryKind::Directory) {
      throw TransferError(TransferErrorCode::InvalidInput,
                          "remote path component is not a directory: " + current);
    }
    if (is_final && expect_directory && stat->kind != RemoteEntryKind::Directory) {
      throw TransferError(TransferErrorCode::InvalidInput,
                          "remote path is not a directory: " + current);
    }
    last = stat;
  }
  if (components.empty()) {
    const auto stat = backend.lstat(server_id, "/");
    if (!stat) throw TransferError(TransferErrorCode::Remote, "remote root does not exist");
    if (stat->kind != RemoteEntryKind::Directory) {
      throw TransferError(TransferErrorCode::InvalidInput, "remote root is not a directory");
    }
    return *stat;
  }
  if (!last) throw TransferError(TransferErrorCode::InvalidInput, "remote path is empty");
  return *last;
}

// Create `path` as a directory if absent, accepting a concurrent creation that
// really produced a directory and nothing else.
void ensure_remote_directory(TransferBackend& backend, const std::string& server_id,
                             const std::string& path) {
  validate_remote_path(path);
  if (path != "/") {
    ensure_remote_path_chain(backend, server_id, parent_of(path), true);
  }
  const auto stat = backend.lstat(server_id, path);
  if (stat) {
    if (stat->kind == RemoteEntryKind::Directory) return;
    if (stat->kind == RemoteEntryKind::Symlink) {
      throw TransferError(TransferErrorCode::InvalidInput,
                          "symbolic link cannot be used as a transfer directory: " + path);
    }
    throw TransferError(TransferErrorCode::InvalidInput,
                        "transfer directory path is occupied by a non-directory: " + path);
  }
  try {
    backend.create_dir(server_id, path);
  } catch (const TransferError&) {
    const auto after = backend.lstat(server_id, path);
    if (after && after->kind == RemoteEntryKind::Directory) return;
    throw;
  }
  ensure_remote_path_chain(backend, server_id, path, true);
}

// Create one local download directory, refusing a symlink or non-directory and
// re-checking the type after creation.
void ensure_local_directory(const std::filesystem::path& path) {
  std::error_code error;
  const auto status = std::filesystem::symlink_status(path, error);
  if (error && error != std::errc::no_such_file_or_directory) {
    throw TransferError(TransferErrorCode::Local,
                        "could not inspect the local directory: " + error.message());
  }
  if (status.type() == std::filesystem::file_type::not_found ||
      error == std::errc::no_such_file_or_directory) {
    std::error_code create_error;
    std::filesystem::create_directory(path, create_error);
    if (create_error) {
      throw TransferError(TransferErrorCode::Local,
                          "could not create the local directory: " + create_error.message());
    }
    std::error_code after_error;
    const auto after = std::filesystem::symlink_status(path, after_error);
    if (after_error || !std::filesystem::is_directory(after)) {
      throw TransferError(TransferErrorCode::InvalidInput,
                          "directory changed during creation: selected directory");
    }
    return;
  }
  if (std::filesystem::is_symlink(status) || !std::filesystem::is_directory(status)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "directory destination is a symbolic link or non-directory: selected directory");
  }
}

std::optional<std::uint64_t> file_time_epoch_seconds(
    const std::filesystem::file_time_type& time) {
  const auto system_now = std::chrono::system_clock::now();
  const auto file_now = std::filesystem::file_time_type::clock::now();
  const auto system_time = system_now + (time - file_now);
  const auto seconds =
      std::chrono::duration_cast<std::chrono::seconds>(system_time.time_since_epoch()).count();
  if (seconds < 0) return std::nullopt;
  return static_cast<std::uint64_t>(seconds);
}

// ── Execution context and work items ─────────────────────────────────────────

struct TransferExecutionContext {
  TransferBackend* backend = nullptr;
  std::string transfer_id;
  std::string server_id;
  std::shared_ptr<SnapshotCell> snapshot;
  std::shared_ptr<std::atomic<bool>> cancel;
  ConflictPolicy policy = ConflictPolicy::Ask;
};

struct UploadItem {
  std::filesystem::path source;
  std::string target;
  std::string display_name;
  std::uint64_t index = 0;
};

struct DownloadItem {
  std::string source;
  std::filesystem::path initial_target;
  std::string display_name;
  RemoteStat initial_stat;
  std::uint64_t index = 0;
};

enum class DestinationChoice { Skip, Create, Replace };

struct RemoteResolution {
  std::string target;
  DestinationChoice choice = DestinationChoice::Create;
  std::optional<RemoteStat> expected_target;
};

struct LocalResolution {
  std::filesystem::path target;
  DestinationChoice choice = DestinationChoice::Create;
  std::optional<LocalFingerprint> expected_target;
};

// ── Byte-stream adapters ─────────────────────────────────────────────────────

class LocalFileReader : public TransferReader {
 public:
  explicit LocalFileReader(std::ifstream& stream) : stream_(stream) {}

  std::size_t read(char* data, std::size_t size) override {
    stream_.read(data, static_cast<std::streamsize>(size));
    const auto count = static_cast<std::size_t>(stream_.gcount());
    if (stream_.bad() || (stream_.fail() && !stream_.eof())) {
      throw TransferError(TransferErrorCode::Local, "could not read the local source file");
    }
    return count;
  }

 private:
  std::ifstream& stream_;
};

class LocalStagingWriter : public TransferWriter {
 public:
  explicit LocalStagingWriter(LocalStagingFile file) : file_(std::move(file)) {}

  void write(const char* data, std::size_t size) override { file_.write(data, size); }
  void finish() override { file_.close(); }
  [[nodiscard]] const std::filesystem::path& path() const { return file_.path(); }

 private:
  LocalStagingFile file_;
};

struct StreamResult {
  std::uint64_t bytes = 0;
  std::string digest;
};

// Streams reader to writer in 64 KiB chunks, checking cancellation between every
// read and write, accumulating a SHA-256 and progress. A blocking backend call can
// still only be interrupted at a chunk boundary; the retired async host could
// cancel a read in flight, which the blocking SFTP seam cannot.
template <typename Reader, typename Writer>
StreamResult copy_stream(Reader& reader, Writer& writer,
                         const std::shared_ptr<SnapshotCell>& snapshot,
                         const std::shared_ptr<std::atomic<bool>>& cancel) {
  std::vector<char> buffer(TransferManager::kCopyBufferBytes);
  ContentHasher hasher;
  std::uint64_t total = 0;
  for (;;) {
    if (cancel->load()) throw TransferError(TransferErrorCode::Cancelled, "transfer cancelled");
    const std::size_t count = reader.read(buffer.data(), buffer.size());
    if (count == 0) break;
    if (cancel->load()) throw TransferError(TransferErrorCode::Cancelled, "transfer cancelled");
    writer.write(buffer.data(), count);
    hasher.update(buffer.data(), count);
    total += count;
    {
      auto guard = snapshot->write();
      guard->current_item_bytes = guard->current_item_bytes + count;
      guard->transferred_bytes = guard->transferred_bytes + count;
      touch(*guard);
    }
  }
  return {total, hasher.hexdigest()};
}

template <typename Reader>
std::string hash_reader(Reader& reader, const std::shared_ptr<std::atomic<bool>>& cancel) {
  std::vector<char> buffer(TransferManager::kCopyBufferBytes);
  ContentHasher hasher;
  for (;;) {
    if (cancel->load()) throw TransferError(TransferErrorCode::Cancelled, "transfer cancelled");
    const std::size_t count = reader.read(buffer.data(), buffer.size());
    if (count == 0) break;
    hasher.update(buffer.data(), count);
  }
  return hasher.hexdigest();
}

std::string hash_remote_file(TransferBackend& backend, const std::string& server_id,
                             const std::string& path,
                             const std::shared_ptr<std::atomic<bool>>& cancel) {
  auto reader = backend.open_read(server_id, path);
  return hash_reader(*reader, cancel);
}

std::string hash_local_file(const std::filesystem::path& path,
                            const std::shared_ptr<std::atomic<bool>>& cancel) {
  LocalSource source = open_local_source(path);
  LocalFileReader reader(source.stream);
  return hash_reader(reader, cancel);
}

// ── Remote staging ───────────────────────────────────────────────────────────

struct RemoteStage {
  std::string path;
  std::unique_ptr<TransferWriter> writer;
};

RemoteStage create_remote_stage(TransferBackend& backend, const std::string& server_id,
                                const std::string& transfer_id, const std::string& target,
                                std::uint64_t item_index) {
  const std::string parent = parent_of(target);
  const std::string name = basename_of(target);
  std::string owner = transfer_id;
  owner.erase(std::remove(owner.begin(), owner.end(), '-'), owner.end());
  for (std::uint32_t attempt = 0; attempt < 32; ++attempt) {
    const std::string stage_name = "." + name + ".yukinal-" + owner + "-" +
                                   std::to_string(item_index) + "-" + std::to_string(attempt) +
                                   ".part";
    const std::string stage_path = remote_child_checked(parent, stage_name);
    if (backend.lstat(server_id, stage_path)) continue;
    try {
      auto writer = backend.create_exclusive(server_id, stage_path);
      return {stage_path, std::move(writer)};
    } catch (const TransferError& error) {
      if (error.code() == TransferErrorCode::AlreadyExists) continue;
      throw;
    }
  }
  throw TransferError(TransferErrorCode::Unsupported,
                      "could not allocate a unique remote staging file after 32 attempts");
}

void cleanup_remote_stage(TransferBackend& backend, const std::string& server_id,
                          const std::string& stage_path, std::uint64_t item_index,
                          const std::shared_ptr<SnapshotCell>& snapshot,
                          const TransferStagingFile& artifact) {
  std::optional<RemoteStat> stat;
  try {
    stat = backend.lstat(server_id, stage_path);
  } catch (const TransferError& error) {
    note_staging_residue(snapshot, item_index, stage_path, error);
    return;
  }
  if (!stat) {
    try {
      snapshot->remove_staging(artifact);
    } catch (...) {
    }
    return;
  }
  if (stat->kind != RemoteEntryKind::File) {
    note_staging_residue(
        snapshot, item_index, stage_path,
        TransferError(TransferErrorCode::InvalidInput,
                      "remote staging path no longer refers to a regular file"));
    return;
  }
  try {
    backend.remove_file(server_id, stage_path);
  } catch (const TransferError& error) {
    note_staging_residue(snapshot, item_index, stage_path, error);
    return;
  }
  try {
    snapshot->remove_staging(artifact);
  } catch (...) {
  }
}

void remove_published_remote_stage(TransferBackend& backend, const std::string& server_id,
                                   const std::string& stage_path) {
  const auto stat = backend.lstat(server_id, stage_path);
  if (!stat) return;
  if (stat->kind == RemoteEntryKind::File) {
    backend.remove_file(server_id, stage_path);
    return;
  }
  throw TransferError(TransferErrorCode::InvalidInput,
                      "remote staging path no longer refers to a regular file");
}

// ── Conflict resolution ──────────────────────────────────────────────────────

ConflictAction await_conflict(const TransferExecutionContext& context, Job& job,
                              const ConflictRequest& conflict) {
  const std::uint64_t item_index = conflict.item_index;
  {
    auto guard = context.snapshot->write();
    guard->status = TransferStatus::WaitingConflict;
    guard->active_conflict = conflict;
    touch(*guard);
  }
  ConflictAction action;
  {
    std::unique_lock<std::mutex> lock(job.decision_mutex);
    for (;;) {
      if (context.cancel->load()) {
        throw TransferError(TransferErrorCode::Cancelled, "transfer cancelled");
      }
      if (!job.decisions.empty()) {
        ConflictDecision decision = job.decisions.front();
        job.decisions.pop_front();
        if (decision.item_index == item_index) {
          action = decision.action;
          break;
        }
        continue;
      }
      job.decision_cv.wait_for(lock, std::chrono::milliseconds(20), [&] {
        return !job.decisions.empty() || context.cancel->load();
      });
    }
  }
  {
    auto guard = context.snapshot->write();
    guard->status = TransferStatus::Running;
    guard->active_conflict.reset();
    touch(*guard);
  }
  return action;
}

RemoteResolution resolve_remote_destination(const TransferExecutionContext& context,
                                            const UploadItem& item, std::uint64_t incoming_size,
                                            const std::optional<RemoteStat>& existing, Job& job) {
  TransferBackend& backend = *context.backend;
  const std::string& server_id = context.server_id;
  const std::string& initial_target = item.target;
  if (!existing) return {initial_target, DestinationChoice::Create, std::nullopt};
  if (existing->kind != RemoteEntryKind::File) {
    throw TransferError(
        TransferErrorCode::InvalidInput,
        "remote transfer target is occupied by a directory, symbolic link, or special file");
  }
  if (context.policy == ConflictPolicy::Overwrite && !backend.supports_atomic_replace()) {
    throw TransferError(TransferErrorCode::Unsupported,
                        "this SFTP backend does not advertise an atomic overwrite rename");
  }
  std::vector<ConflictActionKind> allowed{ConflictActionKind::Skip, ConflictActionKind::Rename};
  if (backend.supports_atomic_replace()) allowed.push_back(ConflictActionKind::Overwrite);
  ConflictAction action;
  if (context.policy == ConflictPolicy::Skip) {
    action.kind = ConflictActionKind::Skip;
  } else if (context.policy == ConflictPolicy::Overwrite) {
    action.kind = ConflictActionKind::Overwrite;
  } else {
    ConflictRequest request;
    request.item_index = item.index;
    request.source_name = item.display_name;
    request.target_name = initial_target;
    request.existing_size = existing->size;
    request.incoming_size = incoming_size;
    if (existing->modified) request.existing_modified_epoch_seconds = *existing->modified;
    request.allowed_actions = std::move(allowed);
    action = await_conflict(context, job, request);
  }
  switch (action.kind) {
    case ConflictActionKind::Skip:
      return {initial_target, DestinationChoice::Skip, existing};
    case ConflictActionKind::Overwrite:
      if (!backend.supports_atomic_replace()) {
        throw TransferError(TransferErrorCode::Unsupported,
                            "this SFTP backend does not advertise an atomic overwrite rename");
      }
      return {initial_target, DestinationChoice::Replace, existing};
    case ConflictActionKind::Rename: {
      const std::string renamed =
          remote_child_checked(parent_of(initial_target), action.rename_to);
      if (backend.lstat(server_id, renamed)) {
        throw TransferError(TransferErrorCode::InvalidInput,
                            "the renamed remote target already exists");
      }
      return {renamed, DestinationChoice::Create, std::nullopt};
    }
  }
  throw TransferError(TransferErrorCode::InvalidInput, "unreachable conflict action");
}

LocalResolution resolve_local_destination(const TransferExecutionContext& context,
                                          const DownloadItem& item, Job& job) {
  const auto initial_target = item.initial_target;
  const auto existing = path_fingerprint_if_regular(initial_target);
  if (!existing) return {initial_target, DestinationChoice::Create, std::nullopt};
  ConflictAction action;
  if (context.policy == ConflictPolicy::Skip) {
    action.kind = ConflictActionKind::Skip;
  } else if (context.policy == ConflictPolicy::Overwrite) {
    action.kind = ConflictActionKind::Overwrite;
  } else {
    ConflictRequest request;
    request.item_index = item.index;
    request.source_name = item.display_name;
    request.target_name = path_to_utf8(initial_target.filename());
    request.existing_size = existing->size;
    request.incoming_size = item.initial_stat.size;
    if (existing->modified) {
      request.existing_modified_epoch_seconds = file_time_epoch_seconds(*existing->modified);
    }
    request.allowed_actions = {ConflictActionKind::Skip, ConflictActionKind::Overwrite,
                               ConflictActionKind::Rename};
    action = await_conflict(context, job, request);
  }
  switch (action.kind) {
    case ConflictActionKind::Skip:
      return {initial_target, DestinationChoice::Skip, existing};
    case ConflictActionKind::Overwrite:
      return {initial_target, DestinationChoice::Replace, existing};
    case ConflictActionKind::Rename: {
      safe_name(action.rename_to);
      const auto parent = initial_target.parent_path();
      if (parent.empty()) {
        throw TransferError(TransferErrorCode::InvalidInput,
                            "download target has no parent directory");
      }
      const auto renamed = parent / path_from_utf8(action.rename_to);
      if (path_fingerprint_if_regular(renamed)) {
        throw TransferError(TransferErrorCode::InvalidInput,
                            "the renamed local target already exists");
      }
      return {renamed, DestinationChoice::Create, std::nullopt};
    }
  }
  throw TransferError(TransferErrorCode::InvalidInput, "unreachable conflict action");
}

// ── Single-file transfer ─────────────────────────────────────────────────────

void upload_file(const TransferExecutionContext& context, const UploadItem& item, Job& job) {
  TransferBackend& backend = *context.backend;
  const std::string& server_id = context.server_id;
  const auto& snapshot = context.snapshot;
  const auto& cancel = context.cancel;

  LocalSource input = open_local_source(item.source);
  const std::uint64_t size = input.fingerprint.size;
  begin_item(snapshot, item.display_name, size);
  const auto initial_stat = backend.lstat(server_id, item.target);
  const RemoteResolution resolution =
      resolve_remote_destination(context, item, size, initial_stat, job);
  if (resolution.choice == DestinationChoice::Skip) {
    skip_item(snapshot);
    return;
  }

  ensure_remote_path_chain(backend, server_id, parent_of(resolution.target), true);
  RemoteStage stage =
      create_remote_stage(backend, server_id, context.transfer_id, resolution.target, item.index);
  TransferStagingFile stage_artifact;
  stage_artifact.remote = true;
  stage_artifact.server_id = server_id;
  stage_artifact.path = stage.path;
  stage_artifact.item_index = item.index;
  try {
    snapshot->add_staging(stage_artifact);
  } catch (...) {
    stage.writer.reset();
    cleanup_remote_stage(backend, server_id, stage.path, item.index, snapshot, stage_artifact);
    throw;
  }

  try {
    LocalFileReader reader(input.stream);
    const StreamResult stream = copy_stream(reader, *stage.writer, snapshot, cancel);
    if (stream.bytes != size) {
      throw TransferError(TransferErrorCode::SourceChanged,
                          "selected local source changed size while being uploaded (expected " +
                              std::to_string(size) + " bytes, read " +
                              std::to_string(stream.bytes) + ")");
    }
    verify_local_source_unchanged(item.source, input.fingerprint);
    if (cancel->load()) throw TransferError(TransferErrorCode::Cancelled, "transfer cancelled");
    stage.writer->finish();
    stage.writer.reset();

    const auto stage_stat = backend.lstat(server_id, stage.path);
    if (!stage_stat || stage_stat->kind != RemoteEntryKind::File ||
        stage_stat->size != stream.bytes) {
      throw TransferError(
          TransferErrorCode::Verification,
          "remote staging file size or type did not match the uploaded stream");
    }
    const std::string remote_digest = hash_remote_file(backend, server_id, stage.path, cancel);
    if (remote_digest != stream.digest) {
      throw TransferError(TransferErrorCode::Verification,
                          "remote staging file SHA-256 did not match the local source");
    }
    if (resolution.choice == DestinationChoice::Create) {
      if (backend.lstat(server_id, resolution.target)) {
        throw TransferError(TransferErrorCode::AlreadyExists, "destination already exists");
      }
      backend.publish_no_replace(server_id, stage.path, resolution.target);
    } else {
      const auto current = backend.lstat(server_id, resolution.target);
      if (!current) {
        throw TransferError(TransferErrorCode::InvalidInput,
                            "overwrite target disappeared before publication");
      }
      if (!resolution.expected_target || *current != *resolution.expected_target) {
        throw TransferError(TransferErrorCode::InvalidInput,
                            "overwrite target changed while the upload was staged");
      }
      backend.rename_replace(server_id, stage.path, resolution.target);
    }
    const auto published = backend.lstat(server_id, resolution.target);
    if (!published || published->kind != RemoteEntryKind::File ||
        published->size != stream.bytes) {
      throw TransferError(
          TransferErrorCode::Verification,
          "published remote file size or type did not match the uploaded stream");
    }
  } catch (...) {
    stage.writer.reset();
    cleanup_remote_stage(backend, server_id, stage.path, item.index, snapshot, stage_artifact);
    throw;
  }

  if (resolution.choice == DestinationChoice::Create) {
    bool removed = false;
    try {
      remove_published_remote_stage(backend, server_id, stage.path);
      removed = true;
    } catch (const TransferError& error) {
      note_staging_residue(
          snapshot, item.index, stage.path,
          TransferError(TransferErrorCode::Remote,
                        "remote destination was published and verified, but its staging link "
                        "could not be removed: " +
                            std::string(error.what())));
    }
    if (removed) snapshot->remove_staging(stage_artifact);
  } else {
    snapshot->remove_staging(stage_artifact);
  }
  complete_item(snapshot, true);
}

void download_file(const TransferExecutionContext& context, const DownloadItem& item, Job& job) {
  TransferBackend& backend = *context.backend;
  const std::string& server_id = context.server_id;
  const auto& snapshot = context.snapshot;
  const auto& cancel = context.cancel;
  if (item.initial_stat.kind != RemoteEntryKind::File) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "only remote regular files can be downloaded");
  }
  begin_item(snapshot, item.display_name, item.initial_stat.size);
  const LocalResolution resolution = resolve_local_destination(context, item, job);
  if (resolution.choice == DestinationChoice::Skip) {
    skip_item(snapshot);
    return;
  }
  if (resolution.target.parent_path().empty()) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "download target has no parent directory");
  }
  ensure_download_directory(resolution.target.parent_path());
  const std::string transfer_id = snapshot->read().transfer_id;
  auto writer = std::make_unique<LocalStagingWriter>(
      create_local_staging_file(resolution.target, transfer_id, item.index));
  const std::filesystem::path stage_path = writer->path();
  TransferStagingFile stage_artifact;
  stage_artifact.remote = false;
  stage_artifact.path = path_to_utf8(stage_path);
  stage_artifact.item_index = item.index;
  try {
    snapshot->add_staging(stage_artifact);
  } catch (...) {
    writer.reset();
    try {
      remove_local_staging(stage_path);
    } catch (...) {
    }
    try {
      snapshot->remove_staging(stage_artifact);
    } catch (...) {
    }
    throw;
  }

  try {
    // SFTP v3 has no open-no-follow flag, so every path component is lstat-checked
    // before the open and the source is re-checked immediately after.
    ensure_remote_path_chain(backend, server_id, item.source, false);
    auto remote_input = backend.open_read(server_id, item.source);
    const auto after_open = backend.lstat(server_id, item.source);
    if (!after_open || !(*after_open == item.initial_stat) ||
        after_open->kind != RemoteEntryKind::File) {
      throw TransferError(TransferErrorCode::SourceChanged, item.source);
    }
    const StreamResult stream = copy_stream(*remote_input, *writer, snapshot, cancel);
    if (stream.bytes != item.initial_stat.size) {
      throw TransferError(TransferErrorCode::SourceChanged,
                          item.source + " changed size while being downloaded (expected " +
                              std::to_string(item.initial_stat.size) + " bytes, read " +
                              std::to_string(stream.bytes) + ")");
    }
    if (cancel->load()) throw TransferError(TransferErrorCode::Cancelled, "transfer cancelled");
    writer->finish();
    writer.reset();
    const std::string staged_digest = hash_local_file(stage_path, cancel);
    if (staged_digest != stream.digest) {
      throw TransferError(
          TransferErrorCode::Verification,
          "local staging file SHA-256 did not match the downloaded stream");
    }
    // The remote endpoint offers no independent digest, so the source is read a
    // second time and compared before the local target is published.
    const std::string second_digest = hash_remote_file(backend, server_id, item.source, cancel);
    if (second_digest != stream.digest) {
      throw TransferError(TransferErrorCode::SourceChanged,
                          item.source + " changed between download verification reads");
    }
    const auto final_remote = backend.lstat(server_id, item.source);
    if (!final_remote || !(*final_remote == item.initial_stat)) {
      throw TransferError(TransferErrorCode::SourceChanged, item.source);
    }
    if (resolution.choice == DestinationChoice::Create) {
      if (path_fingerprint_if_regular(resolution.target)) {
        throw TransferError(TransferErrorCode::AlreadyExists, "destination already exists");
      }
      publish_local_no_replace(stage_path, resolution.target);
    } else {
      if (!resolution.expected_target) {
        throw TransferError(TransferErrorCode::InvalidInput,
                            "overwrite target guard is missing");
      }
      publish_local_replace(stage_path, resolution.target, *resolution.expected_target);
    }
  } catch (...) {
    writer.reset();
    try {
      remove_local_staging(stage_path);
      try {
        snapshot->remove_staging(stage_artifact);
      } catch (...) {
      }
    } catch (const TransferError& cleanup_error) {
      note_staging_residue(snapshot, item.index, path_to_utf8(stage_path.filename()),
                           cleanup_error);
    }
    throw;
  }
  snapshot->remove_staging(stage_artifact);
  complete_item(snapshot, true);
}

// ── Batch walkers ────────────────────────────────────────────────────────────

struct UploadFrame {
  std::filesystem::path local_dir;
  std::string remote_dir;
  std::string display_prefix;
};

struct DownloadFrame {
  std::string remote_dir;
  std::filesystem::path target_dir;
  std::string display_prefix;
};

void upload_batch(const TransferExecutionContext& context, const std::string& remote_dir,
                  const std::vector<std::filesystem::path>& local_paths, Job& job) {
  TransferBackend& backend = *context.backend;
  const std::string& server_id = context.server_id;
  const auto& snapshot = context.snapshot;
  const auto& cancel = context.cancel;
  try {
    ensure_remote_directory(backend, server_id, remote_dir);
  } catch (const TransferError& error) {
    push_failure(snapshot, failure_for(std::nullopt, remote_dir, error, std::nullopt));
    return;
  }

  for (const auto& source : local_paths) {
    if (cancel->load()) break;
    const std::string name = path_to_utf8(source.filename());
    if (name.empty()) {
      push_failure(snapshot,
                   failure_for(std::nullopt, "selected source file",
                               TransferError(TransferErrorCode::InvalidInput,
                                             "local path has no portable UTF-8 name"),
                               std::nullopt));
      continue;
    }
    try {
      safe_name(name);
    } catch (const TransferError& error) {
      push_failure(snapshot, failure_for(std::nullopt, name, error, std::nullopt));
      continue;
    }
    std::error_code status_error;
    const auto status = std::filesystem::symlink_status(source, status_error);
    if (status_error && status_error != std::errc::no_such_file_or_directory) {
      push_failure(snapshot,
                   failure_for(std::nullopt, name,
                               TransferError(TransferErrorCode::Local,
                                             "could not inspect the local source: " +
                                                 status_error.message()),
                               std::nullopt));
      continue;
    }
    if (status.type() == std::filesystem::file_type::not_found) {
      push_failure(snapshot,
                   failure_for(std::nullopt, name,
                               TransferError(TransferErrorCode::Local,
                                             "could not inspect the local source"),
                               std::nullopt));
      continue;
    }
    if (std::filesystem::is_symlink(status)) {
      push_failure(snapshot,
                   failure_for(std::nullopt, name,
                               TransferError(TransferErrorCode::InvalidInput,
                                             "symbolic links and reparse points are not followed"),
                               std::nullopt));
      continue;
    }
    std::string remote_root;
    try {
      remote_root = remote_child_checked(remote_dir, name);
    } catch (const TransferError& error) {
      push_failure(snapshot, failure_for(std::nullopt, name, error, std::nullopt));
      continue;
    }
    if (std::filesystem::is_regular_file(status)) {
      std::error_code size_error;
      const auto size = std::filesystem::file_size(source, size_error);
      const std::uint64_t index = allocate_file(snapshot, size_error ? 0 : size);
      try {
        upload_file(context, UploadItem{source, remote_root, name, index}, job);
      } catch (const TransferError& error) {
        push_failure(snapshot, failure_for(index, name, error, std::nullopt));
      }
    } else if (std::filesystem::is_directory(status)) {
      try {
        ensure_remote_directory(backend, server_id, remote_root);
      } catch (const TransferError& error) {
        push_failure(snapshot, failure_for(std::nullopt, name, error, std::nullopt));
        continue;
      }
      std::vector<UploadFrame> stack{{source, remote_root, name}};
      while (!stack.empty()) {
        const UploadFrame frame = stack.back();
        stack.pop_back();
        if (cancel->load()) break;
        std::vector<std::filesystem::path> children;
        try {
          children = read_local_directory(frame.local_dir);
        } catch (const TransferError& error) {
          push_failure(snapshot,
                       failure_for(std::nullopt, frame.display_prefix, error, std::nullopt));
          continue;
        }
        for (auto it = children.rbegin(); it != children.rend(); ++it) {
          const auto& child = *it;
          const std::string child_name = path_to_utf8(child.filename());
          if (child_name.empty()) {
            push_failure(snapshot,
                         failure_for(std::nullopt, frame.display_prefix,
                                     TransferError(TransferErrorCode::InvalidInput,
                                                   "directory entry name is not UTF-8"),
                                     std::nullopt));
            continue;
          }
          try {
            safe_name(child_name);
          } catch (const TransferError& error) {
            push_failure(snapshot, failure_for(std::nullopt, child_name, error, std::nullopt));
            continue;
          }
          std::error_code child_error;
          const auto child_status = std::filesystem::symlink_status(child, child_error);
          if (child_error || child_status.type() == std::filesystem::file_type::not_found) {
            push_failure(snapshot,
                         failure_for(std::nullopt, child_name,
                                     TransferError(TransferErrorCode::Local,
                                                   "could not inspect the local entry"),
                                     std::nullopt));
            continue;
          }
          if (std::filesystem::is_symlink(child_status)) {
            push_failure(
                snapshot,
                failure_for(std::nullopt, frame.display_prefix + "/" + child_name,
                            TransferError(TransferErrorCode::InvalidInput,
                                          "symbolic links and reparse points are not followed"),
                            std::nullopt));
            continue;
          }
          std::string child_remote;
          try {
            child_remote = remote_child_checked(frame.remote_dir, child_name);
          } catch (const TransferError& error) {
            push_failure(snapshot, failure_for(std::nullopt, child_name, error, std::nullopt));
            continue;
          }
          const std::string display = frame.display_prefix + "/" + child_name;
          if (std::filesystem::is_directory(child_status)) {
            try {
              ensure_remote_directory(backend, server_id, child_remote);
            } catch (const TransferError& error) {
              push_failure(snapshot, failure_for(std::nullopt, display, error, std::nullopt));
              continue;
            }
            stack.push_back(UploadFrame{child, child_remote, display});
          } else if (std::filesystem::is_regular_file(child_status)) {
            std::error_code size_error;
            const auto child_size = std::filesystem::file_size(child, size_error);
            const std::uint64_t index = allocate_file(snapshot, size_error ? 0 : child_size);
            try {
              upload_file(context, UploadItem{child, child_remote, display, index}, job);
            } catch (const TransferError& error) {
              push_failure(snapshot, failure_for(index, display, error, std::nullopt));
            }
          } else {
            push_failure(snapshot,
                         failure_for(std::nullopt, display,
                                     TransferError(TransferErrorCode::InvalidInput,
                                                   "special files cannot be transferred"),
                                     std::nullopt));
          }
        }
      }
    } else {
      push_failure(snapshot,
                   failure_for(std::nullopt, name,
                               TransferError(TransferErrorCode::InvalidInput,
                                             "special files cannot be transferred"),
                               std::nullopt));
    }
  }
}

void download_batch(const TransferExecutionContext& context,
                    const std::vector<std::string>& remote_paths,
                    const std::filesystem::path& local_dir, Job& job) {
  TransferBackend& backend = *context.backend;
  const std::string& server_id = context.server_id;
  const auto& snapshot = context.snapshot;
  const auto& cancel = context.cancel;
  try {
    ensure_download_directory(local_dir);
  } catch (const TransferError& error) {
    push_failure(snapshot,
                 failure_for(std::nullopt, "local download destination", error, std::nullopt));
    return;
  }

  for (const auto& source : remote_paths) {
    if (cancel->load()) break;
    const std::string name = basename_of(source);
    try {
      safe_name(name);
    } catch (const TransferError& error) {
      push_failure(snapshot, failure_for(std::nullopt, source, error, std::nullopt));
      continue;
    }
    std::optional<RemoteStat> stat;
    try {
      stat = backend.lstat(server_id, source);
    } catch (const TransferError& error) {
      push_failure(snapshot, failure_for(std::nullopt, source, error, std::nullopt));
      continue;
    }
    if (!stat) {
      push_failure(snapshot,
                   failure_for(std::nullopt, source,
                               TransferError(TransferErrorCode::Remote, "source does not exist"),
                               std::nullopt));
      continue;
    }
    if (stat->kind == RemoteEntryKind::File) {
      const auto target = local_dir / path_from_utf8(name);
      const std::uint64_t index = allocate_file(snapshot, stat->size);
      try {
        download_file(context, DownloadItem{source, target, name, *stat, index}, job);
      } catch (const TransferError& error) {
        push_failure(snapshot, failure_for(index, source, error, std::nullopt));
      }
    } else if (stat->kind == RemoteEntryKind::Directory) {
      const auto root_target = local_dir / path_from_utf8(name);
      try {
        ensure_local_directory(root_target);
      } catch (const TransferError& error) {
        push_failure(snapshot, failure_for(std::nullopt, source, error, std::nullopt));
        continue;
      }
      std::vector<DownloadFrame> stack{{source, root_target, name}};
      while (!stack.empty()) {
        const DownloadFrame frame = stack.back();
        stack.pop_back();
        if (cancel->load()) break;
        std::vector<RemoteTransferEntry> entries;
        try {
          entries = backend.list_dir(server_id, frame.remote_dir);
        } catch (const TransferError& error) {
          push_failure(snapshot,
                       failure_for(std::nullopt, frame.display_prefix, error, std::nullopt));
          continue;
        }
        for (auto it = entries.rbegin(); it != entries.rend(); ++it) {
          const auto& entry = *it;
          if (entry.name == "." || entry.name == "..") continue;
          try {
            safe_name(entry.name);
          } catch (const TransferError& error) {
            push_failure(snapshot, failure_for(std::nullopt, entry.name, error, std::nullopt));
            continue;
          }
          std::string child_remote;
          try {
            child_remote = remote_child_checked(frame.remote_dir, entry.name);
          } catch (const TransferError& error) {
            push_failure(snapshot, failure_for(std::nullopt, entry.name, error, std::nullopt));
            continue;
          }
          std::optional<RemoteStat> child_stat;
          try {
            child_stat = backend.lstat(server_id, child_remote);
          } catch (const TransferError& error) {
            push_failure(snapshot, failure_for(std::nullopt, child_remote, error, std::nullopt));
            continue;
          }
          if (!child_stat) {
            push_failure(snapshot,
                         failure_for(std::nullopt, child_remote,
                                     TransferError(TransferErrorCode::Remote,
                                                   "entry disappeared during traversal"),
                                     std::nullopt));
            continue;
          }
          const auto child_target = frame.target_dir / path_from_utf8(entry.name);
          const std::string display = frame.display_prefix + "/" + entry.name;
          if (child_stat->kind == RemoteEntryKind::Directory) {
            try {
              ensure_local_directory(child_target);
            } catch (const TransferError& error) {
              push_failure(snapshot, failure_for(std::nullopt, display, error, std::nullopt));
              continue;
            }
            stack.push_back(DownloadFrame{child_remote, child_target, display});
          } else if (child_stat->kind == RemoteEntryKind::File) {
            const std::uint64_t index = allocate_file(snapshot, child_stat->size);
            try {
              download_file(context,
                            DownloadItem{child_remote, child_target, display, *child_stat, index},
                            job);
            } catch (const TransferError& error) {
              push_failure(snapshot, failure_for(index, display, error, std::nullopt));
            }
          } else {
            push_failure(snapshot,
                         failure_for(std::nullopt, display,
                                     TransferError(TransferErrorCode::InvalidInput,
                                                   "symbolic links and special files are not "
                                                   "transferred"),
                                     std::nullopt));
          }
        }
      }
    } else {
      push_failure(snapshot,
                   failure_for(std::nullopt, source,
                               TransferError(TransferErrorCode::InvalidInput,
                                             "symbolic links and special files are not transferred"),
                               std::nullopt));
    }
  }
}

// ── Job lifecycle ────────────────────────────────────────────────────────────

using JobWork = std::function<void(TransferBackend&, const std::shared_ptr<SnapshotCell>&,
                                   const std::shared_ptr<std::atomic<bool>>&, Job&)>;

void run_job(const std::shared_ptr<TransferManagerImpl>& inner, const std::string& transfer_id,
             const std::shared_ptr<Job>& job, const JobWork& work) {
  if (!inner->permits.acquire(job->cancel)) {
    job->snapshot->finalize_status(TransferStatus::Cancelled);
    remove_terminal_job(*inner, transfer_id);
    return;
  }
  bool cancelled_early = false;
  {
    auto guard = job->snapshot->write();
    if (job->cancel->load()) {
      guard->status = TransferStatus::Cancelled;
      touch(*guard);
      cancelled_early = true;
    } else {
      guard->status = TransferStatus::Running;
      touch(*guard);
    }
  }
  if (!cancelled_early) {
    work(*inner->backend, job->snapshot, job->cancel, *job);
  }
  inner->permits.release();
  if (cancelled_early) {
    remove_terminal_job(*inner, transfer_id);
    return;
  }
  TransferStatus final_status;
  {
    const auto snapshot = job->snapshot->read();
    if (job->cancel->load()) {
      final_status = TransferStatus::Cancelled;
    } else if (!snapshot.failures.empty() || snapshot.skipped_files > 0) {
      final_status = (snapshot.completed_files > 0 || snapshot.skipped_files > 0)
                         ? TransferStatus::Partial
                         : TransferStatus::Failed;
    } else {
      final_status = TransferStatus::Completed;
    }
  }
  job->snapshot->finalize_status(final_status);
  remove_terminal_job(*inner, transfer_id);
}

void validate_start(const std::string& server_id, const std::string& remote_dir,
                    const std::vector<std::filesystem::path>& local_paths) {
  if (trim(server_id).empty()) {
    throw TransferError(TransferErrorCode::InvalidInput, "serverId is required");
  }
  validate_remote_path(remote_dir);
  if (local_paths.empty()) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "at least one local source is required");
  }
  for (const auto& path : local_paths) {
    if (!path.is_absolute()) {
      throw TransferError(TransferErrorCode::InvalidInput,
                          "local source paths must be absolute host paths");
    }
  }
}

void validate_download_start(const std::string& server_id,
                             const std::vector<std::string>& remote_paths,
                             const std::filesystem::path& local_dir) {
  if (trim(server_id).empty()) {
    throw TransferError(TransferErrorCode::InvalidInput, "serverId is required");
  }
  if (remote_paths.empty()) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "at least one remote source is required");
  }
  for (const auto& path : remote_paths) validate_remote_path(path);
  if (!local_dir.is_absolute()) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "local destination must be an absolute host path");
  }
}

// Synchronous, fail-closed startup recovery. Active rows become `interrupted`,
// validated local staging files are removed, and remote staging records stay
// pending an authenticated session.
void recover_into(const std::shared_ptr<TransferManagerImpl>& inner) {
  std::vector<RecoveredTransfer> recovered;
  try {
    recovered = inner->history->recover(epoch_millis());
  } catch (const std::exception& error) {
    throw TransferError(TransferErrorCode::Local,
                        std::string("transfer recovery failed: ") + error.what());
  }
  for (auto& record : recovered) {
    TransferSnapshot snapshot = record.snapshot;
    std::vector<TransferStagingFile> pending;
    for (const auto& artifact : record.staging) {
      if (!artifact.remote) {
        try {
          remove_recovered_local_staging(path_from_utf8(artifact.path), snapshot.transfer_id);
        } catch (const TransferError& error) {
          std::string item = basename_of(artifact.path);
          if (item.empty()) item = "local staging file";
          push_recovery_failure(snapshot, artifact.item_index, item,
                                TransferFailureKind::LocalIo,
                                std::string("startup recovery could not clean local staging: ") +
                                    error.what());
          pending.push_back(artifact);
        }
      } else {
        std::string name = basename_of(artifact.path);
        if (name.empty()) name = "remote staging file";
        const bool safe = artifact.server_id == snapshot.server_id &&
                          remote_path_is_valid(artifact.path) &&
                          is_owned_staging_name(name, snapshot.transfer_id);
        if (!safe) {
          push_recovery_failure(
              snapshot, artifact.item_index, "remote staging record",
              TransferFailureKind::RemoteIo,
              "startup recovery rejected a remote staging record that did not match its owner");
        } else {
          push_recovery_failure(
              snapshot, artifact.item_index, name, TransferFailureKind::RemoteIo,
              "remote staging cleanup is pending an authenticated server session");
        }
        pending.push_back(artifact);
      }
    }
    if (!pending.empty()) {
      std::lock_guard<std::mutex> lock(inner->pending_mutex);
      inner->pending_recovery[snapshot.transfer_id] = pending;
    }
    try {
      inner->history->update(snapshot, pending);
    } catch (const std::exception& error) {
      throw TransferError(TransferErrorCode::Local,
                          std::string("could not persist transfer recovery: ") + error.what());
    }
  }
}

}  // namespace

TransferManager::TransferManager(std::shared_ptr<TransferBackend> backend)
    : TransferManager(std::move(backend), kMaxParallelTransfers) {}

TransferManager::TransferManager(std::shared_ptr<TransferBackend> backend,
                                 std::size_t max_parallel_transfers)
    : impl_(std::make_shared<TransferManagerImpl>(std::move(backend), max_parallel_transfers,
                                                  nullptr)) {}

TransferManager::TransferManager(std::shared_ptr<TransferManagerImpl> impl)
    : impl_(std::move(impl)) {}

TransferManager TransferManager::with_history(std::shared_ptr<TransferBackend> backend,
                                              std::shared_ptr<TransferHistoryStore> history) {
  auto impl = std::make_shared<TransferManagerImpl>(std::move(backend), kMaxParallelTransfers,
                                                    std::move(history));
  recover_into(impl);
  return TransferManager(std::move(impl));
}

std::string TransferManager::start_upload(const std::string& server_id,
                                          const std::string& remote_dir,
                                          std::vector<std::filesystem::path> local_paths,
                                          ConflictPolicy policy) {
  validate_start(server_id, remote_dir, local_paths);
  const std::string id = new_transfer_id();
  auto snapshot = std::make_shared<SnapshotCell>(
      new_transfer_snapshot(id, server_id, TransferDirection::Upload), impl_->publisher,
      impl_->history);
  auto job = std::make_shared<Job>();
  job->snapshot = snapshot;
  snapshot->persist_initial();
  insert_job(*impl_, id, job);
  snapshot->publish_initial();

  auto inner = impl_;
  inner->active_workers.fetch_add(1);
  std::thread([inner, id, job, server_id = std::string(server_id),
               remote_dir = std::string(remote_dir), local_paths = std::move(local_paths),
               policy]() {
    try {
      run_job(
          inner, id, job,
          [&](TransferBackend& backend, const std::shared_ptr<SnapshotCell>& cell,
              const std::shared_ptr<std::atomic<bool>>& cancel, Job& worker_job) {
            TransferExecutionContext context{&backend, id, server_id, cell, cancel, policy};
            upload_batch(context, remote_dir, local_paths, worker_job);
          });
    } catch (...) {
    }
    inner->active_workers.fetch_sub(1);
    inner->workers_cv.notify_all();
  }).detach();
  return id;
}

std::string TransferManager::start_download(const std::string& server_id,
                                            std::vector<std::string> remote_paths,
                                            std::filesystem::path local_dir,
                                            ConflictPolicy policy) {
  validate_download_start(server_id, remote_paths, local_dir);
  const std::string id = new_transfer_id();
  auto snapshot = std::make_shared<SnapshotCell>(
      new_transfer_snapshot(id, server_id, TransferDirection::Download), impl_->publisher,
      impl_->history);
  auto job = std::make_shared<Job>();
  job->snapshot = snapshot;
  snapshot->persist_initial();
  insert_job(*impl_, id, job);
  snapshot->publish_initial();

  auto inner = impl_;
  inner->active_workers.fetch_add(1);
  std::thread([inner, id, job, server_id = std::string(server_id),
               remote_paths = std::move(remote_paths), local_dir = std::move(local_dir),
               policy]() {
    try {
      run_job(
          inner, id, job,
          [&](TransferBackend& backend, const std::shared_ptr<SnapshotCell>& cell,
              const std::shared_ptr<std::atomic<bool>>& cancel, Job& worker_job) {
            TransferExecutionContext context{&backend, id, server_id, cell, cancel, policy};
            download_batch(context, remote_paths, local_dir, worker_job);
          });
    } catch (...) {
    }
    inner->active_workers.fetch_sub(1);
    inner->workers_cv.notify_all();
  }).detach();
  return id;
}

std::optional<TransferSnapshot> TransferManager::get(const std::string& transfer_id) const {
  if (auto job = find_job(*impl_, transfer_id)) return job->snapshot->read();
  if (!impl_->history) return std::nullopt;
  try {
    return impl_->history->get(transfer_id);
  } catch (...) {
    return std::nullopt;
  }
}

std::vector<TransferSnapshot> TransferManager::list(const std::optional<std::string>& server_id,
                                                    std::size_t limit) const {
  if (limit == 0 || limit > 100) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "transfer history limit must be between 1 and 100");
  }
  std::map<std::string, TransferSnapshot> snapshots;
  if (impl_->history) {
    std::vector<TransferSnapshot> stored;
    try {
      stored = impl_->history->list(server_id, limit);
    } catch (const std::exception& error) {
      throw TransferError(TransferErrorCode::Local,
                          std::string("could not list transfer history: ") + error.what());
    }
    for (auto& snapshot : stored) snapshots[snapshot.transfer_id] = std::move(snapshot);
  }
  std::vector<std::shared_ptr<Job>> jobs;
  {
    std::lock_guard<std::mutex> lock(impl_->jobs_mutex);
    jobs.reserve(impl_->jobs.size());
    for (auto& entry : impl_->jobs) jobs.push_back(entry.second);
  }
  for (auto& job : jobs) {
    auto snapshot = job->snapshot->read();
    if (!server_id || snapshot.server_id == *server_id) {
      snapshots[snapshot.transfer_id] = std::move(snapshot);
    }
  }
  std::vector<TransferSnapshot> result;
  result.reserve(snapshots.size());
  for (auto& entry : snapshots) result.push_back(std::move(entry.second));
  std::sort(result.begin(), result.end(),
            [](const TransferSnapshot& left, const TransferSnapshot& right) {
              if (left.updated_at_epoch_ms != right.updated_at_epoch_ms) {
                return left.updated_at_epoch_ms > right.updated_at_epoch_ms;
              }
              return left.transfer_id > right.transfer_id;
            });
  if (result.size() > limit) result.resize(limit);
  return result;
}

void TransferManager::cancel(const std::string& transfer_id) {
  auto job = find_job(*impl_, transfer_id);
  if (!job) throw TransferError(TransferErrorCode::InvalidInput, "transfer was not found");
  job->cancel->store(true);
  job->decision_cv.notify_all();
}

void TransferManager::resolve_conflict(const std::string& transfer_id, std::uint64_t item_index,
                                       ConflictAction action) {
  auto job = find_job(*impl_, transfer_id);
  if (!job) throw TransferError(TransferErrorCode::InvalidInput, "transfer was not found");
  ConflictRequest conflict;
  {
    const auto snapshot = job->snapshot->read();
    if (!snapshot.active_conflict) {
      throw TransferError(TransferErrorCode::InvalidInput,
                          "transfer is not waiting for a conflict decision");
    }
    if (snapshot.status != TransferStatus::WaitingConflict ||
        snapshot.active_conflict->item_index != item_index) {
      throw TransferError(TransferErrorCode::InvalidInput,
                          "conflict decision does not match the active transfer item");
    }
    conflict = *snapshot.active_conflict;
  }
  if (std::find(conflict.allowed_actions.begin(), conflict.allowed_actions.end(), action.kind) ==
      conflict.allowed_actions.end()) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "conflict action is not allowed for this target");
  }
  if (action.kind == ConflictActionKind::Rename) safe_name(action.rename_to);
  {
    std::lock_guard<std::mutex> lock(job->decision_mutex);
    job->decisions.push_back(ConflictDecision{item_index, std::move(action)});
  }
  job->decision_cv.notify_all();
}

void TransferManager::reconcile_remote_staging() {
  std::unordered_map<std::string, std::vector<TransferStagingFile>> pending;
  {
    std::lock_guard<std::mutex> lock(impl_->pending_mutex);
    pending = impl_->pending_recovery;
  }
  for (auto& entry : pending) {
    const std::string& transfer_id = entry.first;
    std::vector<TransferStagingFile> remaining;
    std::vector<std::string> cleaned_names;
    for (const auto& artifact : entry.second) {
      if (artifact.remote) {
        const std::string name = basename_of(artifact.path);
        if (is_owned_staging_name(name, transfer_id) && remote_path_is_valid(artifact.path)) {
          try {
            impl_->backend->remove_recovered_staging(artifact.server_id, artifact.path);
            cleaned_names.push_back(name);
            continue;
          } catch (...) {
            remaining.push_back(artifact);
            continue;
          }
        }
      }
      remaining.push_back(artifact);
    }
    if (cleaned_names.empty()) continue;
    if (impl_->history) {
      std::optional<TransferSnapshot> snapshot;
      try {
        snapshot = impl_->history->get(transfer_id);
      } catch (...) {
        continue;
      }
      if (!snapshot) continue;
      snapshot->staging_residue.erase(
          std::remove_if(snapshot->staging_residue.begin(), snapshot->staging_residue.end(),
                         [&](const std::string& name) {
                           return std::find(cleaned_names.begin(), cleaned_names.end(), name) !=
                                  cleaned_names.end();
                         }),
          snapshot->staging_residue.end());
      snapshot->failures.erase(
          std::remove_if(
              snapshot->failures.begin(), snapshot->failures.end(),
              [&](const TransferItemFailure& failure) {
                return failure.message ==
                           "remote staging cleanup is pending an authenticated server session" &&
                       failure.staging_residue &&
                       std::find(cleaned_names.begin(), cleaned_names.end(),
                                 *failure.staging_residue) != cleaned_names.end();
              }),
          snapshot->failures.end());
      touch(*snapshot);
      try {
        impl_->history->update(*snapshot, remaining);
      } catch (...) {
      }
    }
    {
      std::lock_guard<std::mutex> lock(impl_->pending_mutex);
      impl_->pending_recovery[transfer_id] = remaining;
    }
  }
}

void TransferManager::shutdown() {
  if (impl_->shutting_down.exchange(true)) return;
  std::vector<std::shared_ptr<Job>> jobs;
  {
    std::lock_guard<std::mutex> lock(impl_->jobs_mutex);
    jobs.reserve(impl_->jobs.size());
    for (auto& entry : impl_->jobs) jobs.push_back(entry.second);
  }
  std::vector<std::shared_ptr<Job>> previously_active;
  for (auto& job : jobs) {
    if (!is_terminal(job->snapshot->read().status)) {
      previously_active.push_back(job);
      job->cancel->store(true);
      job->decision_cv.notify_all();
    }
  }
  {
    std::unique_lock<std::mutex> lock(impl_->workers_mutex);
    impl_->workers_cv.wait(lock, [&] { return impl_->active_workers.load() == 0; });
  }
  for (auto& job : previously_active) {
    auto guard = job->snapshot->write();
    guard->status = TransferStatus::Interrupted;
    guard->active_conflict.reset();
    guard->current_item.reset();
    touch(*guard);
  }
}

std::size_t TransferManager::subscribe(SnapshotListener listener) {
  return impl_->publisher->subscribe(std::move(listener));
}

void TransferManager::unsubscribe(std::size_t subscription) {
  impl_->publisher->unsubscribe(subscription);
}

std::size_t TransferManager::active_worker_count() const { return impl_->active_workers.load(); }

}  // namespace yukinal::desktop_host
