// Tests for the C++ transfer manager (`transfer_manager`). They drive the
// manager entirely through an in-memory backend and history store so upload,
// download, conflict, cancellation, parallelism, recovery and the fail-closed
// terminal history write are exercised without a server or a database.

#include "yukinal/desktop_host/transfer_backend.hpp"
#include "yukinal/desktop_host/transfer_manager.hpp"
#include "yukinal/desktop_host/transfer_types.hpp"
#include "yukinal/desktop_host/ipc_router.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <iterator>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <vector>

namespace {

using namespace yukinal::desktop_host;

void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}

template <typename F>
void require_transfer_error(F&& call, TransferErrorCode code, const char* message) {
  try {
    call();
  } catch (const TransferError& error) {
    if (error.code() == code) return;
  }
  std::cerr << message << '\n';
  std::exit(1);
}

template <typename F>
bool wait_until(F predicate, int timeout_ms = 15000) {
  const auto deadline =
      std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
  while (std::chrono::steady_clock::now() < deadline) {
    if (predicate()) return true;
    std::this_thread::sleep_for(std::chrono::milliseconds(5));
  }
  return predicate();
}

TransferSnapshot wait_terminal(TransferManager& manager, const std::string& transfer_id,
                               int timeout_ms = 15000) {
  TransferSnapshot result;
  const bool reached = wait_until(
      [&] {
        const auto snapshot = manager.get(transfer_id);
        if (snapshot && is_terminal(snapshot->status)) {
          result = *snapshot;
          return true;
        }
        return false;
      },
      timeout_ms);
  check(reached, "a transfer did not reach a terminal state in time");
  return result;
}

TransferSnapshot wait_conflict(TransferManager& manager, const std::string& transfer_id) {
  TransferSnapshot result;
  const bool ready = wait_until([&] {
    const auto snapshot = manager.get(transfer_id);
    if (snapshot && snapshot->active_conflict) {
      result = *snapshot;
      return true;
    }
    return false;
  });
  check(ready, "a transfer did not present a conflict in time");
  return result;
}

std::string path_to_utf8(const std::filesystem::path& path) {
  const auto value = path.u8string();
  return std::string(reinterpret_cast<const char*>(value.data()), value.size());
}

struct TemporaryDirectory {
  std::filesystem::path path = std::filesystem::temp_directory_path() /
                               ("yukinal-transfer-manager-" + std::to_string(next_id()));
  TemporaryDirectory() { std::filesystem::create_directories(path); }
  ~TemporaryDirectory() {
    std::error_code error;
    std::filesystem::remove_all(path, error);
  }

 private:
  static std::uint64_t next_id() {
    static std::atomic<std::uint64_t> counter{0};
    return counter.fetch_add(1);
  }
};

void write_file(const std::filesystem::path& path, const std::string& content) {
  std::ofstream out(path, std::ios::binary);
  out << content;
}

std::string read_file(const std::filesystem::path& path) {
  std::ifstream in(path, std::ios::binary);
  return std::string(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
}

// ── In-memory backend and history store ──────────────────────────────────────

struct FakeNode {
  RemoteEntryKind kind = RemoteEntryKind::File;
  std::string content;
  std::optional<std::uint32_t> modified;

  RemoteStat stat() const {
    return RemoteStat{kind, kind == RemoteEntryKind::File ? content.size() : 0, modified};
  }
};

class FakeReader;
class FakeWriter;

class FakeBackend : public TransferBackend {
 public:
  explicit FakeBackend(bool atomic_replace = true) : atomic_(atomic_replace) {
    nodes_["/"] = FakeNode{RemoteEntryKind::Directory, {}, std::nullopt};
  }

  [[nodiscard]] bool supports_atomic_replace() const override { return atomic_; }

  void set_read_delay(int milliseconds) { read_delay_.store(milliseconds); }
  [[nodiscard]] std::size_t peak_reads() const { return peak_reads_.load(); }
  [[nodiscard]] std::size_t live_reads() const { return live_reads_.load(); }
  [[nodiscard]] std::size_t recovered_removals() const { return recovered_removals_.load(); }
  [[nodiscard]] int read_delay_ms() const { return read_delay_.load(); }

  void note_read() {
    const std::size_t live = live_reads_.fetch_add(1) + 1;
    std::size_t peak = peak_reads_.load();
    while (live > peak && !peak_reads_.compare_exchange_weak(peak, live)) {
    }
  }
  void note_read_end() { live_reads_.fetch_sub(1); }

  void add_file(const std::string& path, const std::string& content,
                std::optional<std::uint32_t> modified = std::nullopt) {
    std::lock_guard<std::mutex> lock(mutex_);
    nodes_[path] = FakeNode{RemoteEntryKind::File, content, modified};
  }
  void add_directory(const std::string& path) {
    std::lock_guard<std::mutex> lock(mutex_);
    nodes_[path] = FakeNode{RemoteEntryKind::Directory, {}, std::nullopt};
  }
  [[nodiscard]] bool has(const std::string& path) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return nodes_.count(path) != 0;
  }
  [[nodiscard]] std::string content(const std::string& path) const {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = nodes_.find(path);
    return found == nodes_.end() ? std::string{} : found->second.content;
  }
  void commit(const std::string& path, const std::string& content) {
    std::lock_guard<std::mutex> lock(mutex_);
    nodes_[path] = FakeNode{RemoteEntryKind::File, content, std::nullopt};
  }

  std::optional<RemoteStat> lstat(const std::string&, const std::string& path) override {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = nodes_.find(path);
    if (found == nodes_.end()) return std::nullopt;
    return found->second.stat();
  }

  std::vector<RemoteTransferEntry> list_dir(const std::string&,
                                            const std::string& path) override {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = nodes_.find(path);
    if (found == nodes_.end() || found->second.kind != RemoteEntryKind::Directory) {
      throw TransferError(TransferErrorCode::Remote, "not a directory: " + path);
    }
    const std::string prefix = path == "/" ? "/" : path + "/";
    std::vector<RemoteTransferEntry> entries;
    for (const auto& entry : nodes_) {
      if (entry.first.size() <= prefix.size() ||
          entry.first.compare(0, prefix.size(), prefix) != 0) {
        continue;
      }
      const std::string rest = entry.first.substr(prefix.size());
      if (rest.find('/') != std::string::npos) continue;
      entries.push_back(RemoteTransferEntry{rest, entry.second.stat()});
    }
    std::sort(entries.begin(), entries.end(),
              [](const RemoteTransferEntry& left, const RemoteTransferEntry& right) {
                return left.name < right.name;
              });
    return entries;
  }

  std::unique_ptr<TransferReader> open_read(const std::string&,
                                            const std::string& path) override;

  std::unique_ptr<TransferWriter> create_exclusive(const std::string&,
                                                   const std::string& path) override;

  void create_dir(const std::string&, const std::string& path) override {
    std::lock_guard<std::mutex> lock(mutex_);
    if (nodes_.count(path)) {
      throw TransferError(TransferErrorCode::AlreadyExists, "path exists: " + path);
    }
    nodes_[path] = FakeNode{RemoteEntryKind::Directory, {}, std::nullopt};
  }

  void remove_file(const std::string&, const std::string& path) override {
    std::lock_guard<std::mutex> lock(mutex_);
    if (nodes_.erase(path) == 0) {
      throw TransferError(TransferErrorCode::Remote, "no such file: " + path);
    }
  }

  void publish_no_replace(const std::string&, const std::string& source,
                          const std::string& destination) override {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = nodes_.find(source);
    if (found == nodes_.end()) {
      throw TransferError(TransferErrorCode::Remote, "missing staging: " + source);
    }
    if (nodes_.count(destination)) {
      throw TransferError(TransferErrorCode::AlreadyExists, "destination exists");
    }
    nodes_[destination] = found->second;
  }

  void rename_replace(const std::string&, const std::string& source,
                      const std::string& destination) override {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = nodes_.find(source);
    if (found == nodes_.end()) {
      throw TransferError(TransferErrorCode::Remote, "missing staging: " + source);
    }
    FakeNode node = found->second;
    nodes_.erase(found);
    nodes_[destination] = std::move(node);
  }

  void remove_recovered_staging(const std::string&, const std::string& path) override {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = nodes_.find(path);
    if (found == nodes_.end()) return;
    if (found->second.kind != RemoteEntryKind::File) {
      throw TransferError(TransferErrorCode::Remote, "not a regular file: " + path);
    }
    nodes_.erase(found);
    ++recovered_removals_;
  }

 private:
  mutable std::mutex mutex_;
  std::map<std::string, FakeNode> nodes_;
  bool atomic_ = true;
  std::atomic<int> read_delay_{0};
  std::atomic<std::size_t> live_reads_{0};
  std::atomic<std::size_t> peak_reads_{0};
  std::atomic<std::size_t> recovered_removals_{0};
};

