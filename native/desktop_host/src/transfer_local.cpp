#include "yukinal/desktop_host/transfer_local.hpp"

#include <algorithm>
#include <cerrno>
#include <system_error>

namespace yukinal::desktop_host {
namespace {

LocalFingerprint fingerprint_for(const std::filesystem::path& path) {
  LocalFingerprint result;
  std::error_code error;
  const auto size = std::filesystem::file_size(path, error);
  if (error) {
    throw TransferError(TransferErrorCode::Local,
                        "could not read local file size: " + error.message());
  }
  result.size = size;
  std::error_code time_error;
  const auto modified = std::filesystem::last_write_time(path, time_error);
  if (!time_error) result.modified = modified;
  return result;
}

[[noreturn]] void throw_local(const char* message) {
  throw TransferError(TransferErrorCode::Local, message);
}

// `symlink_status` reports a missing path as `file_type::not_found` without
// setting the error code, so callers must check both.
bool status_missing(const std::filesystem::file_status& status,
                    const std::error_code& error) {
  return status.type() == std::filesystem::file_type::not_found ||
         error == std::errc::no_such_file_or_directory;
}

}  // namespace

LocalStagingFile::~LocalStagingFile() {
  if (file_ != nullptr) std::fclose(file_);
}

LocalStagingFile::LocalStagingFile(LocalStagingFile&& other) noexcept
    : path_(std::move(other.path_)), file_(other.file_) {
  other.file_ = nullptr;
}

LocalStagingFile& LocalStagingFile::operator=(LocalStagingFile&& other) noexcept {
  if (this == &other) return *this;
  if (file_ != nullptr) std::fclose(file_);
  path_ = std::move(other.path_);
  file_ = other.file_;
  other.file_ = nullptr;
  return *this;
}

void LocalStagingFile::write(const void* data, std::size_t bytes) {
  if (file_ == nullptr) throw_local("the local staging file is closed");
  if (bytes == 0) return;
  if (std::fwrite(data, 1, bytes, file_) != bytes) {
    throw_local("could not write to the local staging file");
  }
}

void LocalStagingFile::close() {
  if (file_ == nullptr) return;
  auto* file = file_;
  file_ = nullptr;
  if (std::fflush(file) != 0) {
    std::fclose(file);
    throw_local("could not flush the local staging file");
  }
  if (std::fclose(file) != 0) throw_local("could not close the local staging file");
}

LocalFingerprint local_source_fingerprint(const std::filesystem::path& path) {
  std::error_code error;
  const auto status = std::filesystem::symlink_status(path, error);
  if (status_missing(status, error)) throw_local("could not inspect local source");
  if (error) {
    throw TransferError(TransferErrorCode::Local,
                        "could not inspect local source: " + error.message());
  }
  if (!std::filesystem::is_regular_file(status)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "only regular selected files can be transferred");
  }
  return fingerprint_for(path);
}

LocalSource open_local_source(const std::filesystem::path& path) {
  const auto before = local_source_fingerprint(path);
  LocalSource source;
  source.stream.open(path, std::ios::binary);
  if (!source.stream) throw_local("could not open the local source file");
  source.fingerprint = before;
  return source;
}

void verify_local_source_unchanged(const std::filesystem::path& path,
                                   const LocalFingerprint& initial) {
  std::error_code error;
  const auto status = std::filesystem::symlink_status(path, error);
  if (error && !status_missing(status, error)) {
    throw TransferError(TransferErrorCode::Local,
                        "could not inspect local source: " + error.message());
  }
  if (status_missing(status, error) || !std::filesystem::is_regular_file(status) ||
      fingerprint_for(path) != initial) {
    throw TransferError(TransferErrorCode::SourceChanged,
                        "selected local source changed while it was being uploaded");
  }
}

std::vector<std::filesystem::path> read_local_directory(const std::filesystem::path& path) {
  std::error_code error;
  const auto status = std::filesystem::symlink_status(path, error);
  if (status_missing(status, error)) throw_local("could not inspect local directory");
  if (error) {
    throw TransferError(TransferErrorCode::Local,
                        "could not inspect local directory: " + error.message());
  }
  if (!std::filesystem::is_directory(status)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "only real directories can be traversed");
  }
  std::vector<std::filesystem::path> entries;
  std::error_code iterate_error;
  for (std::filesystem::directory_iterator it(path, iterate_error), end; it != end;
       it.increment(iterate_error)) {
    if (iterate_error) throw_local("could not read the local directory");
    entries.push_back(it->path());
  }
  if (iterate_error) throw_local("could not read the local directory");
  std::sort(entries.begin(), entries.end());
  return entries;
}

