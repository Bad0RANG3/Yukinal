#pragma once

#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <optional>
#include <string>
#include <vector>

#include "yukinal/desktop_host/transfer_types.hpp"

namespace yukinal::desktop_host {

// A port of the retired Rust `filesystem::transfer::local` seam: the local-file
// operations a transfer needs. Everything is blocking and throws `TransferError`
// on failure, so it can be tested without a server.

// Size plus last-write time, the Windows-visible identity Rust compares. A
// source whose path/bytes change during a transfer is rejected, not silently
// truncated.
struct LocalFingerprint {
  std::uint64_t size = 0;
  std::optional<std::filesystem::file_time_type> modified;

  bool operator==(const LocalFingerprint& other) const {
    return size == other.size && modified == other.modified;
  }
  bool operator!=(const LocalFingerprint& other) const { return !(*this == other); }
};

// A source opened for reading together with its identity at open time.
struct LocalSource {
  std::ifstream stream;
  LocalFingerprint fingerprint;
};

// An exclusive staging file that must be published or removed by the caller.
class LocalStagingFile {
 public:
  LocalStagingFile() = default;
  ~LocalStagingFile();
  LocalStagingFile(const LocalStagingFile&) = delete;
  LocalStagingFile& operator=(const LocalStagingFile&) = delete;
  LocalStagingFile(LocalStagingFile&& other) noexcept;
  LocalStagingFile& operator=(LocalStagingFile&& other) noexcept;

  [[nodiscard]] const std::filesystem::path& path() const { return path_; }
  void write(const void* data, std::size_t bytes);
  void close();

 private:
  friend LocalStagingFile create_local_staging_file(const std::filesystem::path&,
                                                    const std::string&, std::uint64_t);
  std::filesystem::path path_;
  std::FILE* file_ = nullptr;
};

LocalFingerprint local_source_fingerprint(const std::filesystem::path& path);
LocalSource open_local_source(const std::filesystem::path& path);
void verify_local_source_unchanged(const std::filesystem::path& path,
                                   const LocalFingerprint& initial);

std::vector<std::filesystem::path> read_local_directory(const std::filesystem::path& path);

// Creates `.<name>.yukinal-<owner>-<item>-<attempt>.part` exclusively next to
// `target`, trying up to 32 attempt suffixes.
LocalStagingFile create_local_staging_file(const std::filesystem::path& target,
                                           const std::string& transfer_id,
                                           std::uint64_t item_index);

// Atomic same-directory publication. `no_replace` fails if the destination
// appeared; `replace` re-checks the destination fingerprint first.
void publish_local_no_replace(const std::filesystem::path& staging,
                              const std::filesystem::path& destination);
void publish_local_replace(const std::filesystem::path& staging,
                           const std::filesystem::path& destination,
                           const LocalFingerprint& expected);

void remove_local_staging(const std::filesystem::path& path);
// Restart cleanup: only removes a regular file whose basename is owned by
// `transfer_id`; an absent staging file is already clean.
void remove_recovered_local_staging(const std::filesystem::path& path,
                                    const std::string& transfer_id);

void ensure_download_directory(const std::filesystem::path& path);
std::optional<LocalFingerprint> path_fingerprint_if_regular(const std::filesystem::path& path);

}  // namespace yukinal::desktop_host