class FakeReader : public TransferReader {
 public:
  FakeReader(FakeBackend& backend, std::string content)
      : backend_(backend), content_(std::move(content)) {}

  std::size_t read(char* data, std::size_t size) override {
    if (offset_ >= content_.size()) return 0;
    const std::size_t count = std::min(size, content_.size() - offset_);
    backend_.note_read();
    const int delay = backend_.read_delay_ms();
    if (delay > 0) std::this_thread::sleep_for(std::chrono::milliseconds(delay));
    std::memcpy(data, content_.data() + offset_, count);
    offset_ += count;
    backend_.note_read_end();
    return count;
  }

 private:
  FakeBackend& backend_;
  std::string content_;
  std::size_t offset_ = 0;
};

class FakeWriter : public TransferWriter {
 public:
  FakeWriter(FakeBackend& backend, std::string path)
      : backend_(backend), path_(std::move(path)) {}

  void write(const char* data, std::size_t size) override { buffer_.append(data, size); }
  void finish() override { backend_.commit(path_, buffer_); }

 private:
  FakeBackend& backend_;
  std::string path_;
  std::string buffer_;
};

std::unique_ptr<TransferWriter> FakeBackend::create_exclusive(const std::string&,
                                                              const std::string& path) {
  std::lock_guard<std::mutex> lock(mutex_);
  if (nodes_.count(path)) {
    throw TransferError(TransferErrorCode::AlreadyExists, "staging exists: " + path);
  }
  nodes_[path] = FakeNode{};
  return std::make_unique<FakeWriter>(*this, path);
}

std::unique_ptr<TransferReader> FakeBackend::open_read(const std::string&,
                                                       const std::string& path) {
  std::string content;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = nodes_.find(path);
    if (found == nodes_.end() || found->second.kind != RemoteEntryKind::File) {
      throw TransferError(TransferErrorCode::Remote, "no such file: " + path);
    }
    content = found->second.content;
  }
  return std::make_unique<FakeReader>(*this, std::move(content));
}

class FakeHistoryStore : public TransferHistoryStore {
 public:
  void insert(const TransferSnapshot& snapshot,
              const std::vector<TransferStagingFile>& staging) override {
    std::lock_guard<std::mutex> lock(mutex_);
    if (fail_insert_) throw std::runtime_error("history insert failed");
    rows_[snapshot.transfer_id] = snapshot;
    staging_[snapshot.transfer_id] = staging;
  }

  void update(const TransferSnapshot& snapshot,
              const std::vector<TransferStagingFile>& staging) override {
    std::lock_guard<std::mutex> lock(mutex_);
    if (fail_terminal_ && is_terminal(snapshot.status)) {
      throw std::runtime_error("history update failed");
    }
    rows_[snapshot.transfer_id] = snapshot;
    staging_[snapshot.transfer_id] = staging;
  }

  std::optional<TransferSnapshot> get(const std::string& transfer_id) override {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = rows_.find(transfer_id);
    if (found == rows_.end()) return std::nullopt;
    return found->second;
  }

  std::vector<TransferSnapshot> list(const std::optional<std::string>& server_id,
                                     std::size_t limit) override {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<TransferSnapshot> out;
    for (const auto& entry : rows_) {
      if (!server_id || entry.second.server_id == *server_id) out.push_back(entry.second);
    }
    std::sort(out.begin(), out.end(),
              [](const TransferSnapshot& left, const TransferSnapshot& right) {
                if (left.updated_at_epoch_ms != right.updated_at_epoch_ms) {
                  return left.updated_at_epoch_ms > right.updated_at_epoch_ms;
                }
                return left.transfer_id > right.transfer_id;
              });
    if (out.size() > limit) out.resize(limit);
    return out;
  }

  std::vector<RecoveredTransfer> recover(std::uint64_t now_epoch_ms) override {
    std::lock_guard<std::mutex> lock(mutex_);
    std::vector<RecoveredTransfer> out;
    for (auto& entry : rows_) {
      if (is_terminal(entry.second.status)) continue;
      entry.second.status = TransferStatus::Interrupted;
      entry.second.updated_at_epoch_ms = now_epoch_ms;
      const auto staging = staging_.find(entry.first);
      out.push_back(RecoveredTransfer{
          entry.second, staging == staging_.end() ? std::vector<TransferStagingFile>{}
                                                  : staging->second});
    }
    return out;
  }

  std::vector<TransferStagingFile> staging(const std::string& transfer_id) override {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = staging_.find(transfer_id);
    return found == staging_.end() ? std::vector<TransferStagingFile>{} : found->second;
  }

  void seed(const TransferSnapshot& snapshot, const std::vector<TransferStagingFile>& staging) {
    std::lock_guard<std::mutex> lock(mutex_);
    rows_[snapshot.transfer_id] = snapshot;
    staging_[snapshot.transfer_id] = staging;
  }
  void set_fail_terminal(bool value) {
    std::lock_guard<std::mutex> lock(mutex_);
    fail_terminal_ = value;
  }
  std::optional<TransferSnapshot> row(const std::string& transfer_id) { return get(transfer_id); }
  std::vector<TransferStagingFile> staging_of(const std::string& transfer_id) {
    return staging(transfer_id);
  }

 private:
  std::mutex mutex_;
  std::map<std::string, TransferSnapshot> rows_;
  std::map<std::string, std::vector<TransferStagingFile>> staging_;
  bool fail_terminal_ = false;
  bool fail_insert_ = false;
};