LocalStagingFile create_local_staging_file(const std::filesystem::path& target,
                                           const std::string& transfer_id,
                                           std::uint64_t item_index) {
  const auto parent = target.parent_path();
  if (parent.empty()) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "download destination has no parent directory");
  }
  const auto name = target.filename().string();
  if (name.empty()) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "download destination file name is not valid UTF-8");
  }
  std::string owner = transfer_id;
  owner.erase(std::remove(owner.begin(), owner.end(), '-'), owner.end());
  for (std::uint32_t attempt = 0; attempt < 32; ++attempt) {
    auto staging = parent / ("." + name + ".yukinal-" + owner + "-" +
                             std::to_string(item_index) + "-" + std::to_string(attempt) +
                             ".part");
#ifdef _WIN32
    std::FILE* file = _wfopen(staging.c_str(), L"wbx");
#else
    std::FILE* file = std::fopen(staging.c_str(), "wbx");
#endif
    if (file != nullptr) {
      LocalStagingFile result;
      result.path_ = std::move(staging);
      result.file_ = file;
      return result;
    }
    if (errno != EEXIST) throw_local("could not create a local staging file");
  }
  throw TransferError(TransferErrorCode::Unsupported,
                      "could not allocate a unique local staging file after 32 attempts");
}

void publish_local_no_replace(const std::filesystem::path& staging,
                              const std::filesystem::path& destination) {
  std::error_code error;
  std::filesystem::create_hard_link(staging, destination, error);
  if (error) {
    std::error_code exists_error;
    if (std::filesystem::exists(destination, exists_error) && !exists_error) {
      throw TransferError(TransferErrorCode::InvalidInput,
                          "destination appeared during publication");
    }
    throw TransferError(TransferErrorCode::Unsupported,
                        "this filesystem cannot publish a download without replacement");
  }
  std::filesystem::remove(staging, error);
  if (error) throw_local("could not remove the local staging file");
}

void publish_local_replace(const std::filesystem::path& staging,
                           const std::filesystem::path& destination,
                           const LocalFingerprint& expected) {
  std::error_code error;
  const auto status = std::filesystem::symlink_status(destination, error);
  if (error && !status_missing(status, error)) {
    throw_local("could not inspect the overwrite target");
  }
  if (status_missing(status, error) || !std::filesystem::is_regular_file(status)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "overwrite target is no longer a regular file");
  }
  if (fingerprint_for(destination) != expected) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "overwrite target changed while the transfer was staged");
  }
  std::filesystem::rename(staging, destination, error);
  if (error) throw_local("could not replace the download destination");
}

void remove_local_staging(const std::filesystem::path& path) {
  std::error_code error;
  std::filesystem::remove(path, error);
  if (error) throw_local("could not remove the local staging file");
}

void remove_recovered_local_staging(const std::filesystem::path& path,
                                    const std::string& transfer_id) {
  if (!path.is_absolute() ||
      !is_owned_staging_name(path.filename().string(), transfer_id)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "recovered staging path is not owned by this transfer");
  }
  const auto parent = path.parent_path();
  std::error_code error;
  const auto parent_status = std::filesystem::symlink_status(parent, error);
  if (error || !std::filesystem::is_directory(parent_status)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "recovered staging parent is not a real directory");
  }
  const auto file_status = std::filesystem::symlink_status(path, error);
  if (status_missing(file_status, error)) return;
  if (error) throw_local("could not inspect the recovered staging file");
  if (!std::filesystem::is_regular_file(file_status)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "recovered staging entry is not a regular file");
  }
  std::filesystem::remove(path, error);
  if (error) throw_local("could not remove the recovered staging file");
}

void ensure_download_directory(const std::filesystem::path& path) {
  std::error_code error;
  const auto status = std::filesystem::symlink_status(path, error);
  if (status_missing(status, error)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "download destination does not exist");
  }
  if (error) throw_local("could not inspect the download destination");
  if (!std::filesystem::is_directory(status)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "download destination is not a real directory");
  }
}

std::optional<LocalFingerprint> path_fingerprint_if_regular(
    const std::filesystem::path& path) {
  std::error_code error;
  const auto status = std::filesystem::symlink_status(path, error);
  if (status_missing(status, error)) return std::nullopt;
  if (error) throw_local("could not inspect the local destination");
  if (std::filesystem::is_symlink(status)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "destination is a symbolic link or reparse point");
  }
  if (!std::filesystem::is_regular_file(status)) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "destination already exists and is not a regular file");
  }
  return fingerprint_for(path);
}

}  // namespace yukinal::desktop_host
