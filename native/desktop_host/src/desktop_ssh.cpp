#include "yukinal/desktop_host/desktop_ssh.hpp"

#include "yukinal/desktop_host/content_revision.hpp"
#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_target.hpp"
#include "yukinal/desktop_host/local_paths.hpp"

#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
#include <array>
#include <cctype>
#include <cstdlib>
#include <random>

#include "yukinal/config.hpp"
#include "yukinal/ssh.hpp"
#endif

namespace yukinal::desktop_host {
namespace {

#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
yukinal::ssh_service::TargetConfig build_target_config(const nlohmann::json& payload) {
  const auto& target = payload.at("target");
  const auto target_id = target.at("target_id").get<std::string>();
  const nlohmann::json document = {
      {"listen_port", 8765},
      {"api_token_env", "YUKINAL_SSH_API_TOKEN"},
      {"targets", nlohmann::json::array({target})},
  };
  auto config = yukinal::ssh_service::parse_config(document);
  return config.targets.at(target_id);
}

std::vector<std::pair<std::string, std::string>> credential_values(
    const nlohmann::json& payload) {
  std::vector<std::pair<std::string, std::string>> values;
  for (auto item = payload.at("credentials").begin(); item != payload.at("credentials").end();
       ++item) {
    values.emplace_back(item.key(), item.value().get<std::string>());
  }
  return values;
}

SshCommandResult to_result(const yukinal::ssh_service::ExecResult& result) {
  SshCommandResult converted;
  converted.completed = result.completed;
  converted.exit_code = result.exit_code;
  converted.stdout_text = result.stdout_text;
  converted.stderr_text = result.stderr_text;
  converted.error_code = result.error_code;
  converted.error_message = result.error_message;
  return converted;
}

// The fixed collector order the Python normalizer expects. The order is part of
// the contract: it is how the parser knows which section each marker delimits.
constexpr std::array<const char*, 22> kSnapshotCommandIds = {
    "os_detect",   "os_release",   "os_kernel",    "os_arch",     "os_hostname",
    "cpu_detect",  "cpu_ticks_a",  "cpu_ticks_b",  "cpu_cores",   "cpu_model",
    "cpu_load",    "memory_detect", "memory_info", "uptime_detect", "uptime_info",
    "disk_detect", "disk_usage",   "network_detect", "network_info", "docker_detect",
    "docker_info", "docker_ps"};

std::string new_snapshot_nonce() {
  std::random_device device;
  static constexpr char kHex[] = "0123456789abcdef";
  std::string nonce;
  nonce.reserve(32);
  for (int index = 0; index < 16; ++index) {
    const auto byte = static_cast<unsigned>(device() & 0xFFu);
    nonce.push_back(kHex[byte >> 4]);
    nonce.push_back(kHex[byte & 0x0Fu]);
  }
  return nonce;
}

nlohmann::json parse_system_snapshot_outputs(const std::string& output,
                                            const std::string& nonce) {
  nlohmann::json results = nlohmann::json::object();
  for (const auto* id : kSnapshotCommandIds) {
    const std::string start_marker =
        std::string("__YK_SNAPSHOT_") + nonce + "_" + id + "_BEGIN__\n";
    const auto start = output.find(start_marker);
    if (start == std::string::npos) {
      throw std::runtime_error("the C++ SSH service returned an incomplete system snapshot");
    }
    const auto data_start = start + start_marker.size();
    if (output.find(start_marker, data_start) != std::string::npos) {
      throw std::runtime_error("the C++ SSH service returned an ambiguous system snapshot");
    }
    const std::string end_prefix =
        std::string("\n__YK_SNAPSHOT_") + nonce + "_" + id + "_END_";
    const auto end = output.find(end_prefix, data_start);
    if (end == std::string::npos) {
      throw std::runtime_error("the C++ SSH service returned an incomplete system snapshot");
    }
    const auto status_start = end + end_prefix.size();
    const auto status_end = output.find("__\n", status_start);
    if (status_end == std::string::npos) {
      throw std::runtime_error("the C++ SSH service returned an invalid system snapshot");
    }
    int exit_code = -1;
    try {
      exit_code = std::stoi(output.substr(status_start, status_end - status_start));
    } catch (const std::exception&) {
      throw std::runtime_error(
          "the C++ SSH service returned an invalid system snapshot status");
    }
    if (exit_code < 0 || exit_code > 255) {
      throw std::runtime_error(
          "the C++ SSH service returned an invalid system snapshot status");
    }
    results[id] = {{"exitCode", exit_code},
                   {"stdout", output.substr(data_start, end - data_start)}};
  }
  return results;
}

int base64_value(char character) {
  if (character >= 'A' && character <= 'Z') return character - 'A';
  if (character >= 'a' && character <= 'z') return character - 'a' + 26;
  if (character >= '0' && character <= '9') return character - '0' + 52;
  if (character == '+') return 62;
  if (character == '/') return 63;
  return -1;
}

std::vector<std::uint8_t> base64_decode(const std::string& text) {
  std::vector<std::uint8_t> bytes;
  int buffer = 0;
  int bits = 0;
  for (const char character : text) {
    if (character == '=') break;
    if (std::isspace(static_cast<unsigned char>(character)) != 0) continue;
    const int value = base64_value(character);
    if (value < 0) throw std::runtime_error("the SSH service returned invalid SFTP data");
    buffer = (buffer << 6) | value;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      bytes.push_back(static_cast<std::uint8_t>((buffer >> bits) & 0xFF));
    }
  }
  return bytes;
}

// Lossy UTF-8 decode for the browser's text view; invalid sequences become U+FFFD
// instead of failing the read.
std::string utf8_lossy(const std::vector<std::uint8_t>& bytes) {
  static constexpr char kReplacement[] = "\xEF\xBF\xBD";
  std::string out;
  out.reserve(bytes.size());
  std::size_t index = 0;
  while (index < bytes.size()) {
    const auto byte = bytes[index];
    if (byte < 0x80) {
      out.push_back(static_cast<char>(byte));
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
      out += kReplacement;
      ++index;
      continue;
    }
    if (index + static_cast<std::size_t>(length) > bytes.size()) {
      out += kReplacement;
      break;
    }
    bool valid = true;
    for (int offset = 1; offset < length; ++offset) {
      const auto continuation = bytes[index + static_cast<std::size_t>(offset)];
      if ((continuation & 0xC0) != 0x80) {
        valid = false;
        break;
      }
      code_point = (code_point << 6) | (continuation & 0x3Fu);
    }
    if (!valid || code_point < minimum || code_point > 0x10FFFF ||
        (code_point >= 0xD800 && code_point <= 0xDFFF)) {
      out += kReplacement;
      ++index;
      continue;
    }
    out.append(reinterpret_cast<const char*>(bytes.data() + index),
               static_cast<std::size_t>(length));
    index += static_cast<std::size_t>(length);
  }
  return out;
}

std::string join_remote_path(const std::string& parent, const std::string& name) {
  if (parent == "/") return "/" + name;
  if (!parent.empty() && parent.back() == '/') return parent + name;
  return parent + "/" + name;
}

bool valid_browser_path(const std::string& path) {
  return !path.empty() && path.size() <= 4096 && path.find('\0') == std::string::npos &&
         path.find('\r') == std::string::npos && path.find('\n') == std::string::npos;
}

bool valid_browser_preview_path(const std::string& path) {
  if (!valid_browser_path(path) || path.front() != '/') return false;
  if (path.find('\\') != std::string::npos) return false;
  std::size_t start = 0;
  while (start <= path.size()) {
    const auto slash = path.find('/', start);
    const auto segment =
        path.substr(start, slash == std::string::npos ? std::string::npos : slash - start);
    if (segment == "." || segment == "..") return false;
    if (slash == std::string::npos) break;
    start = slash + 1;
  }
  return true;
}
#endif

}  // namespace