// Records every published snapshot so a test can observe states that are not
// durable (a failed terminal history write leaves the stored row non-terminal).
class Observation {
 public:
  void record(const TransferSnapshot& snapshot) {
    std::lock_guard<std::mutex> lock(mutex_);
    latest_[snapshot.transfer_id] = snapshot;
  }
  std::optional<TransferSnapshot> get(const std::string& transfer_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = latest_.find(transfer_id);
    if (found == latest_.end()) return std::nullopt;
    return found->second;
  }

 private:
  std::mutex mutex_;
  std::map<std::string, TransferSnapshot> latest_;
};

// The read-only remote seam the drag preparation drives. A test configures the
// `lstat` result per path (or a thrown `TransferError`) without a server.
class FakeRemoteDragProbe : public RemoteDragProbe {
 public:
  void set(const std::string& path, std::optional<RemoteStat> stat) {
    std::lock_guard<std::mutex> lock(mutex_);
    stats_[path] = std::move(stat);
  }
  void set_error(TransferErrorCode code, std::string message) {
    std::lock_guard<std::mutex> lock(mutex_);
    error_code_ = code;
    error_message_ = std::move(message);
  }
  void clear_error() {
    std::lock_guard<std::mutex> lock(mutex_);
    error_code_.reset();
  }

  std::optional<RemoteStat> lstat(const std::string&, const std::string& path) override {
    std::lock_guard<std::mutex> lock(mutex_);
    if (error_code_.has_value()) throw TransferError(*error_code_, error_message_);
    const auto found = stats_.find(path);
    if (found == stats_.end()) return std::nullopt;
    return found->second;
  }

 private:
  std::mutex mutex_;
  std::map<std::string, std::optional<RemoteStat>> stats_;
  std::optional<TransferErrorCode> error_code_;
  std::string error_message_;
};

// ── Checks ───────────────────────────────────────────────────────────────────

void run_upload_and_download_checks() {
  TemporaryDirectory temporary;
  auto backend = std::make_shared<FakeBackend>();
  backend->add_directory("/srv");

  const auto source = temporary.path / "hello.txt";
  write_file(source, "hello world");
  {
    TransferManager manager(backend);
    const std::string id =
        manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Skip);
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Completed && snapshot.completed_files == 1 &&
              snapshot.verified_files == 1 && snapshot.total_files == 1 &&
              snapshot.total_bytes == 11 && snapshot.failures.empty(),
          "an upload of one file must complete and verify");
    check(backend->has("/srv/hello.txt") &&
              backend->content("/srv/hello.txt") == "hello world",
          "an upload must publish the file at the remote directory");
    manager.shutdown();
  }

  const auto tree = temporary.path / "tree";
  std::filesystem::create_directories(tree / "b");
  write_file(tree / "a.txt", "alpha");
  write_file(tree / "b" / "c.txt", "gamma");
  {
    TransferManager manager(backend);
    const std::string id =
        manager.start_upload("srv_1", "/srv", {tree}, ConflictPolicy::Skip);
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Completed && snapshot.completed_files == 2 &&
              snapshot.total_files == 2,
          "an upload of a directory tree must transfer every file");
    check(backend->content("/srv/tree/a.txt") == "alpha" &&
              backend->content("/srv/tree/b/c.txt") == "gamma",
          "a directory upload must recreate the nested layout");
    manager.shutdown();
  }

  backend->add_directory("/srv/logs");
  backend->add_file("/srv/logs/app.log", "some logs");
  const auto downloads = temporary.path / "downloads";
  std::filesystem::create_directories(downloads);
  {
    TransferManager manager(backend);
    const std::string id = manager.start_download("srv_1", {"/srv/logs/app.log"}, downloads,
                                                  ConflictPolicy::Skip);
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Completed && snapshot.completed_files == 1 &&
              snapshot.verified_files == 1,
          "a download of one file must complete and verify");
    check(read_file(downloads / "app.log") == "some logs",
          "a download must publish the file into the local directory");
    manager.shutdown();
  }

  backend->add_directory("/srv/project");
  backend->add_directory("/srv/project/src");
  backend->add_file("/srv/project/readme.txt", "read me");
  backend->add_file("/srv/project/src/main.cpp", "int main() {}");
  const auto outdir = temporary.path / "out";
  std::filesystem::create_directories(outdir);
  {
    TransferManager manager(backend);
    const std::string id =
        manager.start_download("srv_1", {"/srv/project"}, outdir, ConflictPolicy::Skip);
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Completed && snapshot.completed_files == 2,
          "a download of a remote directory tree must transfer every file");
    check(read_file(outdir / "project" / "readme.txt") == "read me" &&
              read_file(outdir / "project" / "src" / "main.cpp") == "int main() {}",
          "a directory download must recreate the nested layout");
    manager.shutdown();
  }
}

