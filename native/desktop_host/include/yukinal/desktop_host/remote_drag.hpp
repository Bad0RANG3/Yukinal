#pragma once

#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

#include "yukinal/desktop_host/transfer_backend.hpp"

namespace yukinal::desktop_host {

class DesktopDatabase;
class CredentialStore;

// A typed port of the retired Rust `crate::commands::files::drag_out` module: the
// host-private staging that prepares a remote file for a native OS drag-out. Remote
// bytes are downloaded into a short-lived private directory; the UI receives only an
// opaque one-time ticket, and starting the native drag consumes it while keeping the
// directory alive until the OS reports the drag ended. Absolute local paths never
// cross IPC.
//
// `RemoteDragProbe` is the small read-only remote seam the preparation needs for its
// size/type pre-check, separated from the full SFTP adapter so the preparation is
// unit-testable without a server.

class RemoteDragProbe {
 public:
  virtual ~RemoteDragProbe() = default;

  // A no-follow stat, or `std::nullopt` when the path is absent. Throws
  // `TransferError(Unsupported)` when the target cannot be served by the C++ SSH
  // core and `TransferError(Remote)` on a remote failure.
  virtual std::optional<RemoteStat> lstat(const std::string& server_id,
                                          const std::string& path) = 0;
};

// The production probe over the SFTP remote ops. Returns nullptr when this build
// has no SSH core.
[[nodiscard]] std::shared_ptr<RemoteDragProbe> make_ssh_service_remote_drag_probe(
    DesktopDatabase* database, const CredentialStore* credentials);

// The exact response contract of `file_prepare_remote_drag`.
struct PreparedRemoteDrag {
  std::string drag_id;
  std::string name;
  std::uint64_t size = 0;
};

// Owns one prepared drag's private staging directory until the OS reports the drag
// ended. The directory (and the staged file inside it) is removed on destruction
// unless `keep()` was called.
class StagedRemoteDrag {
 public:
  StagedRemoteDrag() = default;
  StagedRemoteDrag(std::filesystem::path directory, std::vector<std::filesystem::path> files);
  ~StagedRemoteDrag();

  StagedRemoteDrag(StagedRemoteDrag&& other) noexcept;
  StagedRemoteDrag& operator=(StagedRemoteDrag&& other) noexcept;
  StagedRemoteDrag(const StagedRemoteDrag&) = delete;
  StagedRemoteDrag& operator=(const StagedRemoteDrag&) = delete;

  [[nodiscard]] const std::filesystem::path& directory() const { return directory_; }
  [[nodiscard]] const std::vector<std::filesystem::path>& files() const { return files_; }
  [[nodiscard]] bool owns_directory() const { return owns_directory_; }

  // Leave the directory on disk (a hung backend must not race directory cleanup)
  // so the next startup sweep can remove it.
  void keep() { keep_ = true; }
  // Give up ownership without deleting it; the caller takes over.
  void release() { owns_directory_ = false; }

 private:
  void remove_now();

  std::filesystem::path directory_;
  std::vector<std::filesystem::path> files_;
  bool owns_directory_ = false;
  bool keep_ = false;
};

// The host-private drag-out staging root. Thread-safe: the preparation may run from
// more than one IPC thread at once.
class RemoteDragStore {
 public:
  static constexpr std::uint64_t kMaxFileBytes = 8ull * 1024 * 1024 * 1024;
  static constexpr std::uint64_t kMaxPreparedBytes = 16ull * 1024 * 1024 * 1024;
  static constexpr std::size_t kMaxPreparedCount = 64;
  static constexpr std::int64_t kTicketTtlMillis = 5LL * 60 * 1000;
  static constexpr const char* kDirectoryName = "native-remote-drag";
  static constexpr const char* kStagePrefix = "yukinal-remote-drag-";

  using Clock = std::function<std::int64_t()>;

  RemoteDragStore(std::filesystem::path root, Clock clock);
  // Removes every still-prepared stage directory. A directory whose native drag is
  // in progress was handed to `take` and is no longer owned here.
  ~RemoteDragStore();

  RemoteDragStore(const RemoteDragStore&) = delete;
  RemoteDragStore& operator=(const RemoteDragStore&) = delete;

  // Creates and validates the root, then sweeps stale stage directories left by an
  // earlier run. Throws `std::runtime_error` when the location is not a private
  // directory.
  [[nodiscard]] static std::unique_ptr<RemoteDragStore> open(std::filesystem::path root);

  // Creates a fresh private staging directory under the root and returns its path.
  // Throws `std::runtime_error` on failure.
  [[nodiscard]] std::filesystem::path create_stage();

  // Registers one staged regular file (already validated by the caller) and returns
  // a one-time ticket. Throws `std::runtime_error` when the store is full or the
  // prepared bytes would exceed the staging cap; the caller still owns and must
  // clean up `stage_directory` when this throws.
  [[nodiscard]] PreparedRemoteDrag insert(std::filesystem::path stage_directory,
                                          std::filesystem::path staged_file,
                                          std::string name, std::uint64_t size);

  // Consumes a ticket and transfers ownership of its staging directory to the
  // returned handle. Throws `std::runtime_error` when the ticket is unknown or
  // expired.
  [[nodiscard]] StagedRemoteDrag take(const std::string& drag_id);

  // Drops every expired ticket and removes its staging directory.
  void discard_expired();

  [[nodiscard]] std::size_t size();
  [[nodiscard]] const std::filesystem::path& root() const { return root_; }

 private:
  struct Entry {
    std::filesystem::path directory;
    std::filesystem::path file;
    std::string name;
    std::uint64_t size = 0;
    std::int64_t expires_at_millis = 0;
  };

  void sweep_stale_directories();
  void remove_directory(const std::filesystem::path& directory);

  std::filesystem::path root_;
  Clock clock_;
  std::mutex mutex_;
  std::unordered_map<std::string, Entry> prepared_;
};

// A safe absolute remote file path for drag-out: absolute, bounded, no backslashes,
// no NUL, no `.`/`..` segments and no trailing slash. Pure, so it is directly
// testable.
[[nodiscard]] bool valid_remote_drag_path(const std::string& path);

// The file name of a safe remote drag path, or `std::nullopt` when it has none.
[[nodiscard]] std::optional<std::string> remote_drag_file_name(const std::string& path);

// A fresh `remote_drag_<64 hex>` ticket id.
[[nodiscard]] std::string new_remote_drag_id();

}  // namespace yukinal::desktop_host
