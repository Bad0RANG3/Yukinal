#include "yukinal/desktop_host/host_trust.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <fstream>
#include <sstream>
#include <stdexcept>
#include <thread>
#include <vector>

#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

namespace yukinal::desktop_host {
namespace {

constexpr const char* kHeader = "# yukinal known_hosts v1 — do not hand-edit lightly\n";

std::string path_as_utf8(const std::filesystem::path& path) {
  const auto encoded = path.u8string();
  return {encoded.begin(), encoded.end()};
}

bool is_absolute_port(const std::string& value, std::uint16_t& port) {
  if (value.empty() || value.size() > 5) return false;
  unsigned int parsed = 0;
  for (const auto character : value) {
    if (character < '0' || character > '9') return false;
    parsed = parsed * 10 + static_cast<unsigned int>(character - '0');
  }
  if (parsed == 0 || parsed > 65535) return false;
  port = static_cast<std::uint16_t>(parsed);
  return true;
}

void parse_line(const std::string& line, std::string& host, std::uint16_t& port,
                std::string& fingerprint) {
  const auto split = [&line](std::size_t begin) {
    const auto end = line.find(':', begin);
    return std::make_pair(line.substr(begin, end - begin),
                          end == std::string::npos ? line.size() : end + 1);
  };
  if (line.rfind("v1:", 0) != 0) throw std::runtime_error("Malformed known_hosts line: " + line);
  auto [host_value, after_version] = split(3);
  auto [port_value, after_host] = split(after_version);
  const auto fingerprint_value = line.substr(after_host);
  if (host_value.empty() || !is_absolute_port(port_value, port) ||
      fingerprint_value.rfind("SHA256:", 0) != 0 || fingerprint_value.size() <= 7) {
    throw std::runtime_error("Malformed known_hosts line: " + line);
  }
  host = std::move(host_value);
  fingerprint = fingerprint_value;
}

std::vector<std::string> split_lines(const std::string& raw) {
  std::vector<std::string> lines;
  std::istringstream stream(raw);
  std::string line;
  while (std::getline(stream, line)) lines.push_back(std::move(line));
  return lines;
}

std::string trim(const std::string& value) {
  const auto first = value.find_first_not_of(" \t\r\n");
  if (first == std::string::npos) return {};
  const auto last = value.find_last_not_of(" \t\r\n");
  return value.substr(first, last - first + 1);
}

// Replace `path` with `contents` atomically: write a sibling temp file, then rename
// over the destination. Windows can return a sharing violation while another process
// is replacing the same file, so the rename is retried.
void write_atomic(const std::filesystem::path& path, const std::string& contents) {
  const auto parent = path.parent_path().empty() ? std::filesystem::path(".") : path.parent_path();
  std::error_code error;
  std::filesystem::create_directories(parent, error);
  if (error) throw std::runtime_error("Could not create known_hosts directory: " + error.message());

  static std::atomic<std::uint64_t> counter{1};
  const auto temporary =
      parent / ("." + path.filename().string() + ".tmp-" + std::to_string(counter.fetch_add(1)));
  {
    std::ofstream output(temporary, std::ios::binary | std::ios::trunc);
    if (!output) throw std::runtime_error("Could not open known_hosts temporary file");
    output << contents;
    output.flush();
    if (!output) throw std::runtime_error("Could not write known_hosts temporary file");
  }

  std::chrono::milliseconds delay(1);
  for (int attempt = 0; attempt < 8; ++attempt) {
#ifdef _WIN32
    if (MoveFileExW(temporary.wstring().c_str(), path.wstring().c_str(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
      return;
    }
    const auto failure = GetLastError();
    const bool retryable = failure == ERROR_ACCESS_DENIED || failure == ERROR_SHARING_VIOLATION ||
                           failure == ERROR_LOCK_VIOLATION;
#else
    std::error_code rename_error;
    std::filesystem::rename(temporary, path, rename_error);
    if (!rename_error) return;
    const bool retryable = rename_error.value() == EACCES || rename_error.value() == EBUSY;
#endif
    if (!retryable || attempt == 7) {
      std::error_code ignored;
      std::filesystem::remove(temporary, ignored);
      throw std::runtime_error("Could not replace known_hosts atomically");
    }
    std::this_thread::sleep_for(delay);
    delay *= 2;
  }
}

}  // namespace

HostTrustStore HostTrustStore::load(const std::filesystem::path& path) {
  HostTrustStore store;
  std::error_code error;
  if (!std::filesystem::exists(path, error)) {
    if (error) throw std::runtime_error("Could not inspect known_hosts");
    store.path_ = path;
    return store;
  }
  std::ifstream input(path, std::ios::binary);
  if (!input) throw std::runtime_error("Could not read known_hosts at " + path_as_utf8(path));
  std::ostringstream buffer;
  buffer << input.rdbuf();
  for (const auto& candidate : split_lines(buffer.str())) {
    const auto line = trim(candidate);
    if (line.empty() || line.front() == '#') continue;
    std::string host;
    std::uint16_t port = 0;
    std::string fingerprint;
    parse_line(line, host, port, fingerprint);
    store.entries_[{host, port}] = std::move(fingerprint);
  }
  store.path_ = path;
  return store;
}

std::optional<std::string> HostTrustStore::pinned_fingerprint(const std::string& host,
                                                             std::uint16_t port) const {
  const auto found = entries_.find({host, port});
  return found == entries_.end() ? std::nullopt : std::make_optional(found->second);
}

HostKeyCheck HostTrustStore::check(const std::string& host, std::uint16_t port,
                                   const std::string& presented) const {
  const auto found = entries_.find({host, port});
  if (found == entries_.end()) return {};
  if (found->second == presented) return {HostKeyComparison::Matches, found->second};
  return {HostKeyComparison::Mismatch, found->second};
}

void HostTrustStore::pin(const std::string& host, std::uint16_t port,
                         const std::string& fingerprint) {
  const auto key = std::make_pair(host, port);
  const auto previous = entries_.find(key);
  const std::optional<std::string> old_value =
      previous == entries_.end() ? std::nullopt : std::make_optional(previous->second);
  entries_[key] = fingerprint;
  if (!path_) return;
  try {
    save();
  } catch (...) {
    if (old_value) entries_[key] = *old_value;
    else entries_.erase(key);
    throw;
  }
}

HostKeyForgetOutcome HostTrustStore::forget(const std::string& host, std::uint16_t port) {
  const auto key = std::make_pair(host, port);
  const auto found = entries_.find(key);
  if (found == entries_.end()) return HostKeyForgetOutcome::NothingPinned;
  const auto fingerprint = found->second;
  entries_.erase(found);
  if (path_) {
    try {
      save();
    } catch (...) {
      entries_[key] = fingerprint;
      throw;
    }
  }
  return HostKeyForgetOutcome::Removed;
}

void HostTrustStore::save() const {
  std::ostringstream output;
  output << kHeader;
  for (const auto& [key, fingerprint] : entries_) {
    output << "v1:" << key.first << ':' << key.second << ':' << fingerprint << '\n';
  }
  write_atomic(*path_, output.str());
}

}  // namespace yukinal::desktop_host