void run_conflict_checks() {
  TemporaryDirectory temporary;

  // Upload conflict, Ask -> Skip.
  {
    auto backend = std::make_shared<FakeBackend>();
    backend->add_directory("/srv");
    backend->add_file("/srv/app.log", "old");
    const auto source = temporary.path / "app.log";
    write_file(source, "new");
    TransferManager manager(backend);
    const std::string id =
        manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Ask);
    const auto waiting = wait_conflict(manager, id);
    check(waiting.status == TransferStatus::WaitingConflict &&
              waiting.active_conflict->source_name == "app.log" &&
              waiting.active_conflict->existing_size == 3 &&
              waiting.active_conflict->incoming_size == 3 &&
              std::find(waiting.active_conflict->allowed_actions.begin(),
                        waiting.active_conflict->allowed_actions.end(),
                        ConflictActionKind::Overwrite) !=
                  waiting.active_conflict->allowed_actions.end(),
          "an upload conflict must report both sizes and, on an atomic backend, overwrite");
    manager.resolve_conflict(id, waiting.active_conflict->item_index,
                             ConflictAction{ConflictActionKind::Skip, {}});
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Partial && snapshot.skipped_files == 1 &&
              snapshot.completed_files == 0,
          "skipping an upload conflict must leave the transfer partial");
    check(backend->content("/srv/app.log") == "old",
          "skipping an upload conflict must not touch the destination");
    manager.shutdown();
  }

  // Upload conflict, Ask -> Overwrite.
  {
    auto backend = std::make_shared<FakeBackend>(true);
    backend->add_directory("/srv");
    backend->add_file("/srv/app.log", "old");
    const auto source = temporary.path / "app.log";
    write_file(source, "newer");
    TransferManager manager(backend);
    const std::string id =
        manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Ask);
    const auto waiting = wait_conflict(manager, id);
    manager.resolve_conflict(id, waiting.active_conflict->item_index,
                             ConflictAction{ConflictActionKind::Overwrite, {}});
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Completed && snapshot.completed_files == 1,
          "overwriting an upload conflict must complete");
    check(backend->content("/srv/app.log") == "newer",
          "overwriting an upload conflict must replace the destination");
    manager.shutdown();
  }

  // Upload conflict, Ask -> Rename.
  {
    auto backend = std::make_shared<FakeBackend>();
    backend->add_directory("/srv");
    backend->add_file("/srv/app.log", "old");
    const auto source = temporary.path / "app.log";
    write_file(source, "renamed");
    TransferManager manager(backend);
    const std::string id =
        manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Ask);
    const auto waiting = wait_conflict(manager, id);
    manager.resolve_conflict(id, waiting.active_conflict->item_index,
                             ConflictAction{ConflictActionKind::Rename, "copy.log"});
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Completed,
          "renaming an upload conflict must complete");
    check(backend->content("/srv/app.log") == "old" &&
              backend->content("/srv/copy.log") == "renamed",
          "renaming must create the new name and leave the original untouched");
    manager.shutdown();
  }

  // Download conflict, Ask -> Skip.
  {
    auto backend = std::make_shared<FakeBackend>();
    backend->add_directory("/srv");
    backend->add_file("/srv/report.txt", "remote");
    const auto downloads = temporary.path / "conflict-skip";
    std::filesystem::create_directories(downloads);
    write_file(downloads / "report.txt", "local");
    TransferManager manager(backend);
    const std::string id = manager.start_download("srv_1", {"/srv/report.txt"}, downloads,
                                                  ConflictPolicy::Ask);
    const auto waiting = wait_conflict(manager, id);
    check(waiting.active_conflict->existing_size == 5 &&
              waiting.active_conflict->incoming_size == 6,
          "a download conflict must report the local and remote sizes");
    manager.resolve_conflict(id, waiting.active_conflict->item_index,
                             ConflictAction{ConflictActionKind::Skip, {}});
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Partial && snapshot.skipped_files == 1,
          "skipping a download conflict must leave the transfer partial");
    check(read_file(downloads / "report.txt") == "local",
          "skipping a download conflict must not touch the local file");
    manager.shutdown();
  }

  // Download conflict, Ask -> Overwrite.
  {
    auto backend = std::make_shared<FakeBackend>();
    backend->add_directory("/srv");
    backend->add_file("/srv/report.txt", "remote");
    const auto downloads = temporary.path / "conflict-overwrite";
    std::filesystem::create_directories(downloads);
    write_file(downloads / "report.txt", "local");
    TransferManager manager(backend);
    const std::string id = manager.start_download("srv_1", {"/srv/report.txt"}, downloads,
                                                  ConflictPolicy::Ask);
    const auto waiting = wait_conflict(manager, id);
    manager.resolve_conflict(id, waiting.active_conflict->item_index,
                             ConflictAction{ConflictActionKind::Overwrite, {}});
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Completed,
          "overwriting a download conflict must complete");
    check(read_file(downloads / "report.txt") == "remote",
          "overwriting a download conflict must replace the local file");
    manager.shutdown();
  }

  // A backend without atomic replace must not offer overwrite.
  {
    auto backend = std::make_shared<FakeBackend>(false);
    backend->add_directory("/srv");
    backend->add_file("/srv/app.log", "old");
    const auto source = temporary.path / "app.log";
    write_file(source, "new");
    TransferManager manager(backend);
    const std::string id =
        manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Ask);
    const auto waiting = wait_conflict(manager, id);
    check(std::find(waiting.active_conflict->allowed_actions.begin(),
                    waiting.active_conflict->allowed_actions.end(),
                    ConflictActionKind::Overwrite) ==
              waiting.active_conflict->allowed_actions.end(),
          "a backend without atomic replace must not offer overwrite");
    require_transfer_error(
        [&] {
          manager.resolve_conflict(id, waiting.active_conflict->item_index,
                                   ConflictAction{ConflictActionKind::Overwrite, {}});
        },
        TransferErrorCode::InvalidInput, "an unsupported overwrite decision must be rejected");
    manager.resolve_conflict(id, waiting.active_conflict->item_index,
                             ConflictAction{ConflictActionKind::Skip, {}});
    (void)wait_terminal(manager, id);
    manager.shutdown();
  }

  // An overwrite policy against a non-atomic backend fails closed.
  {
    auto backend = std::make_shared<FakeBackend>(false);
    backend->add_directory("/srv");
    backend->add_file("/srv/app.log", "old");
    const auto source = temporary.path / "app.log";
    write_file(source, "new");
    TransferManager manager(backend);
    const std::string id =
        manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Overwrite);
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Failed && snapshot.failures.size() == 1 &&
              snapshot.failures.front().kind == TransferFailureKind::Unsupported,
          "an overwrite policy without atomic replace must fail closed");
    check(backend->content("/srv/app.log") == "old",
          "a refused overwrite must not modify the destination");
    manager.shutdown();
  }
}

void run_cancellation_checks() {
  TemporaryDirectory temporary;

  // Cancel a transfer blocked on a conflict decision.
  {
    auto backend = std::make_shared<FakeBackend>();
    backend->add_directory("/srv");
    backend->add_file("/srv/app.log", "old");
    const auto source = temporary.path / "app.log";
    write_file(source, "new");
    TransferManager manager(backend);
    const std::string id =
        manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Ask);
    (void)wait_conflict(manager, id);
    manager.cancel(id);
    const auto snapshot = wait_terminal(manager, id);
    check(snapshot.status == TransferStatus::Cancelled,
          "cancelling a transfer waiting on a conflict must cancel it");
    manager.shutdown();
  }

  // Cancel a transfer still queued behind the worker cap.
  {
    auto backend = std::make_shared<FakeBackend>();
    backend->add_directory("/srv");
    backend->add_file("/srv/slow.bin", std::string(256 * 1024, 'a'));
    backend->add_file("/srv/other.bin", "other");
    backend->set_read_delay(300);
    const auto downloads = temporary.path / "cancel-queue";
    std::filesystem::create_directories(downloads);
    TransferManager manager(backend, 1);
    const std::string running =
        manager.start_download("srv_1", {"/srv/slow.bin"}, downloads, ConflictPolicy::Skip);
    std::this_thread::sleep_for(std::chrono::milliseconds(100));
    const std::string queued =
        manager.start_download("srv_1", {"/srv/other.bin"}, downloads, ConflictPolicy::Skip);
    manager.cancel(queued);
    const auto queued_snapshot = wait_terminal(manager, queued);
    check(queued_snapshot.status == TransferStatus::Cancelled,
          "cancelling a queued transfer must cancel it without running");
    const auto running_snapshot = wait_terminal(manager, running);
    check(running_snapshot.status == TransferStatus::Completed,
          "cancelling a queued transfer must not disturb the running one");
    manager.shutdown();
  }

  // Shutdown interrupts an unfinished transfer.
  {
    auto backend = std::make_shared<FakeBackend>();
    backend->add_directory("/srv");
    backend->add_file("/srv/app.log", "old");
    const auto source = temporary.path / "app.log";
    write_file(source, "new");
    TransferManager manager(backend);
    const std::string id =
        manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Ask);
    (void)wait_conflict(manager, id);
    manager.shutdown();
    const auto snapshot = manager.get(id);
    check(snapshot && snapshot->status == TransferStatus::Interrupted,
          "shutdown must mark an unfinished transfer interrupted");
  }
}