SshCommandResult run_server_logs(const DesktopDatabase& database,
                                 const CredentialStore& credentials,
                                 const std::string& server_id,
                                 std::optional<std::uint32_t> since_seconds,
                                 const std::string& unit, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the server.logs timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  return to_result(
      yukinal::ssh_service::server_logs(target, since_seconds, unit, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)since_seconds;
  (void)unit;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

SshCommandResult run_host_key_probe(const DesktopDatabase& database,
                                    const CredentialStore& credentials,
                                    const std::string& server_id, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the host-key probe timeout is out of range");
  }
  const auto payload = resolve_ssh_service_probe_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  return to_result(yukinal::ssh_service::host_key_probe(target, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

SshCommandResult run_service_inventory(const DesktopDatabase& database,
                                       const CredentialStore& credentials,
                                       const std::string& server_id, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the service inventory timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  return to_result(yukinal::ssh_service::service_inventory(target, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

/**
 * Resolves the target and runs one bounded read-only SSH operation, mapping the raw
 * result. The `research` nested callbacks keep each helper's SSH-core reference
 * inside the build guard so the desktop host still compiles without the SSH core.
 */
SshCommandResult run_docker_ps(const DesktopDatabase& database,
                               const CredentialStore& credentials,
                               const std::string& server_id, bool all, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the docker.ps timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  return to_result(yukinal::ssh_service::docker_ps(target, all, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)all;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

SshCommandResult run_docker_logs(const DesktopDatabase& database,
                                 const CredentialStore& credentials,
                                 const std::string& server_id, const std::string& container,
                                 std::uint32_t tail, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the docker.logs timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  return to_result(
      yukinal::ssh_service::docker_logs(target, container, tail, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)container;
  (void)tail;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

SshCommandResult run_docker_inspect(const DesktopDatabase& database,
                                    const CredentialStore& credentials,
                                    const std::string& server_id, const std::string& container,
                                    int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the docker.inspect timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  return to_result(yukinal::ssh_service::docker_inspect(target, container, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)container;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

SshCommandResult run_systemd_inspect(const DesktopDatabase& database,
                                     const CredentialStore& credentials,
                                     const std::string& server_id, const std::string& service,
                                     int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the systemd.inspect timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  return to_result(yukinal::ssh_service::systemd_inspect(target, service, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)service;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

SshCommandResult run_package_inspect(const DesktopDatabase& database,
                                     const CredentialStore& credentials,
                                     const std::string& server_id, const std::string& manager,
                                     const std::string& package, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the package.inspect timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  return to_result(
      yukinal::ssh_service::package_inspect(target, manager, package, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)manager;
  (void)package;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

nlohmann::json run_agent_filesystem_read(const DesktopDatabase& database,
                                         const CredentialStore& credentials,
                                         const std::string& server_id, const std::string& path,
                                         std::uint32_t max_bytes, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the filesystem.read timeout is out of range");
  }
  const auto payload = resolve_ssh_service_filesystem_read_target(
      database, credentials,
      {{"serverId", server_id}, {"path", path}, {"maxBytes", max_bytes}});
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  const auto bounded = payload.at("filesystemRead").at("maxBytes").get<std::uint32_t>();
  const auto result = yukinal::ssh_service::sftp_read(target, path, bounded, timeout_seconds);
  if (!result.error_code.empty() || !result.completed) {
    throw std::runtime_error(result.error_message.empty() ? result.error_code
                                                          : result.error_message);
  }
  auto bytes = base64_decode(result.stdout_text);
  const bool truncated = bytes.size() > bounded;
  if (truncated) bytes.resize(bounded);
  const std::string raw(reinterpret_cast<const char*>(bytes.data()), bytes.size());
  return {{"path", path},
          {"content", utf8_lossy(bytes)},
          {"truncated", truncated},
          {"revision", content_revision_hex(raw)}};
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)path;
  (void)max_bytes;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

SshCommandResult run_connection_check(const DesktopDatabase& database,
                                      const CredentialStore& credentials,
                                      const std::string& server_id, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the connection check timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  return to_result(yukinal::ssh_service::connection_check(target, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

nlohmann::json run_system_snapshot(const DesktopDatabase& database,
                                   const CredentialStore& credentials,
                                   const std::string& server_id, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the system snapshot timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  const auto nonce = new_snapshot_nonce();
  const auto result = yukinal::ssh_service::system_snapshot(target, nonce, timeout_seconds);
  if (!result.error_code.empty() || !result.completed) {
    throw std::runtime_error(result.error_message.empty()
                                 ? "the system snapshot operation failed"
                                 : result.error_message);
  }
  return parse_system_snapshot_outputs(result.stdout_text, nonce);
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

nlohmann::json run_remote_file_list(const DesktopDatabase& database,
                                    const CredentialStore& credentials,
                                    const std::string& server_id, const std::string& path,
                                    int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (!valid_browser_path(path)) throw std::runtime_error("the remote path is invalid");
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the remote listing timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  const auto result = yukinal::ssh_service::sftp_list(target, path, timeout_seconds);
  if (!result.error_code.empty() || !result.completed) {
    throw std::runtime_error(result.error_message.empty() ? result.error_code
                                                          : result.error_message);
  }
  nlohmann::json entries;
  try {
    entries = nlohmann::json::parse(result.stdout_text);
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error("the SSH service returned an invalid directory listing");
  }
  if (!entries.is_array() || entries.size() > 4096) {
    throw std::runtime_error("the SSH service returned an invalid directory listing");
  }
  nlohmann::json converted = nlohmann::json::array();
  for (const auto& entry : entries) {
    if (!entry.is_object() || !entry.contains("name") || !entry.contains("file_type") ||
        !entry.contains("size") || !entry.at("name").is_string() ||
        !entry.at("file_type").is_string() || !entry.at("size").is_number_unsigned()) {
      throw std::runtime_error("the SSH service returned an invalid directory entry");
    }
    const auto name = entry.at("name").get<std::string>();
    const auto type = entry.at("file_type").get<std::string>();
    const bool known_type = type == "directory" || type == "file" || type == "symlink" ||
                            type == "other";
    if (name.empty() || name.size() > 4096 || !known_type) {
      throw std::runtime_error("the SSH service returned an invalid directory entry");
    }
    converted.push_back({{"name", name},
                         {"path", join_remote_path(path, name)},
                         {"type", type},
                         {"size", entry.at("size")}});
  }
  return {{"path", path}, {"entries", std::move(converted)}};
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)path;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

nlohmann::json run_remote_file_read(const DesktopDatabase& database,
                                    const CredentialStore& credentials,
                                    const std::string& server_id, const std::string& path,
                                    int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (!valid_browser_path(path)) throw std::runtime_error("the remote path is invalid");
  constexpr std::uint32_t kBrowserReadBytes = 1024 * 1024;
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  const auto result = yukinal::ssh_service::sftp_read(target, path, kBrowserReadBytes, timeout_seconds);
  if (!result.error_code.empty() || !result.completed) {
    throw std::runtime_error(result.error_message.empty() ? result.error_code
                                                          : result.error_message);
  }
  auto bytes = base64_decode(result.stdout_text);
  const bool truncated = bytes.size() > kBrowserReadBytes;
  if (truncated) bytes.resize(kBrowserReadBytes);
  nlohmann::json response = {{"path", path},
                             {"content", utf8_lossy(bytes)},
                             {"truncated", truncated}};
  return response;
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)path;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

nlohmann::json run_remote_file_preview(const DesktopDatabase& database,
                                       const CredentialStore& credentials,
                                       const std::string& server_id, const std::string& path,
                                       int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (!valid_browser_preview_path(path)) throw std::runtime_error("the remote path is invalid");
  constexpr std::uint32_t kBrowserReadBytes = 1024 * 1024;
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  yukinal::ssh_service::ScopedCredentialOverride credentials_scope(credential_values(payload));
  const auto stat = yukinal::ssh_service::sftp_stat(target, path, timeout_seconds);
  if (!stat.error_code.empty() || !stat.completed) {
    throw std::runtime_error(stat.error_message.empty() ? stat.error_code : stat.error_message);
  }
  nlohmann::json metadata;
  try {
    metadata = nlohmann::json::parse(stat.stdout_text);
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error("the SSH service returned invalid file metadata");
  }
  if (!metadata.is_object() || metadata.value("kind", std::string()) != "file") {
    throw std::runtime_error("only regular remote files can be previewed");
  }
  const auto read = yukinal::ssh_service::sftp_read(target, path, kBrowserReadBytes, timeout_seconds);
  if (!read.error_code.empty() || !read.completed) {
    throw std::runtime_error(read.error_message.empty() ? read.error_code : read.error_message);
  }
  auto bytes = base64_decode(read.stdout_text);
  const auto slash = path.rfind('/');
  const auto name = slash == std::string::npos || slash + 1 >= path.size()
                        ? std::string("远端文件")
                        : path.substr(slash + 1);
  return make_file_preview(name, metadata.value("size", std::uint64_t{0}), std::move(bytes),
                           kBrowserReadBytes);
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)path;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

}  // namespace yukinal::desktop_host
