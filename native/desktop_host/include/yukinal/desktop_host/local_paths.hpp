#pragma once

#include <cstdint>
#include <filesystem>
#include <functional>
#include <mutex>
#include <nlohmann/json.hpp>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

namespace yukinal::desktop_host {

// The UI only previews a bounded prefix; the bound is shared by the reader and
// the response builder so a truncated read can never be mistaken for a full one.
constexpr std::size_t kMaxFilePreviewBytes = 1024 * 1024;
constexpr std::size_t kMaxLocalHandles = 2048;
constexpr std::size_t kMaxPickedPaths = 256;
constexpr std::int64_t kLocalHandleTtlMillis = 15 * 60 * 1000;

// One opaque, short-lived reference to a path the user picked or dropped in the
// native shell. The absolute path never crosses the IPC boundary.
struct LocalPathHandle {
  std::string handle_id;
  std::string name;
  std::string kind;  // "file" | "directory"
  std::optional<std::uint64_t> size;
};

struct LocalPathRegistration {
  std::vector<LocalPathHandle> handles;
  std::size_t rejected = 0;
};

struct LocalPreviewData {
  std::string name;
  std::uint64_t size = 0;
  std::vector<std::uint8_t> bytes;
};

// Owns the short-lived handle table for `local_path_pick` and `local_file_preview`.
// Every lookup revalidates the path: a symlink or a file that changed type after
// selection is refused, because the handle must not become a way to follow the
// user's path somewhere it did not point when they chose it.
class LocalPathHandles {
 public:
  using Clock = std::function<std::int64_t()>;

  LocalPathHandles();
  explicit LocalPathHandles(Clock clock);

  LocalPathHandles(const LocalPathHandles&) = delete;
  LocalPathHandles& operator=(const LocalPathHandles&) = delete;

  [[nodiscard]] LocalPathRegistration register_paths(
      const std::vector<std::filesystem::path>& paths);

  // Reads at most `max_bytes + 1` bytes so the caller can detect truncation.
  [[nodiscard]] LocalPreviewData read_preview(const std::string& handle_id,
                                              std::size_t max_bytes);

  // Consumes one to `kMaxPickedPaths` upload handles, returning the paths they
  // name and forgetting them so a start request cannot be replayed. Every path is
  // revalidated with no-follow semantics; a repeated, expired, missing, symlinked
  // or retyped handle throws `std::runtime_error`.
  [[nodiscard]] std::vector<std::filesystem::path> take_upload_sources(
      const std::vector<std::string>& handle_ids);

  // Resolves one download-directory handle. The destination stays selected and
  // reusable until its short TTL expires, but every call re-checks that it is
  // still a real directory. Throws `std::runtime_error` on any mismatch.
  [[nodiscard]] std::filesystem::path take_download_directory(
      const std::string& handle_id);

 private:
  void prune_locked(std::int64_t now);

  mutable std::mutex mutex_;
  struct Entry {
    std::filesystem::path path;
    bool directory = false;
    std::int64_t expires_at_millis = 0;
  };
  std::unordered_map<std::string, Entry> entries_;
  Clock clock_;
};

// Builds the shared `FilePreviewResponse` (`{name,size,kind,truncated,text,dataUrl}`)
// from the bounded preview bytes. `max_bytes` is the read cap; a longer body is
// reported as truncated and never decoded as an image.
[[nodiscard]] nlohmann::json make_file_preview(const std::string& name,
                                               std::uint64_t size,
                                               std::vector<std::uint8_t> bytes,
                                               std::size_t max_bytes);

// Opens the platform's native picker for `kind` (uploadFiles, uploadDirectory,
// downloadDirectory) and returns the chosen paths. An empty vector means the user
// cancelled. Throws std::runtime_error when the picker is unavailable.
[[nodiscard]] std::vector<std::filesystem::path> pick_local_paths(
    const std::string& kind);

}  // namespace yukinal::desktop_host