void run_parallelism_checks() {
  TemporaryDirectory temporary;
  const auto downloads = temporary.path / "parallel";
  std::filesystem::create_directories(downloads);

  // Five transfers under the default three-way cap must overlap but never
  // exceed three concurrent backend reads.
  {
    auto backend = std::make_shared<FakeBackend>();
    backend->add_directory("/srv");
    backend->set_read_delay(120);
    std::vector<std::string> sources;
    for (int index = 0; index < 5; ++index) {
      const std::string path = "/srv/file" + std::to_string(index) + ".bin";
      backend->add_file(path, std::string(1024, static_cast<char>('a' + index)));
      sources.push_back(path);
    }
    TransferManager manager(backend);
    std::vector<std::string> ids;
    for (const auto& source : sources) {
      ids.push_back(manager.start_download("srv_1", {source}, downloads, ConflictPolicy::Skip));
    }
    const bool overlapped = wait_until([&] { return backend->live_reads() >= 2; }, 5000);
    for (const auto& id : ids) (void)wait_terminal(manager, id);
    check(overlapped, "parallel transfers must overlap");
    check(backend->peak_reads() <= TransferManager::kMaxParallelTransfers,
          "the parallel worker cap must bound concurrent reads");
    manager.shutdown();
  }

  // A single permit must serialise backend reads.
  {
    auto backend = std::make_shared<FakeBackend>();
    backend->add_directory("/srv");
    backend->set_read_delay(60);
    std::vector<std::string> sources;
    for (int index = 0; index < 3; ++index) {
      const std::string path = "/srv/serial" + std::to_string(index) + ".bin";
      backend->add_file(path, std::string(512, 'z'));
      sources.push_back(path);
    }
    TransferManager manager(backend, 1);
    std::vector<std::string> ids;
    for (const auto& source : sources) {
      ids.push_back(manager.start_download("srv_1", {source}, downloads, ConflictPolicy::Skip));
    }
    for (const auto& id : ids) (void)wait_terminal(manager, id);
    check(backend->peak_reads() == 1, "a single permit must serialise backend reads");
    manager.shutdown();
  }
}

void run_recovery_checks() {
  TemporaryDirectory temporary;
  auto backend = std::make_shared<FakeBackend>();
  backend->add_directory("/srv");
  const std::string remote_stage = "/srv/.target.txt.yukinal-tr_abcdef-1-0.part";
  backend->add_file(remote_stage, "remote residue");
  const auto local_stage = temporary.path / ".target.txt.yukinal-tr_abcdef-0-0.part";
  write_file(local_stage, "local residue");

  TransferSnapshot row = new_transfer_snapshot("tr_abc-def", "srv_1", TransferDirection::Upload);
  row.status = TransferStatus::Running;
  row.total_files = 1;
  auto history = std::make_shared<FakeHistoryStore>();
  history->seed(row, {TransferStagingFile{false, path_to_utf8(local_stage), "", 0},
                      TransferStagingFile{true, remote_stage, "srv_1", 1}});

  TransferManager manager = TransferManager::with_history(backend, history);
  check(!std::filesystem::exists(local_stage),
        "startup recovery must remove validated local staging residue");
  const auto recovered = history->row("tr_abc-def");
  check(recovered.has_value() && recovered->status == TransferStatus::Interrupted,
        "startup recovery must mark an active transfer interrupted");
  const bool pending_note =
      std::any_of(recovered->failures.begin(), recovered->failures.end(),
                  [](const TransferItemFailure& failure) {
                    return failure.message ==
                           "remote staging cleanup is pending an authenticated server session";
                  });
  check(pending_note, "startup recovery must record pending remote staging cleanup");
  check(std::find(recovered->staging_residue.begin(), recovered->staging_residue.end(),
                  ".target.txt.yukinal-tr_abcdef-1-0.part") != recovered->staging_residue.end(),
        "startup recovery must surface remote staging residue");
  check(backend->has(remote_stage) && backend->recovered_removals() == 0,
        "startup recovery must not touch remote staging without a session");
  check(history->staging_of("tr_abc-def").size() == 1 &&
            history->staging_of("tr_abc-def").front().remote,
        "startup recovery must keep the remote staging record pending");

  manager.reconcile_remote_staging();
  check(!backend->has(remote_stage) && backend->recovered_removals() == 1,
        "remote staging reconciliation must remove the owned staging file");
  const auto after = history->row("tr_abc-def");
  check(after.has_value() &&
            std::find(after->staging_residue.begin(), after->staging_residue.end(),
                      ".target.txt.yukinal-tr_abcdef-1-0.part") ==
                after->staging_residue.end(),
        "reconciliation must clear the resolved staging residue");
  const bool still_pending =
      std::any_of(after->failures.begin(), after->failures.end(),
                  [](const TransferItemFailure& failure) {
                    return failure.message ==
                           "remote staging cleanup is pending an authenticated server session";
                  });
  check(!still_pending, "reconciliation must clear the pending cleanup note");
  manager.shutdown();
}

void run_fail_closed_history_check() {
  TemporaryDirectory temporary;
  auto backend = std::make_shared<FakeBackend>();
  backend->add_directory("/srv");
  const auto source = temporary.path / "done.txt";
  write_file(source, "payload");
  auto history = std::make_shared<FakeHistoryStore>();
  history->set_fail_terminal(true);

  Observation observation;
  TransferManager manager = TransferManager::with_history(backend, history);
  const std::size_t token = manager.subscribe(
      [&](const TransferSnapshot& snapshot) { observation.record(snapshot); });
  const std::string id =
      manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Skip);
  const bool failed = wait_until([&] {
    const auto snapshot = observation.get(id);
    return snapshot && snapshot->status == TransferStatus::Failed;
  });
  check(failed, "a failed terminal history write must publish a failed snapshot");
  const auto published = observation.get(id);
  const bool noted =
      std::any_of(published->failures.begin(), published->failures.end(),
                  [](const TransferItemFailure& failure) {
                    return failure.item == "transfer history";
                  });
  check(noted, "a failed terminal history write must record a local failure");
  const auto stored = manager.get(id);
  check(!stored || !is_terminal(stored->status),
        "the durable row must stay non-terminal when the terminal write is rejected");
  manager.unsubscribe(token);
  manager.shutdown();
}

void run_query_checks() {
  TemporaryDirectory temporary;
  auto backend = std::make_shared<FakeBackend>();
  backend->add_directory("/srv");
  auto history = std::make_shared<FakeHistoryStore>();
  const auto source = temporary.path / "q.txt";
  write_file(source, "q");
  TransferManager manager = TransferManager::with_history(backend, history);
  const std::string id =
      manager.start_upload("srv_1", "/srv", {source}, ConflictPolicy::Skip);
  (void)wait_terminal(manager, id);

  const auto listed = manager.list("srv_1", 10);
  check(listed.size() == 1 && listed.front().transfer_id == id &&
            listed.front().status == TransferStatus::Completed,
        "the history list must include the finished transfer");
  check(manager.list("other", 10).empty(), "the history list must filter by server");
  require_transfer_error([&] { (void)manager.list(std::nullopt, 0); },
                         TransferErrorCode::InvalidInput, "a zero limit must be rejected");
  require_transfer_error([&] { (void)manager.list(std::nullopt, 101); },
                         TransferErrorCode::InvalidInput, "an oversized limit must be rejected");
  require_transfer_error([&] { manager.cancel("tr_missing"); }, TransferErrorCode::InvalidInput,
                         "cancelling an unknown transfer must be rejected");
  require_transfer_error(
      [&] { manager.resolve_conflict("tr_missing", 0, ConflictAction{}); },
      TransferErrorCode::InvalidInput,
      "resolving an unknown transfer's conflict must be rejected");
  require_transfer_error(
      [&] { (void)manager.start_upload("srv_1", "/srv", {}, ConflictPolicy::Skip); },
      TransferErrorCode::InvalidInput, "an upload without sources must be rejected");
  require_transfer_error(
      [&] {
        (void)manager.start_download("srv_1", {"/srv/x"}, "relative", ConflictPolicy::Skip);
      },
      TransferErrorCode::InvalidInput, "a relative download directory must be rejected");
  manager.shutdown();
}

