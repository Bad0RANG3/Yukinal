#include "yukinal/desktop_host/local_paths.hpp"

#include <algorithm>
#include <array>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <random>
#include <stdexcept>
#include <system_error>

#ifdef _WIN32
#include <windows.h>
#include <commdlg.h>
#include <shlobj.h>
#else
#include <cerrno>
#include <fcntl.h>
#include <unistd.h>
#endif

namespace yukinal::desktop_host {
namespace {

std::int64_t system_epoch_millis() {
  const auto now = std::chrono::system_clock::now().time_since_epoch();
  return std::chrono::duration_cast<std::chrono::milliseconds>(now).count();
}

std::string new_handle_id() {
  std::random_device device;
  static constexpr char kHex[] = "0123456789abcdef";
  std::string id = "local_path_";
  id.reserve(11 + 48);
  for (int index = 0; index < 24; ++index) {
    const auto byte = static_cast<unsigned>(device() & 0xFFu);
    id.push_back(kHex[byte >> 4]);
    id.push_back(kHex[byte & 0x0Fu]);
  }
  return id;
}

std::string utf8_path_text(const std::filesystem::path& path) {
  const auto bytes = path.u8string();
  return std::string(bytes.begin(), bytes.end());
}

// Returns the POSIX bytes of a file name, or a stable fallback for paths whose
// last segment is empty (a filesystem root, for example).
std::string file_name_or(const std::filesystem::path& path, const char* fallback) {
  const auto name = path.filename();
  if (name.empty()) return fallback;
  auto text = utf8_path_text(name);
  if (text.empty()) return fallback;
  return text;
}

const char* image_mime(const std::vector<std::uint8_t>& bytes) {
  if (bytes.size() >= 8 && std::memcmp(bytes.data(), "\x89PNG\r\n\x1a\n", 8) == 0) {
    return "image/png";
  }
  if (bytes.size() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF) {
    return "image/jpeg";
  }
  if (bytes.size() >= 6 && (std::memcmp(bytes.data(), "GIF87a", 6) == 0 ||
                            std::memcmp(bytes.data(), "GIF89a", 6) == 0)) {
    return "image/gif";
  }
  if (bytes.size() >= 12 && std::memcmp(bytes.data(), "RIFF", 4) == 0 &&
      std::memcmp(bytes.data() + 8, "WEBP", 4) == 0) {
    return "image/webp";
  }
  return nullptr;
}

std::string base64_encode(const std::vector<std::uint8_t>& data) {
  static constexpr char kAlphabet[] =
      "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  std::string out;
  out.reserve(((data.size() + 2) / 3) * 4);
  std::size_t index = 0;
  while (index + 3 <= data.size()) {
    const std::uint32_t triple = (static_cast<std::uint32_t>(data[index]) << 16) |
                                 (static_cast<std::uint32_t>(data[index + 1]) << 8) |
                                 static_cast<std::uint32_t>(data[index + 2]);
    out.push_back(kAlphabet[(triple >> 18) & 0x3F]);
    out.push_back(kAlphabet[(triple >> 12) & 0x3F]);
    out.push_back(kAlphabet[(triple >> 6) & 0x3F]);
    out.push_back(kAlphabet[triple & 0x3F]);
    index += 3;
  }
  const auto remaining = data.size() - index;
  if (remaining == 1) {
    const std::uint32_t triple = static_cast<std::uint32_t>(data[index]) << 16;
    out.push_back(kAlphabet[(triple >> 18) & 0x3F]);
    out.push_back(kAlphabet[(triple >> 12) & 0x3F]);
    out.push_back('=');
    out.push_back('=');
  } else if (remaining == 2) {
    const std::uint32_t triple = (static_cast<std::uint32_t>(data[index]) << 16) |
                                 (static_cast<std::uint32_t>(data[index + 1]) << 8);
    out.push_back(kAlphabet[(triple >> 18) & 0x3F]);
    out.push_back(kAlphabet[(triple >> 12) & 0x3F]);
    out.push_back(kAlphabet[(triple >> 6) & 0x3F]);
    out.push_back('=');
  }
  return out;
}

enum class Utf8State { kComplete, kIncomplete, kInvalid };

struct Utf8Scan {
  Utf8State state = Utf8State::kComplete;
  std::size_t valid_up_to = 0;
};

// Scans UTF-8 bytes far enough to tell a complete document from one that merely
// ends mid-character (`kIncomplete`, so the readable prefix can still be shown as
// text) and from genuinely invalid bytes (`kInvalid`, shown as binary).
Utf8Scan scan_utf8(const std::vector<std::uint8_t>& bytes) {
  std::size_t index = 0;
  while (index < bytes.size()) {
    const auto byte = bytes[index];
    if (byte < 0x80) {
      ++index;
      continue;
    }
    int length = 0;
    std::uint32_t code_point = 0;
    std::uint32_t minimum = 0;
    if ((byte & 0xE0) == 0xC0) {
      length = 2;
      code_point = byte & 0x1Fu;
      minimum = 0x80;
    } else if ((byte & 0xF0) == 0xE0) {
      length = 3;
      code_point = byte & 0x0Fu;
      minimum = 0x800;
    } else if ((byte & 0xF8) == 0xF0) {
      length = 4;
      code_point = byte & 0x07u;
      minimum = 0x10000;
    } else {
      return {Utf8State::kInvalid, index};
    }
    if (index + static_cast<std::size_t>(length) > bytes.size()) {
      return {Utf8State::kIncomplete, index};
    }
    for (int offset = 1; offset < length; ++offset) {
      const auto continuation = bytes[index + static_cast<std::size_t>(offset)];
      if ((continuation & 0xC0) != 0x80) return {Utf8State::kInvalid, index};
      code_point = (code_point << 6) | (continuation & 0x3Fu);
    }
    if (code_point < minimum || code_point > 0x10FFFF ||
        (code_point >= 0xD800 && code_point <= 0xDFFF)) {
      return {Utf8State::kInvalid, index};
    }
    index += static_cast<std::size_t>(length);
  }
  return {Utf8State::kComplete, bytes.size()};
}

std::vector<std::uint8_t> read_bounded_file(const std::filesystem::path& path,
                                            std::size_t max_bytes) {
  const std::size_t limit = max_bytes + 1;
  std::vector<std::uint8_t> bytes;
  bytes.reserve(std::min<std::size_t>(limit, 64 * 1024));
#ifdef _WIN32
  const HANDLE handle = CreateFileW(path.wstring().c_str(), GENERIC_READ, FILE_SHARE_READ,
                                    nullptr, OPEN_EXISTING,
                                    FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
                                    nullptr);
  if (handle == INVALID_HANDLE_VALUE) {
    throw std::runtime_error("the selected local file could not be opened");
  }
  BY_HANDLE_FILE_INFORMATION info{};
  if (!GetFileInformationByHandle(handle, &info)) {
    CloseHandle(handle);
    throw std::runtime_error("the selected local file could not be inspected");
  }
  if ((info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
    CloseHandle(handle);
    throw std::runtime_error("selected local file is no longer a regular file");
  }
  std::array<std::uint8_t, 64 * 1024> buffer{};
  while (bytes.size() < limit) {
    const auto remaining = static_cast<DWORD>(
        std::min<std::size_t>(buffer.size(), limit - bytes.size()));
    DWORD read = 0;
    if (!ReadFile(handle, buffer.data(), remaining, &read, nullptr)) {
      CloseHandle(handle);
      throw std::runtime_error("the selected local file could not be read");
    }
    if (read == 0) break;
    bytes.insert(bytes.end(), buffer.begin(), buffer.begin() + read);
  }
  CloseHandle(handle);
#else
  const int descriptor = ::open(path.c_str(), O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
  if (descriptor < 0) {
    throw std::runtime_error("the selected local file could not be opened");
  }
  std::array<std::uint8_t, 64 * 1024> buffer{};
  while (bytes.size() < limit) {
    const auto remaining = std::min<std::size_t>(buffer.size(), limit - bytes.size());
    const auto read = ::read(descriptor, buffer.data(), remaining);
    if (read < 0) {
      if (errno == EINTR) continue;
      ::close(descriptor);
      throw std::runtime_error("the selected local file could not be read");
    }
    if (read == 0) break;
    bytes.insert(bytes.end(), buffer.begin(), buffer.begin() + read);
  }
  ::close(descriptor);
#endif
  return bytes;
}

}  // namespace

LocalPathHandles::LocalPathHandles() : clock_(system_epoch_millis) {}

LocalPathHandles::LocalPathHandles(Clock clock)
    : clock_(clock ? std::move(clock) : Clock(system_epoch_millis)) {}

void LocalPathHandles::prune_locked(std::int64_t now) {
  for (auto item = entries_.begin(); item != entries_.end();) {
    if (item->second.expires_at_millis <= now) {
      item = entries_.erase(item);
    } else {
      ++item;
    }
  }
}

LocalPathRegistration LocalPathHandles::register_paths(
    const std::vector<std::filesystem::path>& paths) {
  const auto now = clock_();
  std::lock_guard lock(mutex_);
  prune_locked(now);
  if (entries_.size() >= kMaxLocalHandles) {
    return LocalPathRegistration{std::vector<LocalPathHandle>{}, 1};
  }

  LocalPathRegistration result;
  for (const auto& path : paths) {
    if (result.handles.size() >= kMaxPickedPaths || entries_.size() >= kMaxLocalHandles) {
      ++result.rejected;
      continue;
    }
    std::error_code error;
    const auto status = std::filesystem::symlink_status(path, error);
    if (error) {
      ++result.rejected;
      continue;
    }
    if (std::filesystem::is_symlink(status)) {
      ++result.rejected;
      continue;
    }
    const bool directory = std::filesystem::is_directory(status);
    if (!directory && !std::filesystem::is_regular_file(status)) {
      ++result.rejected;
      continue;
    }

    LocalPathHandle handle;
    handle.handle_id = new_handle_id();
    handle.name = file_name_or(path, directory ? "根目录" : "未命名文件");
    handle.kind = directory ? "directory" : "file";
    if (!directory) {
      std::error_code size_error;
      const auto size = std::filesystem::file_size(path, size_error);
      if (size_error) {
        ++result.rejected;
        continue;
      }
      handle.size = static_cast<std::uint64_t>(size);
    }

    Entry entry;
    entry.path = path;
    entry.directory = directory;
    entry.expires_at_millis = now + kLocalHandleTtlMillis;
    entries_.emplace(handle.handle_id, std::move(entry));
    result.handles.push_back(std::move(handle));
  }
  return result;
}

LocalPreviewData LocalPathHandles::read_preview(const std::string& handle_id,
                                                std::size_t max_bytes) {
  const auto now = clock_();
  std::lock_guard lock(mutex_);
  prune_locked(now);
  const auto found = entries_.find(handle_id);
  if (found == entries_.end()) {
    throw std::runtime_error("local file handle expired or was not found");
  }
  if (found->second.directory) {
    throw std::runtime_error("only regular files can be previewed");
  }
  std::error_code error;
  const auto status = std::filesystem::symlink_status(found->second.path, error);
  if (error) throw std::runtime_error("the selected local file could not be inspected");
  if (std::filesystem::is_symlink(status) || !std::filesystem::is_regular_file(status)) {
    throw std::runtime_error("selected local file is no longer a regular file");
  }
  std::error_code size_error;
  const auto size = std::filesystem::file_size(found->second.path, size_error);
  if (size_error) throw std::runtime_error("the selected local file could not be inspected");
  LocalPreviewData data;
  data.name = file_name_or(found->second.path, "未命名文件");
  data.size = static_cast<std::uint64_t>(size);
  data.bytes = read_bounded_file(found->second.path, max_bytes);
  return data;
}

nlohmann::json make_file_preview(const std::string& name, std::uint64_t size,
                                 std::vector<std::uint8_t> bytes,
                                 std::size_t max_bytes) {
  const bool truncated = bytes.size() > max_bytes;
  if (truncated) bytes.resize(max_bytes);

  if (!truncated) {
    if (const auto* mime = image_mime(bytes)) {
      return {{"name", name},
              {"size", size},
              {"kind", "image"},
              {"truncated", false},
              {"text", nullptr},
              {"dataUrl", std::string("data:") + mime + ";base64," + base64_encode(bytes)}};
    }
  }

  if (std::find(bytes.begin(), bytes.end(), static_cast<std::uint8_t>(0)) == bytes.end()) {
    const auto scan = scan_utf8(bytes);
    if (scan.state == Utf8State::kComplete) {
      return {{"name", name},
              {"size", size},
              {"kind", "text"},
              {"truncated", truncated},
              {"text", std::string(bytes.begin(), bytes.end())},
              {"dataUrl", nullptr}};
    }
    if (scan.state == Utf8State::kIncomplete) {
      return {{"name", name},
              {"size", size},
              {"kind", "text"},
              {"truncated", true},
              {"text", std::string(bytes.begin(), bytes.begin() + static_cast<std::ptrdiff_t>(scan.valid_up_to))},
              {"dataUrl", nullptr}};
    }
  }

  return {{"name", name},
          {"size", size},
          {"kind", "binary"},
          {"truncated", truncated},
          {"text", nullptr},
          {"dataUrl", nullptr}};
}

#ifdef _WIN32
std::vector<std::filesystem::path> pick_local_paths(const std::string& kind) {
  std::vector<std::filesystem::path> paths;
  if (kind == "uploadFiles") {
    std::vector<wchar_t> buffer(32768, L'\0');
    OPENFILENAMEW dialog{};
    dialog.lStructSize = sizeof(dialog);
    dialog.hwndOwner = GetActiveWindow();
    dialog.lpstrFilter = L"所有文件\0*.*\0";
    dialog.lpstrFile = buffer.data();
    dialog.nMaxFile = static_cast<DWORD>(buffer.size());
    dialog.Flags = OFN_ALLOWMULTISELECT | OFN_EXPLORER | OFN_FILEMUSTEXIST |
                   OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    if (!GetOpenFileNameW(&dialog)) return paths;
    std::vector<std::wstring> tokens;
    const wchar_t* cursor = buffer.data();
    while (*cursor != L'\0') {
      std::wstring token(cursor);
      tokens.push_back(token);
      cursor += token.size() + 1;
    }
    if (tokens.empty()) return paths;
    if (tokens.size() == 1) {
      paths.emplace_back(tokens.front());
      return paths;
    }
    const std::filesystem::path directory(tokens.front());
    for (std::size_t index = 1; index < tokens.size() && paths.size() < kMaxPickedPaths; ++index) {
      paths.push_back(directory / tokens[index]);
    }
    return paths;
  }

  BROWSEINFOW dialog{};
  dialog.hwndOwner = GetActiveWindow();
  dialog.lpszTitle = L"选择目录";
  dialog.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX;
  LPITEMIDLIST item = SHBrowseForFolderW(&dialog);
  if (item == nullptr) return paths;
  wchar_t chosen[MAX_PATH] = {};
  const bool resolved = SHGetPathFromIDListW(item, chosen) != FALSE;
  ILFree(item);
  if (resolved && chosen[0] != L'\0') paths.emplace_back(chosen);
  return paths;
}
#else
std::vector<std::filesystem::path> pick_local_paths(const std::string&) {
  throw std::runtime_error(
      "local path selection is not available in this build yet");
}
#endif

}  // namespace yukinal::desktop_host
