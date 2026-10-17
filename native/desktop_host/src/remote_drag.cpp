#include "yukinal/desktop_host/remote_drag.hpp"

#include "yukinal/desktop_host/sftp_transfer_backend.hpp"
#include "yukinal/desktop_host/transfer_types.hpp"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <random>
#include <stdexcept>
#include <utility>

namespace yukinal::desktop_host {
namespace {

std::int64_t now_millis() { return static_cast<std::int64_t>(epoch_millis()); }

std::vector<std::filesystem::path> staged_files(const std::filesystem::path& file) {
  if (file.empty()) return {};
  return {file};
}

// The production probe: it reuses the SFTP remote ops the transfer backend drives,
// so an unsupported target fails with `TransferError(Unsupported)` before any work.
class SftpRemoteDragProbe : public RemoteDragProbe {
 public:
  explicit SftpRemoteDragProbe(std::shared_ptr<SftpRemoteOps> ops) : ops_(std::move(ops)) {}

  std::optional<RemoteStat> lstat(const std::string& server_id,
                                  const std::string& path) override {
    ops_->ensure_supported(server_id);
    return ops_->lstat(server_id, path);
  }

 private:
  std::shared_ptr<SftpRemoteOps> ops_;
};

}  // namespace

std::shared_ptr<RemoteDragProbe> make_ssh_service_remote_drag_probe(
    DesktopDatabase* database, const CredentialStore* credentials) {
  auto ops = make_ssh_service_sftp_remote_ops(database, credentials);
  if (!ops) return nullptr;
  return std::make_shared<SftpRemoteDragProbe>(std::move(ops));
}

// ── Path helpers ───────────────────────────────────────────────────────────────

bool valid_remote_drag_path(const std::string& path) {
  if (path.empty() || path.size() > 4096 || path.front() != '/') return false;
  if (path.back() == '/') return false;
  if (path.find('\\') != std::string::npos) return false;
  if (path.find('\0') != std::string::npos) return false;
  std::size_t start = 0;
  while (start <= path.size()) {
    const auto end = path.find('/', start);
    const auto segment =
        path.substr(start, end == std::string::npos ? std::string::npos : end - start);
    if (segment == "." || segment == "..") return false;
    if (end == std::string::npos) break;
    start = end + 1;
  }
  return true;
}

std::optional<std::string> remote_drag_file_name(const std::string& path) {
  const auto slash = path.rfind('/');
  const auto name = slash == std::string::npos ? path : path.substr(slash + 1);
  if (name.empty() || name == "." || name == "..") return std::nullopt;
  return name;
}

std::string new_remote_drag_id() {
  std::random_device device;
  static constexpr char kHex[] = "0123456789abcdef";
  std::string id = "remote_drag_";
  id.reserve(12 + 64);
  for (int index = 0; index < 32; ++index) {
    const auto byte = static_cast<unsigned>(device() & 0xFFu);
    id.push_back(kHex[byte >> 4]);
    id.push_back(kHex[byte & 0x0Fu]);
  }
  return id;
}

bool is_remote_drag_id(const std::string& id) {
  constexpr std::size_t kPrefixLength = 12;  // "remote_drag_"
  if (id.size() != kPrefixLength + 64) return false;
  if (id.compare(0, kPrefixLength, "remote_drag_") != 0) return false;
  for (std::size_t index = kPrefixLength; index < id.size(); ++index) {
    const char value = id[index];
    if (!((value >= '0' && value <= '9') || (value >= 'a' && value <= 'f'))) return false;
  }
  return true;
}

std::shared_ptr<NativeFileDrag> make_native_file_drag() {
  // The Windows `IDropSource`/`DoDragDrop` drag and the other platforms' drags are
  // still to come. Until then the command fails closed with a clear error rather
  // than silently doing nothing.
  return nullptr;
}

// ── StagedRemoteDrag ───────────────────────────────────────────────────────────

StagedRemoteDrag::StagedRemoteDrag(std::filesystem::path directory,
                                   std::vector<std::filesystem::path> files)
    : directory_(std::move(directory)), files_(std::move(files)), owns_directory_(true) {}

StagedRemoteDrag::~StagedRemoteDrag() { remove_now(); }

StagedRemoteDrag::StagedRemoteDrag(StagedRemoteDrag&& other) noexcept
    : directory_(std::move(other.directory_)),
      files_(std::move(other.files_)),
      owns_directory_(other.owns_directory_),
      keep_(other.keep_) {
  other.owns_directory_ = false;
}

StagedRemoteDrag& StagedRemoteDrag::operator=(StagedRemoteDrag&& other) noexcept {
  if (this == &other) return *this;
  remove_now();
  directory_ = std::move(other.directory_);
  files_ = std::move(other.files_);
  owns_directory_ = other.owns_directory_;
  keep_ = other.keep_;
  other.owns_directory_ = false;
  return *this;
}

void StagedRemoteDrag::remove_now() {
  if (!owns_directory_ || keep_ || directory_.empty()) {
    std::fprintf(stderr,
                 "[diag] remove_now skipped owns=%d keep=%d empty=%d dir=%s\n",
                 owns_directory_ ? 1 : 0, keep_ ? 1 : 0, directory_.empty() ? 1 : 0,
                 directory_.string().c_str());
    return;
  }
  std::error_code error;
  std::uintmax_t removed = 0;
  try {
    removed = std::filesystem::remove_all(directory_, error);
  } catch (const std::exception& exception) {
    std::fprintf(stderr, "[diag] remove_now threw dir=%s what=%s\n",
                 directory_.string().c_str(), exception.what());
    return;
  }
  std::fprintf(stderr, "[diag] remove_now dir=%s removed=%llu ec=%s\n",
               directory_.string().c_str(), static_cast<unsigned long long>(removed),
               error ? error.message().c_str() : "none");
}

// ── RemoteDragStore ────────────────────────────────────────────────────────────

RemoteDragStore::RemoteDragStore(std::filesystem::path root, Clock clock)
    : root_(std::move(root)), clock_(std::move(clock)) {
  if (!clock_) clock_ = &now_millis;
}

RemoteDragStore::~RemoteDragStore() {
  std::lock_guard lock(mutex_);
  for (auto& entry : prepared_) remove_directory(entry.second.directory);
  prepared_.clear();
}

std::unique_ptr<RemoteDragStore> RemoteDragStore::open(std::filesystem::path root) {
  if (root.empty() || !root.is_absolute()) {
    throw std::runtime_error("native drag staging root must be an absolute path");
  }
  std::error_code error;
  std::filesystem::create_directories(root, error);
  if (error) {
    throw std::runtime_error("could not create native drag staging root: " + error.message());
  }
  const auto status = std::filesystem::symlink_status(root, error);
  if (error || std::filesystem::is_symlink(status) || !std::filesystem::is_directory(status)) {
    throw std::runtime_error("native drag staging location is not a private directory");
  }
  auto store = std::make_unique<RemoteDragStore>(std::move(root), &now_millis);
  store->sweep_stale_directories();
  return store;
}

void RemoteDragStore::sweep_stale_directories() {
  std::error_code error;
  if (!std::filesystem::is_directory(root_, error)) return;
  for (const auto& entry : std::filesystem::directory_iterator(
           root_, std::filesystem::directory_options::skip_permission_denied, error)) {
    if (error) break;
    const auto name = entry.path().filename().string();
    if (name.rfind(kStagePrefix, 0) != 0) continue;
    std::error_code status_error;
    const auto status = entry.symlink_status(status_error);
    if (status_error) continue;
    std::error_code remove_error;
    if (std::filesystem::is_directory(status)) {
      std::filesystem::remove_all(entry.path(), remove_error);
    } else {
      std::filesystem::remove(entry.path(), remove_error);
    }
  }
}

void RemoteDragStore::remove_directory(const std::filesystem::path& directory) {
  if (directory.empty()) return;
  std::error_code error;
  std::filesystem::remove_all(directory, error);
}

std::filesystem::path RemoteDragStore::create_stage() {
  std::error_code error;
  if (!std::filesystem::is_directory(root_, error)) {
    throw std::runtime_error("native drag staging is unavailable");
  }
  std::random_device device;
  for (int attempt = 0; attempt < 8; ++attempt) {
    auto suffix = std::to_string(device());
    for (int index = 0; index < 4; ++index) {
      suffix.push_back("0123456789abcdef"[device() % 16]);
    }
    const auto stage = root_ / (std::string(kStagePrefix) + suffix);
    std::error_code create_error;
    if (std::filesystem::create_directory(stage, create_error)) return stage;
    if (create_error && create_error != std::errc::file_exists) {
      throw std::runtime_error("could not create native drag staging directory");
    }
  }
  throw std::runtime_error("could not create native drag staging directory");
}

void RemoteDragStore::discard_expired() {
  std::lock_guard lock(mutex_);
  const auto now = clock_();
  for (auto entry = prepared_.begin(); entry != prepared_.end();) {
    if (entry->second.expires_at_millis > now) {
      ++entry;
      continue;
    }
    remove_directory(entry->second.directory);
    entry = prepared_.erase(entry);
  }
}

PreparedRemoteDrag RemoteDragStore::insert(std::filesystem::path stage_directory,
                                           std::filesystem::path staged_file, std::string name,
                                           std::uint64_t size) {
  std::lock_guard lock(mutex_);
  const auto now = clock_();
  for (auto entry = prepared_.begin(); entry != prepared_.end();) {
    if (entry->second.expires_at_millis > now) {
      ++entry;
      continue;
    }
    remove_directory(entry->second.directory);
    entry = prepared_.erase(entry);
  }
  if (prepared_.size() >= kMaxPreparedCount) {
    throw std::runtime_error("too many remote files are prepared for drag-out; retry shortly");
  }
  std::uint64_t total = 0;
  for (const auto& entry : prepared_) total += entry.second.size;
  if (total > kMaxPreparedBytes || size > kMaxPreparedBytes - total) {
    throw std::runtime_error("prepared remote drag files exceed the 16 GiB staging limit");
  }
  const std::string drag_id = new_remote_drag_id();
  Entry entry;
  entry.directory = std::move(stage_directory);
  entry.file = std::move(staged_file);
  entry.name = name;
  entry.size = size;
  entry.expires_at_millis = now + kTicketTtlMillis;
  prepared_.emplace(drag_id, std::move(entry));
  return PreparedRemoteDrag{drag_id, std::move(name), size};
}

StagedRemoteDrag RemoteDragStore::take(const std::string& drag_id) {
  std::lock_guard lock(mutex_);
  const auto now = clock_();
  for (auto entry = prepared_.begin(); entry != prepared_.end();) {
    if (entry->second.expires_at_millis > now) {
      ++entry;
      continue;
    }
    remove_directory(entry->second.directory);
    entry = prepared_.erase(entry);
  }
  const auto found = prepared_.find(drag_id);
  if (found == prepared_.end()) {
    throw std::runtime_error("remote drag ticket expired or was already used");
  }
  auto entry = std::move(found->second);
  prepared_.erase(found);
  return StagedRemoteDrag(std::move(entry.directory), staged_files(std::move(entry.file)));
}

std::size_t RemoteDragStore::size() {
  std::lock_guard lock(mutex_);
  return prepared_.size();
}

}  // namespace yukinal::desktop_host