// Drives the transfer IPC commands through `IpcRouter` with an injected,
// in-memory manager so the request validation, the manager wiring, the event
// forwarding and the snapshot response shapes are all exercised without a
// database, a server or the SSH core.
void run_ipc_router_transfer_checks() {
  using nlohmann::json;

  TemporaryDirectory temporary;
  auto backend = std::make_shared<FakeBackend>();
  backend->add_directory("/srv");

  const auto source = temporary.path / "hello.txt";
  write_file(source, "hello world");
  const auto downloads = temporary.path / "downloads";
  std::filesystem::create_directories(downloads);

  auto history = std::make_shared<FakeHistoryStore>();
  TransferManager manager = TransferManager::with_history(backend, history);

  IpcRouter router;
  router.set_transfer_manager(std::make_unique<TransferManager>(manager));

  std::mutex events_mutex;
  std::vector<std::pair<std::string, json>> events;
  router.set_event_sink([&](const std::string& name, const json& payload) {
    std::lock_guard<std::mutex> lock(events_mutex);
    events.emplace_back(name, payload);
  });

  const auto upload_registration = router.local_paths().register_paths({source});
  const auto download_registration = router.local_paths().register_paths({downloads});
  check(upload_registration.handles.size() == 1 && download_registration.handles.size() == 1,
        "the IPC test handles should register");
  const auto& upload_handle = upload_registration.handles.front().handle_id;
  const auto& download_handle = download_registration.handles.front().handle_id;

  // Parameter validation happens before the manager is touched.
  check(!router.invoke("file_transfer_upload", {{"serverId", "srv_1"}}).ok,
        "file_transfer_upload should require its full parameter set");
  check(!router.invoke("file_transfer_upload",
                       {{"serverId", "not-a-server"},
                        {"remoteDirectory", "/srv"},
                        {"sourceHandleIds", {upload_handle}}})
             .ok,
        "file_transfer_upload should reject a malformed server id");
  check(!router.invoke("file_transfer_upload",
                       {{"serverId", "srv_1"},
                        {"remoteDirectory", "relative"},
                        {"sourceHandleIds", {upload_handle}}})
             .ok,
        "file_transfer_upload should reject a relative remote directory");
  check(!router.invoke("file_transfer_upload",
                       {{"serverId", "srv_1"},
                        {"remoteDirectory", "/srv"},
                        {"sourceHandleIds", {"short"}}})
             .ok,
        "file_transfer_upload should reject a malformed source handle id");
  check(!router.invoke("file_transfer_upload",
                       {{"serverId", "srv_1"},
                        {"remoteDirectory", "/srv"},
                        {"sourceHandleIds", {"local_path_missing0000000000000000000000000000000000000000"}}})
             .ok,
        "file_transfer_upload should reject an unknown source handle");

  const auto upload = router.invoke("file_transfer_upload",
      {{"serverId", "srv_1"}, {"remoteDirectory", "/srv"}, {"sourceHandleIds", {upload_handle}}});
  check(upload.ok, "file_transfer_upload should start an upload");
  const std::string upload_id = upload.value.at("transfer").at("transferId").get<std::string>();
  check(!upload_id.empty() && upload.value.at("transfer").at("direction") == "upload" &&
            upload.value.at("transfer").at("serverId") == "srv_1",
        "file_transfer_upload should return the new upload snapshot");

  const auto uploaded = wait_terminal(manager, upload_id);
  check(uploaded.status == TransferStatus::Completed &&
            backend->content("/srv/hello.txt") == "hello world",
        "the IPC upload must publish the file through the manager");
  const bool saw_update = wait_until([&] {
    std::lock_guard<std::mutex> lock(events_mutex);
    return std::any_of(events.begin(), events.end(), [&](const auto& entry) {
      return entry.first == "file.transfer_updated" &&
             entry.second.value("transfer", json::object()).value("transferId", std::string()) ==
                 upload_id;
    });
  });
  check(saw_update, "the router must forward file.transfer_updated events");

  // The list and get commands route through the manager's merged view.
  const auto list = router.invoke("file_transfer_list", {{"serverId", "srv_1"}});
  check(list.ok && list.value.at("transfers").is_array() &&
            !list.value.at("transfers").empty(),
        "file_transfer_list should return the transfer history");
  check(!router.invoke("file_transfer_list", {{"unexpected", true}}).ok,
        "file_transfer_list should reject unknown parameters");
  check(!router.invoke("file_transfer_list", {{"serverId", "not-a-server"}}).ok,
        "file_transfer_list should reject a malformed server id");
  const auto fetched = router.invoke("file_transfer_get", {{"transferId", upload_id}});
  check(fetched.ok && fetched.value.at("transfer").at("transferId") == upload_id,
        "file_transfer_get should return the transfer snapshot");
  check(!router.invoke("file_transfer_get", {{"transferId", "tr_missing"}}).ok,
        "file_transfer_get should report an unknown transfer");

  // Download through a registered destination handle.
  backend->add_file("/srv/report.txt", "report");
  const auto download = router.invoke("file_transfer_download",
      {{"serverId", "srv_1"},
       {"remotePaths", {"/srv/report.txt"}},
       {"destinationHandleId", download_handle}});
  check(download.ok && download.value.at("transfer").at("direction") == "download",
        "file_transfer_download should start a download");
  const std::string download_id =
      download.value.at("transfer").at("transferId").get<std::string>();
  const auto downloaded = wait_terminal(manager, download_id);
  check(downloaded.status == TransferStatus::Completed &&
            read_file(downloads / "report.txt") == "report",
        "the IPC download must publish the file through the manager");
  check(!router.invoke("file_transfer_download",
                       {{"serverId", "srv_1"},
                        {"remotePaths", {"relative"}},
                        {"destinationHandleId", download_handle}})
             .ok,
        "file_transfer_download should reject a relative remote path");

  // Conflict resolution through the IPC command.
  const auto conflict_source = temporary.path / "report.txt";
  write_file(conflict_source, "local report");
  const auto conflict_registration = router.local_paths().register_paths({conflict_source});
  check(conflict_registration.handles.size() == 1, "the conflict handle should register");
  const auto conflict = router.invoke("file_transfer_upload",
      {{"serverId", "srv_1"},
       {"remoteDirectory", "/srv"},
       {"sourceHandleIds", {conflict_registration.handles.front().handle_id}}});
  check(conflict.ok, "the conflicting upload should start");
  const std::string conflict_id =
      conflict.value.at("transfer").at("transferId").get<std::string>();
  (void)wait_conflict(manager, conflict_id);

  check(!router.invoke("file_transfer_resolve_conflict",
                       {{"transferId", conflict_id}, {"action", {{"action", "rename"}}}})
             .ok,
        "a rename conflict action must carry exactly a name");
  check(!router.invoke("file_transfer_resolve_conflict",
                       {{"transferId", conflict_id},
                        {"action", {{"action", "skip"}, {"name", "x"}}}})
             .ok,
        "a skip conflict action must not carry extra fields");
  const auto resolved = router.invoke("file_transfer_resolve_conflict",
      {{"transferId", conflict_id}, {"action", {{"action", "overwrite"}}}});
  check(resolved.ok, "file_transfer_resolve_conflict should accept overwrite");
  const auto conflict_done = wait_terminal(manager, conflict_id);
  check(conflict_done.status == TransferStatus::Completed &&
            backend->content("/srv/report.txt") == "local report",
        "the IPC conflict resolution must overwrite the destination");
  check(!router.invoke("file_transfer_resolve_conflict",
                       {{"transferId", upload_id}, {"action", {{"action", "overwrite"}}}})
             .ok,
        "resolving a transfer with no active conflict must be rejected");

  // Cancellation through the IPC command, while the transfer waits on a conflict.
  const auto cancel_source = temporary.path / "cancel.txt";
  write_file(cancel_source, "cancel me");
  backend->add_file("/srv/cancel.txt", "existing");
  const auto cancel_registration = router.local_paths().register_paths({cancel_source});
  check(cancel_registration.handles.size() == 1, "the cancel handle should register");
  const auto started = router.invoke("file_transfer_upload",
      {{"serverId", "srv_1"},
       {"remoteDirectory", "/srv"},
       {"sourceHandleIds", {cancel_registration.handles.front().handle_id}}});
  check(started.ok, "the cancellable upload should start");
  const std::string cancel_id =
      started.value.at("transfer").at("transferId").get<std::string>();
  (void)wait_conflict(manager, cancel_id);
  const auto cancelled = router.invoke("file_transfer_cancel", {{"transferId", cancel_id}});
  check(cancelled.ok, "file_transfer_cancel should cancel a waiting transfer");
  const auto cancel_done = wait_terminal(manager, cancel_id);
  check(cancel_done.status == TransferStatus::Cancelled,
        "a cancelled transfer must report cancelled");
  check(!router.invoke("file_transfer_cancel", {{"transferId", "tr_missing"}}).ok,
        "file_transfer_cancel should report an unknown transfer");
}

void run_remote_drag_path_checks() {
  for (const char* path : {"relative.txt", "/srv/../secret", "/srv/./file",
                           "/srv\\secret", "/srv/", "", "/a/b/.."}) {
    check(!valid_remote_drag_path(path), "an unsafe remote drag path should be rejected");
  }
  check(valid_remote_drag_path("/srv/app/config.yml"),
        "a safe absolute remote drag path should be accepted");
  check(remote_drag_file_name("/srv/app/config.yml") == std::optional<std::string>("config.yml"),
        "the remote drag name should be the last path segment");
  check(!remote_drag_file_name("/srv/app/").has_value() &&
            !remote_drag_file_name("/").has_value(),
        "a path without a name segment has no file name");

  const auto id = new_remote_drag_id();
  const bool hex =
      std::all_of(id.begin() + 12, id.end(), [](char value) {
        return (value >= '0' && value <= '9') || (value >= 'a' && value <= 'f');
      });
  check(id.size() == 76 && id.rfind("remote_drag_", 0) == 0 && hex,
        "a fresh drag id should be remote_drag_ plus 64 lowercase hex digits");
}

void run_remote_drag_store_checks() {
  TemporaryDirectory temporary;

  // `open` rejects a non-absolute root.
  bool relative_rejected = false;
  try {
    (void)RemoteDragStore::open(std::filesystem::path("relative-root"));
  } catch (const std::exception&) {
    relative_rejected = true;
  }
  check(relative_rejected, "the drag store should reject a relative root");

  // `open` sweeps only stale drag directories and stage files.
  const auto sweep_root = temporary.path / "sweep-root";
  std::filesystem::create_directories(sweep_root / "yukinal-remote-drag-stale");
  std::filesystem::create_directories(sweep_root / "keep-me");
  write_file(sweep_root / "yukinal-remote-drag-orphan.txt", "orphan");
  auto swept = RemoteDragStore::open(sweep_root);
  check(!std::filesystem::exists(sweep_root / "yukinal-remote-drag-stale") &&
            !std::filesystem::exists(sweep_root / "yukinal-remote-drag-orphan.txt") &&
            std::filesystem::exists(sweep_root / "keep-me"),
        "the drag store should sweep only stale drag entries on open");
  check(swept->root() == sweep_root, "the drag store should report its root");

  // A staged drag owns its directory and removes it on destruction.
  const auto owned = swept->create_stage();
  check(std::filesystem::is_directory(owned), "create_stage should create a directory");
  {
    StagedRemoteDrag drag(owned, {});
    check(drag.owns_directory(), "a staged drag should own its directory");
  }
  check(!std::filesystem::exists(owned),
        "a staged drag should remove its directory on destruction");

  // `keep()` leaves the directory for the next startup sweep.
  const auto kept = swept->create_stage();
  {
    StagedRemoteDrag drag(kept, {});
    drag.keep();
  }
  check(std::filesystem::exists(kept), "keep() should leave the directory for the next sweep");

  // A ticket round-trips and is consumed exactly once.
  const auto stage = swept->create_stage();
  const auto staged = stage / "note.txt";
  write_file(staged, "drag me");
  const auto prepared = swept->insert(stage, staged, "note.txt", 7);
  check(prepared.drag_id.size() == 76 && prepared.drag_id.rfind("remote_drag_", 0) == 0 &&
            prepared.name == "note.txt" && prepared.size == 7,
        "insert should return the ticket, name and size");
  check(swept->size() == 1, "the store should hold one prepared drag");
  auto taken = swept->take(prepared.drag_id);
  check(taken.files().size() == 1 && taken.files().front() == staged,
        "take should hand back the staged file");
  check(swept->size() == 0, "take should consume the ticket");
  taken.release();
  check(std::filesystem::exists(staged), "a released stage should survive");
  bool reuse_rejected = false;
  try {
    (void)swept->take(prepared.drag_id);
  } catch (const std::exception&) {
    reuse_rejected = true;
  }
  check(reuse_rejected, "taking a consumed ticket should fail");

  // Expiry drops the ticket and its directory.
  std::int64_t now = 1'000;
  RemoteDragStore clocked(temporary.path / "clocked-root",
                          [&now]() { return now; });
  std::filesystem::create_directories(clocked.root());
  const auto expiring = clocked.create_stage();
  write_file(expiring / "f", "x");
  const auto ticket = clocked.insert(expiring, expiring / "f", "f", 1);
  now += RemoteDragStore::kTicketTtlMillis + 1;
  bool expired_rejected = false;
  try {
    (void)clocked.take(ticket.drag_id);
  } catch (const std::exception&) {
    expired_rejected = true;
  }
  check(expired_rejected, "an expired drag ticket should be rejected");
  check(!std::filesystem::exists(expiring), "an expired drag should drop its directory");

  // The prepared-count cap is enforced.
  std::int64_t frozen = 5'000;
  RemoteDragStore capped(temporary.path / "capped-root", [&frozen]() { return frozen; });
  std::filesystem::create_directories(capped.root());
  const auto cap_stage = capped.create_stage();
  write_file(cap_stage / "f", "x");
  for (std::size_t index = 0; index < RemoteDragStore::kMaxPreparedCount; ++index) {
    (void)capped.insert(cap_stage, cap_stage / "f", "f", 1);
  }
  bool count_rejected = false;
  try {
    (void)capped.insert(cap_stage, cap_stage / "f", "f", 1);
  } catch (const std::exception&) {
    count_rejected = true;
  }
  check(count_rejected, "the store should cap the number of prepared drags");

  // The prepared-byte cap is enforced.
  RemoteDragStore byte_capped(temporary.path / "byte-capped-root",
                              [&frozen]() { return frozen; });
  std::filesystem::create_directories(byte_capped.root());
  const auto byte_stage = byte_capped.create_stage();
  write_file(byte_stage / "f", "x");
  (void)byte_capped.insert(byte_stage, byte_stage / "f", "f",
                           RemoteDragStore::kMaxPreparedBytes);
  bool bytes_rejected = false;
  try {
    (void)byte_capped.insert(byte_stage, byte_stage / "f", "f", 1);
  } catch (const std::exception&) {
    bytes_rejected = true;
  }
  check(bytes_rejected, "the store should cap the prepared bytes");
}

void run_ipc_router_remote_drag_checks() {
  using nlohmann::json;

  TemporaryDirectory temporary;
  auto backend = std::make_shared<FakeBackend>();
  backend->add_directory("/srv");
  const std::string body = "report body";
  backend->add_file("/srv/report.txt", body);

  auto history = std::make_shared<FakeHistoryStore>();
  TransferManager manager = TransferManager::with_history(backend, history);

  auto probe = std::make_shared<FakeRemoteDragProbe>();
  probe->set("/srv/report.txt", RemoteStat{RemoteEntryKind::File, body.size(), std::nullopt});
  probe->set("/srv/folder", RemoteStat{RemoteEntryKind::Directory, 0, std::nullopt});
  probe->set("/srv/link", RemoteStat{RemoteEntryKind::Symlink, 0, std::nullopt});
  probe->set("/srv/huge.bin",
             RemoteStat{RemoteEntryKind::File, RemoteDragStore::kMaxFileBytes + 1, std::nullopt});

  auto store = RemoteDragStore::open(temporary.path / "native-remote-drag");
  RemoteDragStore* store_handle = store.get();

  IpcRouter router;
  router.set_transfer_manager(std::make_unique<TransferManager>(manager));
  router.set_remote_drag_probe(probe);
  router.set_remote_drag_store(std::move(store));

  const auto invoke = [&router](const char* path) {
    return router.invoke("file_prepare_remote_drag",
                         {{"serverId", "srv_1"}, {"path", path}});
  };

  check(!router.invoke("file_prepare_remote_drag", {{"serverId", "srv_1"}}).ok,
        "file_prepare_remote_drag should require its full parameter set");
  check(!router.invoke("file_prepare_remote_drag",
                       {{"serverId", "srv_1"}, {"path", "/srv/report.txt"}, {"extra", true}})
             .ok,
        "file_prepare_remote_drag should reject unknown parameters");
  check(!router.invoke("file_prepare_remote_drag",
                       {{"serverId", "not-a-server"}, {"path", "/srv/report.txt"}})
             .ok,
        "file_prepare_remote_drag should reject a malformed server id");
  check(invoke("relative").value.at("code") == "INVALID_PARAMS",
        "file_prepare_remote_drag should reject an unsafe path");

  check(invoke("/srv/missing.txt").value.at("code") == "NOT_FOUND",
        "file_prepare_remote_drag should report a missing remote file");
  check(!invoke("/srv/folder").ok, "file_prepare_remote_drag should reject a directory");
  check(!invoke("/srv/link").ok, "file_prepare_remote_drag should reject a symlink");
  check(!invoke("/srv/huge.bin").ok, "file_prepare_remote_drag should reject an oversized file");

  probe->set_error(TransferErrorCode::Unsupported, "target is not supported");
  check(!invoke("/srv/report.txt").ok,
        "file_prepare_remote_drag should fail closed on an unsupported target");
  probe->clear_error();

  const auto prepared = invoke("/srv/report.txt");
  check(prepared.ok, "file_prepare_remote_drag should prepare a regular remote file");
  const auto drag_id = prepared.value.at("dragId").get<std::string>();
  check(drag_id.size() == 76 && drag_id.rfind("remote_drag_", 0) == 0,
        "file_prepare_remote_drag should return a remote_drag_<64 hex> ticket");
  check(prepared.value.at("name") == "report.txt" &&
            prepared.value.at("size").get<std::uint64_t>() == body.size(),
        "file_prepare_remote_drag should echo the file name and size");

  {
    auto staged = store_handle->take(drag_id);
    check(staged.files().size() == 1 && std::filesystem::exists(staged.files().front()),
          "the prepared drag should stage the downloaded bytes behind the ticket");
    check(read_file(staged.files().front()) == body,
          "the staged drag should hold the exact downloaded bytes");
    const auto canonical_stage = std::filesystem::canonical(staged.files().front());
    const auto canonical_root = std::filesystem::canonical(store_handle->root());
    check(canonical_stage.string().rfind(canonical_root.string(), 0) == 0,
          "the staged file should stay inside the private drag root");
  }
  bool reused = false;
  try {
    (void)store_handle->take(drag_id);
  } catch (const std::exception&) {
    reused = true;
  }
  check(reused, "a consumed drag ticket should not be reusable");

  // A hung download is bounded: the preparation cancels it, observes the stop and
  // cleans up rather than racing the stage.
  backend->set_read_delay(400);
  router.set_remote_drag_wait_bounds(std::chrono::milliseconds(30), std::chrono::seconds(5));
  const auto timed_out = invoke("/srv/report.txt");
  check(!timed_out.ok, "a drag preparation should bound a hung download");
  backend->set_read_delay(0);
  check(store_handle->size() == 0, "a timed-out preparation should not leave a ticket");
  std::size_t leftover = 0;
  for (const auto& entry : std::filesystem::directory_iterator(store_handle->root())) {
    if (entry.path().filename().string().rfind(RemoteDragStore::kStagePrefix, 0) == 0) {
      ++leftover;
    }
  }
  check(leftover == 0, "a timed-out preparation should remove its staging directory");
}

}  // namespace

int main() {
  run_upload_and_download_checks();
  run_conflict_checks();
  run_cancellation_checks();
  run_parallelism_checks();
  run_recovery_checks();
  run_fail_closed_history_check();
  run_query_checks();
  run_ipc_router_transfer_checks();
  run_remote_drag_path_checks();
  run_remote_drag_store_checks();
  run_ipc_router_remote_drag_checks();

  std::cout << "transfer manager tests: green\n";
  return 0;
}
