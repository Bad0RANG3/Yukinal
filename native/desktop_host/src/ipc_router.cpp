#include "yukinal/desktop_host/ipc_router.hpp"

#include "yukinal/desktop_host/content_revision.hpp"
#include "yukinal/desktop_host/desktop_mcp.hpp"
#include "yukinal/desktop_host/desktop_oauth.hpp"
#include "yukinal/desktop_host/desktop_ssh.hpp"
#include "yukinal/desktop_host/desktop_target.hpp"
#include "yukinal/desktop_host/desktop_terminal.hpp"
#include "yukinal/desktop_host/host_trust.hpp"
#include "yukinal/desktop_host/network_proxy.hpp"
#include "yukinal/desktop_host/sftp_transfer_backend.hpp"
#include "yukinal/desktop_host/transfer_history.hpp"
#include "yukinal/desktop_host/transfer_types.hpp"

#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
#include "host_key.hpp"
#endif

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdlib>
#include <ctime>
#include <filesystem>
#include <iomanip>
#include <initializer_list>
#include <map>
#include <random>
#include <regex>
#include <set>
#include <sstream>
#include <string_view>
#include <utility>
#include <vector>

#ifdef _WIN32
#include <windows.h>
#include <shellapi.h>
#else
#include <spawn.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <cerrno>
#include <thread>
#include <unistd.h>
extern char** environ;
#endif

namespace yukinal::desktop_host {
namespace {

IpcReply failure(std::string code, std::string message) {
  return {false, {{"code", std::move(code)}, {"message", std::move(message)}}};
}

std::string sidecar_error_code(int code) {
  switch (code) {
    case -32700:
    case -32600:
      return "INVALID_REQUEST";
    case -32601:
      return "METHOD_NOT_FOUND";
    case -32602:
      return "INVALID_PARAMS";
    case -32603:
      return "AGENT_ERROR";
    case -32604:
      return "NOT_IMPLEMENTED";
    default:
      return "AGENT_ERROR";
  }
}

bool exact_string_fields(const nlohmann::json& value,
                         const std::set<std::string>& fields) {
  if (!value.is_object() || value.size() != fields.size()) return false;
  return std::all_of(fields.begin(), fields.end(), [&value](const auto& field) {
    return value.contains(field) && value.at(field).is_string();
  });
}

bool is_one_of(const std::string& value, std::initializer_list<const char*> options) {
  return std::any_of(options.begin(), options.end(), [&value](const char* option) {
    return value == option;
  });
}

bool only_fields(const nlohmann::json& value, const std::set<std::string>& allowed,
                 const std::set<std::string>& required) {
  if (!value.is_object()) return false;
  for (const auto& field : required) {
    if (!value.contains(field)) return false;
  }
  for (auto item = value.begin(); item != value.end(); ++item) {
    if (!allowed.contains(item.key())) return false;
  }
  return true;
}

std::string trim_ascii(const std::string& value) {
  auto first = value.begin();
  while (first != value.end() && std::isspace(static_cast<unsigned char>(*first))) ++first;
  auto last = value.end();
  while (last != first && std::isspace(static_cast<unsigned char>(*(last - 1)))) --last;
  return std::string(first, last);
}

size_t utf8_character_count(const std::string& value) {
  return static_cast<size_t>(std::count_if(value.begin(), value.end(), [](unsigned char byte) {
    return (byte & 0xC0) != 0x80;
  }));
}

bool valid_opaque_id(const std::string& value) {
  if (value.empty() || utf8_character_count(value) > 256) return false;
  return std::all_of(value.begin(), value.end(), [](unsigned char byte) {
    return (byte >= 'a' && byte <= 'z') || (byte >= 'A' && byte <= 'Z') ||
           (byte >= '0' && byte <= '9') || byte == '_' || byte == '-' || byte == '.';
  });
}

bool has_control_characters(const std::string& value) {
  return std::any_of(value.begin(), value.end(), [](unsigned char byte) {
    return byte < 0x20 || byte == 0x7f;
  });
}

bool valid_chat_filename(const std::string& value, size_t max_characters,
                         bool allow_path_separators = false) {
  if (value.empty() || utf8_character_count(value) > max_characters ||
      value != trim_ascii(value) || has_control_characters(value)) return false;
  return allow_path_separators ||
         (value.find('/') == std::string::npos && value.find('\\') == std::string::npos);
}

bool valid_base64(const std::string& value, size_t max_decoded_bytes,
                  size_t& decoded_bytes) {
  decoded_bytes = 0;
  if (value.size() < 4 || value.size() % 4 != 0) return false;
  size_t padding = 0;
  if (!value.empty() && value.back() == '=') ++padding;
  if (value.size() > 1 && value[value.size() - 2] == '=') ++padding;
  for (size_t index = 0; index < value.size() - padding; ++index) {
    const auto byte = static_cast<unsigned char>(value[index]);
    if (!((byte >= 'a' && byte <= 'z') || (byte >= 'A' && byte <= 'Z') ||
          (byte >= '0' && byte <= '9') || byte == '+' || byte == '/')) return false;
  }
  for (size_t index = value.size() - padding; index < value.size(); ++index) {
    if (value[index] != '=') return false;
  }
  decoded_bytes = (value.size() / 4) * 3 - padding;
  return decoded_bytes <= max_decoded_bytes;
}

bool valid_chat_prompt_parts(const nlohmann::json& parts) {
  if (!parts.is_array() || parts.empty() || parts.size() > 25) return false;
  size_t text_characters = 0;
  size_t inline_bytes = 0;
  size_t file_bytes = 0;
  size_t images = 0;
  size_t documents = 0;
  size_t audios = 0;
  size_t files = 0;
  for (const auto& part : parts) {
    if (!part.is_object() || !part.contains("type") || !part.at("type").is_string()) return false;
    const auto type = part.at("type").get<std::string>();
    if (type == "text") {
      if (!only_fields(part, {"type", "text"}, {"type", "text"}) ||
          !part.at("text").is_string() || part.at("text").get<std::string>().empty()) return false;
      text_characters += utf8_character_count(part.at("text").get<std::string>());
      continue;
    }

    if (type == "image" || type == "document" || type == "audio" || type == "file") {
      const bool name_optional = type == "image" || type == "audio";
      const std::set<std::string> allowed = {"type", "mediaType", "data", "name"};
      const std::set<std::string> required = name_optional
          ? std::set<std::string>{"type", "mediaType", "data"}
          : std::set<std::string>{"type", "mediaType", "data", "name"};
      if (!only_fields(part, allowed, required) || !part.at("mediaType").is_string() ||
          !part.at("data").is_string()) return false;
      const auto media_type = part.at("mediaType").get<std::string>();
      const auto data = part.at("data").get<std::string>();
      if (part.contains("name") && !part.at("name").is_string()) return false;

      if (type == "file") {
        if (media_type != "text/plain" || data.empty() || data.size() > 512 * 1024 ||
            !part.at("name").is_string()) return false;
        if (!valid_chat_filename(part.at("name").get<std::string>(), 128)) return false;
        for (const auto byte : data) {
          const auto value = static_cast<unsigned char>(byte);
          if (value == 0 || (value < 0x20 && value != '\n' && value != '\r' && value != '\t') ||
              value == 0x7f) return false;
        }
        file_bytes += data.size();
        ++files;
        continue;
      }

      size_t decoded = 0;
      size_t max_bytes = 0;
      if (type == "image") {
        if (!is_one_of(media_type, {"image/png", "image/jpeg", "image/webp", "image/gif"})) return false;
        max_bytes = 5 * 1024 * 1024;
        ++images;
        if (part.contains("name") &&
            !valid_chat_filename(part.at("name").get<std::string>(), 128, true)) return false;
      } else if (type == "document") {
        if (media_type != "application/pdf" ||
            !valid_chat_filename(part.at("name").get<std::string>(), 128)) return false;
        max_bytes = 8 * 1024 * 1024;
        ++documents;
      } else {
        if (!is_one_of(media_type, {"audio/wav", "audio/mpeg", "audio/ogg", "audio/flac"})) return false;
        max_bytes = 8 * 1024 * 1024;
        ++audios;
        if (part.contains("name") &&
            !valid_chat_filename(part.at("name").get<std::string>(), 128)) return false;
      }
      if (!valid_base64(data, max_bytes, decoded)) return false;
      inline_bytes += decoded;
      if (inline_bytes > 12 * 1024 * 1024) return false;
      continue;
    }
    return false;
  }
  return text_characters <= 100'000 && inline_bytes <= 12 * 1024 * 1024 &&
         file_bytes <= 2 * 1024 * 1024 && images <= 8 && documents <= 4 &&
         audios <= 4 && files <= 8;
}

bool valid_request_text(const nlohmann::json& params, const char* key, size_t max_chars,
                        bool require_nonempty, std::string& result) {
  const auto& value = params.at(key);
  if (!value.is_string()) return false;
  result = trim_ascii(value.get<std::string>());
  const auto count = utf8_character_count(result);
  return (!require_nonempty || count > 0) && count <= max_chars;
}

bool valid_integer_range(const nlohmann::json& value, int minimum, int maximum, int& result) {
  if (value.is_number_unsigned()) {
    const auto requested = value.get<std::uint64_t>();
    if (requested < static_cast<std::uint64_t>(minimum) ||
        requested > static_cast<std::uint64_t>(maximum)) return false;
    result = static_cast<int>(requested);
    return true;
  }
  if (!value.is_number_integer()) return false;
  const auto requested = value.get<std::int64_t>();
  if (requested < minimum || requested > maximum) return false;
  result = static_cast<int>(requested);
  return true;
}

std::string iso8601_now() {
  const auto now = std::chrono::system_clock::now();
  const auto time = std::chrono::system_clock::to_time_t(now);
  std::tm utc{};
#ifdef _WIN32
  gmtime_s(&utc, &time);
#else
  gmtime_r(&time, &utc);
#endif
  std::ostringstream rendered;
  rendered << std::put_time(&utc, "%Y-%m-%dT%H:%M:%SZ");
  return rendered.str();
}

std::string next_id(const char* prefix) {
  static std::atomic<std::uint64_t> counter{1};
  const auto millis = std::chrono::duration_cast<std::chrono::milliseconds>(
                          std::chrono::system_clock::now().time_since_epoch())
                          .count();
  std::ostringstream rendered;
  rendered << prefix << '_' << std::hex << static_cast<std::uint64_t>(millis)
           << counter.fetch_add(1, std::memory_order_relaxed);
  return rendered.str();
}

bool valid_event_name(const std::string& value) {
  static const std::regex pattern(R"(^[a-z][a-z0-9_.-]{0,95}$)");
  return std::regex_match(value, pattern);
}

std::string new_probe_ticket() {
  std::random_device device;
  static constexpr char kHex[] = "0123456789abcdef";
  std::string ticket = "probe_";
  ticket.reserve(6 + 64);
  for (int index = 0; index < 32; ++index) {
    const auto byte = static_cast<unsigned>(device() & 0xFFu);
    ticket.push_back(kHex[byte >> 4]);
    ticket.push_back(kHex[byte & 0x0Fu]);
  }
  return ticket;
}

bool valid_host_key_fingerprint(const std::string& value) {
  if (value.size() != 7 + 43 || value.rfind("SHA256:", 0) != 0) return false;
  return std::all_of(value.begin() + 7, value.end(), [](unsigned char byte) {
    return std::isalnum(byte) != 0 || byte == '+' || byte == '/';
  });
}

const char* comparison_word(HostKeyComparison comparison) {
  switch (comparison) {
    case HostKeyComparison::Matches:
      return "matches";
    case HostKeyComparison::Mismatch:
      return "mismatch";
    default:
      return "unpinned";
  }
}

bool valid_subscription_token(const std::string& value) {
  static const std::regex pattern(R"(^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$)");
  return std::regex_match(value, pattern);
}

bool valid_method_name(const std::string& value) {
  static const std::regex pattern(R"(^[a-z][a-z0-9_.-]{0,127}$)");
  return std::regex_match(value, pattern);
}

bool valid_server_id(const std::string& value) {
  static const std::regex pattern(R"(^srv_[a-z0-9]+$)");
  return value.size() <= 256 && std::regex_match(value, pattern);
}

bool valid_auth_secret(const nlohmann::json& auth, const char* field,
                       size_t max_characters, bool required) {
  if (!auth.contains(field)) return !required;
  if (!auth.at(field).is_string()) return false;
  const auto& value = auth.at(field).get_ref<const std::string&>();
  const auto length = utf8_character_count(value);
  return length > 0 && length <= max_characters;
}

bool validate_authentication(const nlohmann::json& auth) {
  if (!auth.is_object() || !auth.contains("method") || !auth.at("method").is_string()) {
    return false;
  }
  const auto method = auth.at("method").get<std::string>();
  if (method == "password") {
    return only_fields(auth, {"method", "password"}, {"method", "password"}) &&
           valid_auth_secret(auth, "password", 4'096, true);
  }
  if (method == "privateKey") {
    return only_fields(auth, {"method", "privateKeyPem", "passphrase"},
                       {"method", "privateKeyPem"}) &&
           valid_auth_secret(auth, "privateKeyPem", 1'000'000, true) &&
           valid_auth_secret(auth, "passphrase", 4'096, false);
  }
  if (method == "certificate") {
    std::string certificate_path;
    if (!only_fields(auth,
                     {"method", "privateKeyPem", "passphrase", "certificatePath", "privateKeyPath"},
                     {"method", "privateKeyPem", "certificatePath"}) ||
        !valid_auth_secret(auth, "privateKeyPem", 1'000'000, true) ||
        !valid_auth_secret(auth, "passphrase", 4'096, false)) return false;
    nlohmann::json path_value = {{"path", auth.at("certificatePath")}};
    if (!valid_request_text(path_value, "path", 4'096, true, certificate_path)) return false;
    if (auth.contains("privateKeyPath")) {
      nlohmann::json private_path = {{"path", auth.at("privateKeyPath")}};
      std::string normalized;
      if (!valid_request_text(private_path, "path", 4'096, true, normalized)) return false;
    }
    return true;
  }
  if (method == "agent") {
    return only_fields(auth, {"method"}, {"method"});
  }
  if (method == "identity") {
    std::string identity_id;
    return only_fields(auth, {"method", "identityId"}, {"method", "identityId"}) &&
           valid_request_text(auth, "identityId", 256, true, identity_id);
  }
  return false;
}

bool normalize_host_certificate_authority(const nlohmann::json& input,
                                          nlohmann::json& output) {
  if (!input.is_object() ||
      !only_fields(input,
                   {"caPublicKey", "principals", "revocationListPath",
                    "revocationListUrl", "revocationListSigners"},
                   {"caPublicKey", "principals"})) return false;
  nlohmann::json ca_public_key_value = {{"value", input.at("caPublicKey")}};
  std::string ca_public_key;
  if (!valid_request_text(ca_public_key_value, "value", 16'384, true, ca_public_key) ||
      has_control_characters(ca_public_key)) return false;
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (!yukinal::ssh_service::detail::valid_host_ca_public_key(ca_public_key)) return false;
#else
  if (!ca_public_key.starts_with("ssh-") && !ca_public_key.starts_with("ecdsa-") &&
      !ca_public_key.starts_with("sk-")) return false;
#endif

  const auto& principals = input.at("principals");
  if (!principals.is_array() || principals.empty() || principals.size() > 32) return false;
  nlohmann::json normalized_principals = nlohmann::json::array();
  for (const auto& value : principals) {
    if (!value.is_string()) return false;
    nlohmann::json principal_value = {{"value", value}};
    std::string principal;
    if (!valid_request_text(principal_value, "value", 253, true, principal) ||
        has_control_characters(principal)) return false;
    normalized_principals.push_back(std::move(principal));
  }

  output = {{"caPublicKey", std::move(ca_public_key)},
            {"principals", std::move(normalized_principals)}};
  for (const auto& item : {std::pair<const char*, size_t>{"revocationListPath", 4'096},
                           std::pair<const char*, size_t>{"revocationListUrl", 2'048}}) {
    if (!input.contains(item.first)) continue;
    nlohmann::json value = {{"value", input.at(item.first)}};
    std::string normalized;
    if (!valid_request_text(value, "value", item.second, true, normalized) ||
        has_control_characters(normalized)) return false;
    if (std::string_view(item.first) == "revocationListPath" &&
        !std::filesystem::path(normalized).is_absolute()) return false;
    output[item.first] = std::move(normalized);
  }
  if (output.contains("revocationListPath") && output.contains("revocationListUrl")) return false;

  if (input.contains("revocationListSigners")) {
    const auto& signers = input.at("revocationListSigners");
    if (!signers.is_array() || signers.size() > 16) return false;
    std::set<std::string> distinct;
    nlohmann::json normalized_signers = nlohmann::json::array();
    for (const auto& value : signers) {
      if (!value.is_string()) return false;
      nlohmann::json signer_value = {{"value", value}};
      std::string signer;
      if (!valid_request_text(signer_value, "value", 16'384, true, signer) ||
          has_control_characters(signer) || !distinct.insert(signer).second) return false;
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
      if (!yukinal::ssh_service::detail::valid_host_ca_public_key(signer)) return false;
#else
      if (!signer.starts_with("ssh-") && !signer.starts_with("ecdsa-") &&
          !signer.starts_with("sk-")) return false;
#endif
      normalized_signers.push_back(std::move(signer));
    }
    if (!normalized_signers.empty()) output["revocationListSigners"] = std::move(normalized_signers);
  }
  return true;
}

bool normalize_server_input(const std::string& method, const nlohmann::json& params,
                            nlohmann::json& output) {
  const bool add = method == "server_add";
  const std::set<std::string> add_allowed = {
      "name", "host", "port", "username", "environment", "groupId",
      "hostCertificateAuthority", "authentication"};
  const std::set<std::string> update_allowed = {
      "serverId", "name", "host", "port", "username", "environment", "groupId",
      "hostCertificateAuthority", "clearHostCertificateAuthority", "authentication"};
  const std::set<std::string> required = add
      ? std::set<std::string>{"name", "host", "username", "environment", "authentication"}
      : std::set<std::string>{"serverId", "name", "host", "username", "environment"};
  if (!only_fields(params, add ? add_allowed : update_allowed, required)) return false;

  std::string name;
  std::string host;
  std::string username;
  if (!valid_request_text(params, "name", 256, true, name) ||
      !valid_request_text(params, "host", 256, true, host) ||
      !valid_request_text(params, "username", 256, true, username)) return false;
  int port = 22;
  if (params.contains("port") &&
      !valid_integer_range(params.at("port"), 1, 65'535, port)) return false;
  if (!params.at("environment").is_string() ||
      !is_one_of(params.at("environment").get<std::string>(),
                 {"local", "development", "staging", "production", "unknown"})) return false;

  output = {{"name", std::move(name)}, {"host", std::move(host)},
            {"port", port}, {"username", std::move(username)},
            {"environment", params.at("environment")}};
  if (params.contains("serverId")) {
    std::string server_id;
    if (!valid_request_text(params, "serverId", 256, true, server_id) ||
        !valid_server_id(server_id)) return false;
    output["serverId"] = std::move(server_id);
  }
  if (params.contains("groupId")) {
    std::string group_id;
    if (!valid_request_text(params, "groupId", 256, false, group_id)) return false;
    output["groupId"] = std::move(group_id);
  }
  if (params.contains("hostCertificateAuthority")) {
    nlohmann::json authority;
    if (!normalize_host_certificate_authority(params.at("hostCertificateAuthority"), authority)) return false;
    output["hostCertificateAuthority"] = std::move(authority);
  }
  if (params.contains("clearHostCertificateAuthority")) {
    if (!params.at("clearHostCertificateAuthority").is_boolean()) return false;
    output["clearHostCertificateAuthority"] = params.at("clearHostCertificateAuthority");
    if (output.at("clearHostCertificateAuthority").get<bool>() &&
        output.contains("hostCertificateAuthority")) return false;
  }
  if (params.contains("authentication")) {
    if (!validate_authentication(params.at("authentication"))) return false;
    output["authentication"] = params.at("authentication");
  }
  return true;
}

bool clean_credential_references(DesktopDatabase* database,
                                 const CredentialStore& credentials,
                                 const std::vector<std::string>& references,
                                 std::string& error) {
  bool all_recovered = true;
  for (const auto& raw_reference : references) {
    try {
      const auto reference = CredentialReference::parse(raw_reference);
      credentials.erase(reference);
      database->clear_credential_cleanup(raw_reference);
      continue;
    } catch (const std::exception& cleanup_error) {
      try {
        database->queue_credential_cleanup(raw_reference, cleanup_error.what(), iso8601_now());
      } catch (...) {
        all_recovered = false;
        error = "Credential cleanup failed and could not be queued for retry";
      }
    }
  }
  return all_recovered;
}

std::string ascii_lower(std::string value) {
  std::transform(value.begin(), value.end(), value.begin(),
                 [](unsigned char byte) { return static_cast<char>(std::tolower(byte)); });
  return value;
}

bool ipv4_loopback(const std::string& host) {
  int octets[4] = {0, 0, 0, 0};
  std::size_t start = 0;
  for (int index = 0; index < 4; ++index) {
    const auto dot = host.find('.', start);
    const auto end = dot == std::string::npos ? host.size() : dot;
    if (end == start || end - start > 3) return false;
    int value = 0;
    for (std::size_t position = start; position < end; ++position) {
      if (!std::isdigit(static_cast<unsigned char>(host[position]))) return false;
      value = value * 10 + (host[position] - '0');
    }
    if (value > 255) return false;
    octets[index] = value;
    if (index < 3) {
      if (dot == std::string::npos) return false;
      start = dot + 1;
    } else if (dot != std::string::npos) {
      return false;
    }
  }
  return octets[0] == 127;
}

// `validate_http_url` from the retired core: HTTPS anywhere, plain HTTP only on
// loopback, and never a fragment, query, or embedded credentials.
bool is_loopback_host(std::string host) {
  if (host.size() >= 2 && host.front() == '[' && host.back() == ']') {
    host = host.substr(1, host.size() - 2);
  }
  const auto lowered = ascii_lower(host);
  if (lowered == "localhost") return true;
  if (lowered.size() > 10 && lowered.compare(lowered.size() - 10, 10, ".localhost") == 0) {
    return true;
  }
  if (lowered == "::1") return true;
  return ipv4_loopback(lowered);
}

bool validate_oauth_url(const std::string& raw, std::string& out) {
  const auto value = trim_ascii(raw);
  if (value.empty() || value.size() > 2048 || has_control_characters(value)) return false;
  if (value.find('?') != std::string::npos || value.find('#') != std::string::npos) return false;
  std::size_t authority_start = 0;
  bool secure = false;
  if (value.rfind("https://", 0) == 0) {
    authority_start = 8;
    secure = true;
  } else if (value.rfind("http://", 0) == 0) {
    authority_start = 7;
  } else {
    return false;
  }
  if (authority_start >= value.size()) return false;
  const auto path = value.find('/', authority_start);
  const auto authority = value.substr(
      authority_start, path == std::string::npos ? std::string::npos : path - authority_start);
  if (authority.empty() || authority.find('@') != std::string::npos) return false;
  std::string host = authority;
  if (!host.empty() && host.front() == '[') {
    const auto close = host.find(']');
    if (close == std::string::npos) return false;
    host = host.substr(0, close + 1);
  } else {
    const auto colon = host.find(':');
    if (colon != std::string::npos) host = host.substr(0, colon);
  }
  if (host.empty()) return false;
  if (!secure && !is_loopback_host(host)) return false;
  out = value;
  return true;
}

// Mirrors `resolve_oauth_config`: turns the write-only settings form into the stored
// OAuth shape, staging any new client secret in the credential store. The flow,
// client auth and DPoP flag are part of the stored identity, so switching any of them
// (or the issuer or client id) drops the stored token, secret and key.
bool resolve_oauth_config(const std::string& server_id, bool is_http,
                          const nlohmann::json& requested,
                          const std::optional<nlohmann::json>& existing,
                          const nlohmann::json& http_auth_headers,
                          const CredentialStore& credentials,
                          std::optional<nlohmann::json>& resolved,
                          std::vector<std::string>& staged, std::string& reason) {
  if (requested.is_null()) {
    resolved = std::nullopt;
    return true;
  }
  if (!is_http) {
    reason = "OAuth settings require the HTTP transport";
    return false;
  }
  if (requested.is_object() && http_auth_headers.is_array()) {
    for (const auto& header : http_auth_headers) {
      if (header.is_object() &&
          ascii_lower(header.value("name", std::string())) == "authorization") {
        reason = "OAuth and a static Authorization header cannot be configured together";
        return false;
      }
    }
  }
  const auto raw_issuer = trim_ascii(requested.value("issuer", std::string()));
  std::string issuer;
  if (!raw_issuer.empty()) {
    if (!validate_oauth_url(raw_issuer, issuer)) {
      reason = "the OAuth issuer is not a valid endpoint URL";
      return false;
    }
    while (!issuer.empty() && issuer.back() == '/') issuer.pop_back();
  }
  const auto client_id = trim_ascii(requested.value("clientId", std::string()));
  if (client_id.size() > 512 || has_control_characters(client_id)) {
    reason = "the OAuth client id must be at most 512 printable characters";
    return false;
  }
  const auto flow = requested.contains("flow") ? requested.at("flow").get<std::string>()
                                               : std::string("authorization_code");
  const auto client_auth = requested.contains("clientAuth")
                               ? requested.at("clientAuth").get<std::string>()
                               : std::string("none");
  const bool needs_secret = client_auth != "none";
  if (needs_secret && client_id.empty()) {
    reason = "client secret authentication needs a hand-filled client id";
    return false;
  }
  if (client_auth == "client_secret_basic" && client_id.find(':') != std::string::npos) {
    reason = "a client_secret_basic client id must not contain a colon";
    return false;
  }
  if (!requested.contains("scopes") || !requested.at("scopes").is_array() ||
      requested.at("scopes").size() > 32) {
    reason = "OAuth may request at most 32 scopes";
    return false;
  }
  std::vector<std::string> scopes;
  for (const auto& raw : requested.at("scopes")) {
    if (!raw.is_string()) {
      reason = "each OAuth scope must be a string";
      return false;
    }
    const auto scope = trim_ascii(raw.get<std::string>());
    if (scope.empty() || scope.size() > 128 || has_control_characters(scope) ||
        scope.find_first_of(" \t\r\n") != std::string::npos) {
      reason = "each OAuth scope must be 1 to 128 non-whitespace characters";
      return false;
    }
    if (std::find(scopes.begin(), scopes.end(), scope) == scopes.end()) scopes.push_back(scope);
  }
  const bool dpop = requested.value("dpop", false);

  bool preserve = false;
  if (existing.has_value() && !issuer.empty()) {
    std::string existing_issuer = trim_ascii(existing->value("issuer", std::string()));
    while (!existing_issuer.empty() && existing_issuer.back() == '/') existing_issuer.pop_back();
    preserve = existing_issuer == issuer &&
               existing->value("clientId", std::string()) == client_id &&
               existing->value("flow", std::string("authorization_code")) == flow &&
               existing->value("clientAuth", std::string("none")) == client_auth &&
               existing->value("dpop", false) == dpop &&
               existing->value("scopes", nlohmann::json::array()) == nlohmann::json(scopes);
  }

  std::optional<std::string> client_secret_ref;
  if (needs_secret) {
    if (requested.contains("clientSecret") && requested.at("clientSecret").is_string() &&
        !requested.at("clientSecret").get<std::string>().empty()) {
      const auto secret = requested.at("clientSecret").get<std::string>();
      if (secret.size() > 8192 || has_control_characters(secret)) {
        reason = "the OAuth client secret must be 1 to 8192 characters without control characters";
        return false;
      }
      try {
        const auto reference = credentials.set(
            "mcp", server_id + "-" + next_id("oauth-secret"), Secret(std::string_view(secret)));
        staged.push_back(reference.to_string());
        client_secret_ref = reference.to_string();
      } catch (...) {
        reason = "The operating system credential store could not save the OAuth client secret";
        return false;
      }
    } else if (existing.has_value() &&
               existing->value("clientAuth", std::string("none")) == client_auth) {
      const auto carried = existing->value("clientSecretRef", std::string());
      if (!carried.empty()) client_secret_ref = carried;
    }
    if (!client_secret_ref.has_value()) {
      reason =
          "client secret authentication needs a secret; enter one, or switch the client "
          "authentication method";
      return false;
    }
  }

  nlohmann::json config = {{"issuer", issuer},
                           {"clientId", client_id},
                           {"flow", flow},
                           {"clientAuth", client_auth},
                           {"dpop", dpop},
                           {"scopes", scopes}};
  if (client_secret_ref.has_value()) config["clientSecretRef"] = *client_secret_ref;
  if (preserve) {
    for (const auto* key : {"dpopKeyRef", "tokenEndpoint", "credentialRef"}) {
      if (existing->contains(key) && existing->at(key).is_string()) {
        config[key] = existing->at(key);
      }
    }
  }
  resolved = std::move(config);
  return true;
}

// Mirrors the OAuth half of `http_auth_config_error`: describes why a stored row
// cannot be used right now, or nothing when it is fine. A row whose OAuth is
// configured but never connected is reported here instead of failing at launch.
std::optional<nlohmann::json> oauth_config_error(const nlohmann::json& config) {
  if (!config.contains("oauth") || !config.at("oauth").is_object()) return std::nullopt;
  const auto& oauth = config.at("oauth");
  const auto invalid = [](const std::string& message) {
    return nlohmann::json{{"code", "invalid_config"}, {"message", message}};
  };
  if (config.value("transport", std::string()) != "http") {
    return invalid("stdio transport cannot carry OAuth authentication settings");
  }
  for (const auto& header : config.value("httpAuthHeaders", nlohmann::json::array())) {
    if (header.is_object() &&
        ascii_lower(header.value("name", std::string())) == "authorization") {
      return invalid("OAuth and a static Authorization header are mutually exclusive");
    }
  }
  const auto issuer = trim_ascii(oauth.value("issuer", std::string()));
  if (!issuer.empty()) {
    std::string unused;
    if (!validate_oauth_url(issuer, unused)) {
      return invalid("the stored OAuth issuer is not a valid endpoint URL");
    }
  }
  const auto client_id = oauth.value("clientId", std::string());
  if (client_id.size() > 512 || has_control_characters(client_id)) {
    return invalid("the stored OAuth client id is too long or contains control characters");
  }
  if (oauth.value("clientAuth", std::string("none")) != "none") {
    if (trim_ascii(client_id).empty()) {
      return invalid("client secret authentication needs a hand-filled client id");
    }
    if (!oauth.contains("clientSecretRef") || !oauth.at("clientSecretRef").is_string() ||
        oauth.at("clientSecretRef").get<std::string>().empty()) {
      return invalid("OAuth uses client secret authentication but no secret is stored");
    }
  }
  const bool has_endpoint =
      oauth.contains("tokenEndpoint") && oauth.at("tokenEndpoint").is_string() &&
      !oauth.at("tokenEndpoint").get<std::string>().empty();
  const bool has_token =
      oauth.contains("credentialRef") && oauth.at("credentialRef").is_string() &&
      !oauth.at("credentialRef").get<std::string>().empty();
  if (!has_endpoint && !has_token) {
    return invalid("OAuth is configured but not connected; run Connect OAuth");
  }
  if (has_endpoint != has_token) {
    return invalid("the stored OAuth token endpoint and credential reference must both be present");
  }
  if (trim_ascii(client_id).empty()) {
    return invalid("the connected OAuth configuration has no client id");
  }
  std::string unused;
  if (!validate_oauth_url(oauth.at("tokenEndpoint").get<std::string>(), unused)) {
    return invalid("the stored OAuth token endpoint is not a valid endpoint URL");
  }
  return std::nullopt;
}

bool proxy_credential_has_control_characters(const std::string& value) {
  for (std::size_t index = 0; index < value.size(); ++index) {
    const auto byte = static_cast<unsigned char>(value[index]);
    if (byte < 0x20 || byte == 0x7f) return true;
    // U+0080..U+009F are the C1 controls; Rust's `char::is_control` rejects them too.
    if (byte == 0xc2 && index + 1 < value.size()) {
      const auto next = static_cast<unsigned char>(value[index + 1]);
      if (next >= 0x80 && next <= 0x9f) return true;
    }
  }
  return false;
}

// Mirrors `ProxyCredential::new`: a credential is `user:password`, at most 1024 bytes,
// with a non-empty user and no control characters.
bool valid_proxy_credential(const std::string& value, std::string& reason) {
  if (value.size() > 1'024) {
    reason = "proxy credential is longer than 1024 characters";
    return false;
  }
  const auto colon = value.find(':');
  if (colon == std::string::npos || colon == 0) {
    reason = "proxy credential must be `user:password` without control characters";
    return false;
  }
  if (proxy_credential_has_control_characters(value)) {
    reason = "proxy credential must be `user:password` without control characters";
    return false;
  }
  return true;
}

nlohmann::json describe_network_proxy(const DesktopNetworkProxyConfig& config,
                                      const CredentialStore& credentials) {
  bool has_credential = false;
  if (config.credential_ref) {
    has_credential = credentials.has(CredentialReference::parse(*config.credential_ref));
  }
  return {{"mode", config.mode},
          {"hasCredential", has_credential},
          {"resolution", resolve_network_proxy(config.mode == "system")}};
}

// The write-only proxy credential never survives the command call.
class NetworkProxyCredentialScrubber {
 public:
  explicit NetworkProxyCredentialScrubber(nlohmann::json& params) : params_(params) {}
  ~NetworkProxyCredentialScrubber() {
    if (!params_.is_object() || !params_.contains("input") || !params_.at("input").is_object()) {
      return;
    }
    auto& input = params_.at("input");
    if (!input.contains("credential") || !input.at("credential").is_string()) return;
    auto& value = input["credential"].get_ref<std::string&>();
    volatile char* bytes = value.data();
    for (std::size_t index = 0; index < value.size(); ++index) bytes[index] = 0;
  }

 private:
  nlohmann::json& params_;
};

class IpcSecretParamScrubber {
 public:
  explicit IpcSecretParamScrubber(nlohmann::json& params) : params_(params) {}
  ~IpcSecretParamScrubber() {
    if (!params_.contains("authentication") ||
        !params_.at("authentication").is_object()) return;
    auto& authentication = params_["authentication"];
    for (const auto* field : {"password", "privateKeyPem", "passphrase"}) {
      if (!authentication.contains(field) || !authentication.at(field).is_string()) continue;
      auto& value = authentication[field].get_ref<std::string&>();
      volatile char* bytes = value.data();
      for (std::size_t index = 0; index < value.size(); ++index) bytes[index] = 0;
    }
  }

 private:
  nlohmann::json& params_;
};

#ifdef _WIN32
std::wstring utf8_to_wide(const std::string& value) {
  const auto length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                                          value.data(), static_cast<int>(value.size()),
                                          nullptr, 0);
  if (length <= 0) return {};
  std::wstring result(static_cast<size_t>(length), L'\0');
  if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                          static_cast<int>(value.size()), result.data(), length) <= 0) {
    return {};
  }
  return result;
}
#endif

std::optional<std::string> read_environment(const char* name) {
#ifdef _WIN32
  const auto required = GetEnvironmentVariableA(name, nullptr, 0);
  if (required == 0) return std::nullopt;
  std::string value(required, '\0');
  const auto written = GetEnvironmentVariableA(name, value.data(), required);
  if (written == 0 || written >= required) return std::nullopt;
  value.resize(written);
  return value;
#else
  const auto* value = std::getenv(name);
  if (value == nullptr) return std::nullopt;
  return std::string(value);
#endif
}

/* ── Investigation host RPC (Agent → host) ──────────────────────────────── */

constexpr const char* kToolServerExec = "server.exec";
constexpr const char* kToolServerInfo = "server.info";
constexpr const char* kToolServerLogs = "server.logs";
constexpr const char* kToolServerServices = "server.services";
constexpr const char* kToolDockerPs = "docker.ps";
constexpr const char* kToolDockerLogs = "docker.logs";
constexpr const char* kToolDockerInspect = "docker.inspect";
constexpr const char* kToolDockerRestart = "docker.restart";
constexpr const char* kToolSystemdInspect = "systemd.inspect";
constexpr const char* kToolSystemdRestart = "systemd.restart";
constexpr const char* kToolPackageInspect = "package.inspect";
constexpr const char* kToolPackageInstall = "package.install";
constexpr const char* kToolFilesystemRead = "filesystem.read";
constexpr const char* kToolFilesystemBackup = "filesystem.backup";
constexpr const char* kToolFilesystemBackupList = "filesystem.backup.list";
constexpr const char* kToolFilesystemBackupRetention = "filesystem.backup.retention";
constexpr const char* kToolFilesystemBackupCleanup = "filesystem.backup.cleanup";
constexpr const char* kToolFilesystemRestore = "filesystem.restore";
constexpr const char* kToolFilesystemWrite = "filesystem.write";
constexpr const char* kToolFilesystemEdit = "filesystem.edit";
constexpr const char* kToolInvestigationArtifact = "investigation.artifact";
constexpr std::size_t kMaxHostToolReplayCache = 256;

nlohmann::json investigation_error(std::string code, std::string message, bool retryable) {
  return {{"code", std::move(code)}, {"message", std::move(message)}, {"retryable", retryable}};
}

nlohmann::json record_failure_json(std::string code, std::string message, bool retryable) {
  return {{"recorded", false},
          {"error", investigation_error(std::move(code), std::move(message), retryable)}};
}

std::string json_text_field(const nlohmann::json& value, const char* key,
                            const char* fallback = "") {
  if (!value.is_object()) return fallback;
  const auto found = value.find(key);
  if (found == value.end() || !found->is_string()) return fallback;
  return found->get<std::string>();
}

bool json_bool_field(const nlohmann::json& value, const char* key, bool fallback = false) {
  if (!value.is_object()) return fallback;
  const auto found = value.find(key);
  if (found == value.end() || !found->is_boolean()) return fallback;
  return found->get<bool>();
}

std::optional<nlohmann::json> load_investigation_task(DesktopDatabase& database,
                                                      const std::string& task_id) {
  auto detail = database.get_investigation_task(task_id);
  if (!detail.has_value()) return std::nullopt;
  const auto found = detail->find("task");
  if (found == detail->end()) return std::nullopt;
  return *found;
}

/** Whether two ToolTarget-shaped values denote the same place. */
bool same_investigation_target(const nlohmann::json& left, const nlohmann::json& right) {
  if (!left.is_object() || !right.is_object()) return false;
  return json_text_field(left, "host") == json_text_field(right, "host") &&
         json_text_field(left, "serverId") == json_text_field(right, "serverId") &&
         json_text_field(left, "workspaceId") == json_text_field(right, "workspaceId") &&
         json_text_field(left, "environment") == json_text_field(right, "environment");
}

std::optional<std::int64_t> parse_iso8601_utc_seconds(const std::string& value) {
  if (value.size() < 19) return std::nullopt;
  std::tm utc{};
  std::istringstream stream(value);
  stream >> std::get_time(&utc, "%Y-%m-%dT%H:%M:%S");
  if (stream.fail()) return std::nullopt;
#ifdef _WIN32
  const auto seconds = _mkgmtime(&utc);
#else
  const auto seconds = timegm(&utc);
#endif
  if (seconds == static_cast<std::time_t>(-1)) return std::nullopt;
  std::int64_t result = static_cast<std::int64_t>(seconds);
  // A trailing timezone offset (`+HH:MM`/`-HH:MM`) adjusts the instant; `Z`, a
  // fractional part, or end-of-string all mean UTC.
  if (value.size() > 19 && (value[19] == '+' || value[19] == '-')) {
    if (value.size() < 25) return std::nullopt;
    const int hours = std::stoi(value.substr(20, 2));
    const int minutes = std::stoi(value.substr(23, 2));
    const std::int64_t offset = static_cast<std::int64_t>(hours) * 3600 + minutes * 60;
    result += value[19] == '+' ? -offset : offset;
  }
  return result;
}

std::string iso8601_from_epoch(std::int64_t epoch_seconds) {
  const auto time = static_cast<std::time_t>(epoch_seconds);
  std::tm utc{};
#ifdef _WIN32
  gmtime_s(&utc, &time);
#else
  gmtime_r(&time, &utc);
#endif
  std::ostringstream rendered;
  rendered << std::put_time(&utc, "%Y-%m-%dT%H:%M:%SZ");
  return rendered.str();
}

std::int64_t now_epoch_seconds() {
  return static_cast<std::int64_t>(std::chrono::duration_cast<std::chrono::seconds>(
                                       std::chrono::system_clock::now().time_since_epoch())
                                       .count());
}

// The parts of a plan that require a fresh approval. Runtime progress is host-owned
// and excluded, so an idempotent replay cannot reset or widen an approved plan.
nlohmann::json plan_definition(const nlohmann::json& plan) {
  nlohmann::json value = plan;
  if (!value.is_object()) return value;
  for (const char* key : {"revision", "status", "createdAt", "updatedAt", "currentStepId",
                          "approval"}) {
    value.erase(key);
  }
  if (value.contains("steps") && value.at("steps").is_array()) {
    for (auto& step : value["steps"]) {
      if (!step.is_object()) continue;
      for (const char* key : {"attempts", "status", "startedAt", "endedAt", "lastDeviation"}) {
        step.erase(key);
      }
    }
  }
  if (value.contains("observationWindow") && value.at("observationWindow").is_object()) {
    auto& window = value["observationWindow"];
    for (const char* key : {"status", "sampleCount", "startedAt", "deadlineAt",
                            "deadlineEpochSeconds", "lastSampleAt", "lastSampleEpochSeconds",
                            "lastFailure"}) {
      window.erase(key);
    }
  }
  return value;
}

void merge_plan_runtime(nlohmann::json& plan, const nlohmann::json& previous) {
  plan["revision"] = previous.value("revision", std::int64_t{1});
  plan["status"] = previous.value("status", std::string("active"));
  plan["createdAt"] = previous.value("createdAt", std::string());
  if (previous.contains("currentStepId")) {
    plan["currentStepId"] = previous.at("currentStepId");
  } else {
    plan.erase("currentStepId");
  }
  if (previous.contains("approval")) {
    plan["approval"] = previous.at("approval");
  } else {
    plan.erase("approval");
  }
  if (!plan.contains("steps") || !plan.at("steps").is_array() || !previous.contains("steps") ||
      !previous.at("steps").is_array()) {
    return;
  }
  for (auto& step : plan["steps"]) {
    const auto id = step.value("id", std::string());
    for (const auto& prior : previous.at("steps")) {
      if (prior.value("id", std::string()) != id) continue;
      step["attempts"] = prior.value("attempts", 0);
      step["status"] = prior.value("status", std::string("pending"));
      for (const char* key : {"startedAt", "endedAt", "lastDeviation"}) {
        if (prior.contains(key)) {
          step[key] = prior.at(key);
        } else {
          step.erase(key);
        }
      }
      break;
    }
  }
}

void normalize_new_plan_runtime(nlohmann::json& plan, const std::string& now) {
  plan["status"] = "active";
  plan["createdAt"] = now;
  plan["updatedAt"] = now;
  if (plan.contains("steps") && plan.at("steps").is_array() && !plan.at("steps").empty()) {
    plan["currentStepId"] = plan.at("steps")[0].value("id", std::string());
  }
  int index = 0;
  for (auto& step : plan["steps"]) {
    step["attempts"] = 0;
    step["status"] = index == 0 ? "running" : "pending";
    if (index == 0) {
      step["startedAt"] = now;
    } else {
      step.erase("startedAt");
    }
    step.erase("endedAt");
    step.erase("lastDeviation");
    ++index;
  }
}

nlohmann::json plan_deviation_json(const std::string& code, const std::string& action,
                                   const std::string& message, const std::string& tool_name,
                                   const std::string& at,
                                   const std::optional<std::string>& plan_id,
                                   const std::optional<std::string>& step_id) {
  nlohmann::json deviation = {{"code", code},
                              {"action", action},
                              {"message", message},
                              {"toolName", tool_name},
                              {"at", at}};
  if (plan_id.has_value()) deviation["planId"] = *plan_id;
  if (step_id.has_value()) deviation["stepId"] = *step_id;
  return deviation;
}

std::string normalize_guardrail_path(const std::string& value) {
  std::vector<std::string> parts;
  std::stringstream stream(value);
  std::string part;
  while (std::getline(stream, part, '/')) {
    if (part.empty() || part == ".") continue;
    if (part == "..") {
      if (!parts.empty()) parts.pop_back();
      continue;
    }
    parts.push_back(part);
  }
  if (parts.empty()) return "/";
  std::string result;
  for (const auto& item : parts) {
    result += "/";
    result += item;
  }
  return result;
}

bool path_is_under_prefix(const std::string& path, const std::string& prefix) {
  const auto normalized_path = normalize_guardrail_path(path);
  const auto normalized_prefix = normalize_guardrail_path(prefix);
  return normalized_prefix == "/" || normalized_path == normalized_prefix ||
         normalized_path.rfind(normalized_prefix + "/", 0) == 0;
}

void collect_input_path_values(const nlohmann::json& input, std::vector<std::string>& out) {
  if (!input.is_object()) return;
  for (const char* key : {"path", "backupPath", "workdir"}) {
    const auto found = input.find(key);
    if (found != input.end() && found->is_string()) out.push_back(found->get<std::string>());
  }
  const auto items = input.find("items");
  if (items != input.end() && items->is_array()) {
    for (const auto& item : *items) collect_input_path_values(item, out);
  }
}

std::optional<nlohmann::json> task_guardrail_violation(const nlohmann::json& task,
                                                        const std::string& tool_name,
                                                        const nlohmann::json& input) {
  const auto guardrails = task.value("guardrails", nlohmann::json::object());
  if (!guardrails.is_object()) return std::nullopt;
  const auto now = now_epoch_seconds();
  const auto not_before = json_text_field(guardrails, "notBeforeAt");
  if (!not_before.empty()) {
    const auto parsed = parse_iso8601_utc_seconds(not_before);
    if (!parsed.has_value()) {
      return plan_deviation_json("outside_time_window", "wait_user",
                                 "task guardrail notBeforeAt is invalid; execution is blocked",
                                 tool_name, iso8601_now(), std::nullopt, std::nullopt);
    }
    if (now < *parsed) {
      return plan_deviation_json("outside_time_window", "wait_user",
                                 "task execution is not allowed before " + not_before, tool_name,
                                 iso8601_now(), std::nullopt, std::nullopt);
    }
  }
  const auto expires = json_text_field(guardrails, "expiresAt");
  if (!expires.empty()) {
    const auto parsed = parse_iso8601_utc_seconds(expires);
    if (!parsed.has_value()) {
      return plan_deviation_json("outside_time_window", "wait_user",
                                 "task guardrail expiresAt is invalid; execution is blocked",
                                 tool_name, iso8601_now(), std::nullopt, std::nullopt);
    }
    if (now >= *parsed) {
      return plan_deviation_json("outside_time_window", "wait_user",
                                 "task execution time window expired at " + expires, tool_name,
                                 iso8601_now(), std::nullopt, std::nullopt);
    }
  }
  const auto forbidden_tools = guardrails.find("forbiddenTools");
  if (forbidden_tools != guardrails.end() && forbidden_tools->is_array()) {
    for (const auto& entry : *forbidden_tools) {
      if (entry.is_string() && entry.get<std::string>() == tool_name) {
        return plan_deviation_json("scope_forbidden", "wait_user",
                                   "tool `" + tool_name + "` is forbidden by the task guardrails",
                                   tool_name, iso8601_now(), std::nullopt, std::nullopt);
      }
    }
  }
  const auto forbidden_prefixes = guardrails.find("forbiddenPathPrefixes");
  if (forbidden_prefixes != guardrails.end() && forbidden_prefixes->is_array()) {
    std::vector<std::string> paths;
    collect_input_path_values(input, paths);
    for (const auto& prefix : *forbidden_prefixes) {
      if (!prefix.is_string()) continue;
      for (const auto& path : paths) {
        if (path_is_under_prefix(path, prefix.get<std::string>())) {
          return plan_deviation_json(
              "scope_forbidden", "wait_user",
              "path `" + path + "` is forbidden by the task guardrail prefix `" +
                  prefix.get<std::string>() + "`",
              tool_name, iso8601_now(), std::nullopt, std::nullopt);
        }
      }
    }
  }
  return std::nullopt;
}

std::string validate_task_guardrails_for_plan_step(const nlohmann::json& task,
                                                   const nlohmann::json& step) {
  const auto guardrails = task.value("guardrails", nlohmann::json::object());
  const auto forbidden = guardrails.contains("forbiddenTools") &&
                                 guardrails.at("forbiddenTools").is_array()
                             ? guardrails.at("forbiddenTools")
                             : nlohmann::json::array();
  const auto tools = step.contains("allowedTools") && step.at("allowedTools").is_array()
                         ? step.at("allowedTools")
                         : nlohmann::json::array();
  const auto step_id = json_text_field(step, "id");
  if (!forbidden.empty()) {
    for (const auto& tool : tools) {
      if (tool.is_string() && tool.get<std::string>() == "*") {
        return "plan step `" + step_id +
               "` uses wildcard tools while task guardrails forbid specific tools; enumerate "
               "only allowed tools";
      }
    }
  }
  for (const auto& tool : tools) {
    if (!tool.is_string()) continue;
    for (const auto& banned : forbidden) {
      if (banned.is_string() && banned.get<std::string>() == tool.get<std::string>()) {
        return "plan step `" + step_id + "` allows tool `" + tool.get<std::string>() +
               "`, which is forbidden by the task guardrails";
      }
    }
  }
  const auto bindings = step.find("inputBindings");
  if (bindings != step.end() && bindings->is_object()) {
    nlohmann::json input = nlohmann::json::object();
    for (auto it = bindings->begin(); it != bindings->end(); ++it) {
      if (!it.value().is_string()) {
        input[it.key()] = it.value();
        continue;
      }
      const auto raw = it.value().get<std::string>();
      nlohmann::json parsed;
      try {
        parsed = nlohmann::json::parse(raw);
      } catch (const nlohmann::json::exception&) {
        input[it.key()] = raw;
        continue;
      }
      input[it.key()] = (parsed.is_array() || parsed.is_object()) ? parsed : nlohmann::json(raw);
    }
    auto violation = task_guardrail_violation(task, "plan.binding", input);
    if (violation.has_value() && violation->value("code", std::string()) == "scope_forbidden") {
      return "plan step `" + step_id + "` violates task guardrails: " +
             violation->value("message", std::string());
    }
  }
  return "";
}

std::string validate_input_bindings(const nlohmann::json& step) {
  const auto found = step.find("inputBindings");
  if (found == step.end() || found->is_null()) return "";
  const auto step_id = json_text_field(step, "id");
  if (!found->is_object()) return "plan step `" + step_id + "` has an invalid input binding";
  if (found->size() > 8) return "plan step `" + step_id + "` has too many input bindings";
  for (auto it = found->begin(); it != found->end(); ++it) {
    if (trim_ascii(it.key()).empty() || utf8_character_count(it.key()) > 64) {
      return "plan step `" + step_id + "` has an invalid input binding";
    }
    if (!it.value().is_string()) return "plan step `" + step_id + "` has an invalid input binding";
    const auto text = it.value().get<std::string>();
    if (text.empty() || utf8_character_count(text) > 16384) {
      return "plan step `" + step_id + "` has an invalid input binding";
    }
  }
  return "";
}

std::string validate_observation_window(const nlohmann::json& window,
                                        const nlohmann::json& steps) {
  const auto duration = window.value("durationSeconds", 0);
  if (duration == 0 || duration > 86400) {
    return "observation duration must be between 1 and 86400 seconds";
  }
  const auto interval = window.value("intervalSeconds", 0);
  if (interval == 0 || interval > duration) {
    return "observation interval must be between 1 and the duration";
  }
  const auto tools =
      window.contains("allowedTools") && window.at("allowedTools").is_array()
          ? window.at("allowedTools")
          : nlohmann::json::array();
  if (tools.empty() || tools.size() > 32) {
    return "observation window needs 1 to 32 allowed read tools";
  }
  const auto criteria =
      window.contains("successCriteria") && window.at("successCriteria").is_array()
          ? window.at("successCriteria")
          : nlohmann::json::array();
  if (criteria.empty() || criteria.size() > 16) {
    return "observation window needs 1 to 16 success criteria";
  }
  for (const auto& tool : tools) {
    if (!tool.is_string() || trim_ascii(tool.get<std::string>()).empty() ||
        tool.get<std::string>().size() > 256) {
      return "observation tools must be bounded names";
    }
  }
  for (const auto& criterion : criteria) {
    if (!criterion.is_string() || trim_ascii(criterion.get<std::string>()).empty() ||
        utf8_character_count(criterion.get<std::string>()) > 1024) {
      return "observation success criteria must be bounded text";
    }
  }
  if (!steps.is_array() || steps.empty()) {
    return "observation window requires the final plan step to be verification";
  }
  const auto& verification = steps.back();
  if (json_text_field(verification, "kind") != "verification") {
    return "observation window requires the final plan step to be verification";
  }
  const auto allowed = verification.value("allowedTools", nlohmann::json::array());
  for (const auto& tool : tools) {
    bool matched = false;
    if (allowed.is_array()) {
      for (const auto& candidate : allowed) {
        if (candidate.is_string() && candidate == tool) {
          matched = true;
          break;
        }
      }
    }
    if (!matched) return "observation tools must be allowed by the final verification step";
  }
  return "";
}

bool auto_medium_tools_allowed(const nlohmann::json& tools) {
  if (!tools.is_array()) return false;
  const auto name_allowed = [](const std::string& name) {
    return name == kToolFilesystemBackup || name == kToolFilesystemEdit ||
           name == kToolFilesystemWrite;
  };
  if (tools.size() == 1 && tools[0].is_string()) return name_allowed(tools[0].get<std::string>());
  if (tools.size() == 2) {
    bool has_backup = false;
    bool has_write = false;
    for (const auto& tool : tools) {
      if (!tool.is_string()) return false;
      const auto name = tool.get<std::string>();
      if (!name_allowed(name)) return false;
      if (name == kToolFilesystemBackup) has_backup = true;
      if (name == kToolFilesystemWrite || name == kToolFilesystemEdit) has_write = true;
    }
    return has_backup && has_write;
  }
  return false;
}

bool task_allows_auto_medium_action(const nlohmann::json& task, const nlohmann::json& step) {
  const auto target = step.contains("target") && step.at("target").is_object()
                          ? step.at("target")
                          : task.value("scope", nlohmann::json::object());
  const auto environment = json_text_field(target, "environment");
  return json_text_field(task, "permissionMode") == "auto" &&
         json_text_field(task, "mode") == "goal" &&
         json_text_field(task, "automationLevel") == "execute" &&
         json_text_field(target, "host") == "remote" &&
         (environment == "development" || environment == "staging") &&
         json_text_field(step, "riskLevel") == "medium" &&
         auto_medium_tools_allowed(step.value("allowedTools", nlohmann::json::array()));
}

std::string validate_playbook_step(const nlohmann::json& step) {
  const auto step_id = json_text_field(step, "id");
  const auto kind = json_text_field(step, "kind");
  if (json_bool_field(step, "requiresBaseline") && kind != "action") {
    return "only action step `" + step_id + "` may require a baseline";
  }
  if (kind != "action") return "";
  if (trim_ascii(json_text_field(step, "preview")).empty()) {
    return "action step `" + step_id + "` must include a bounded preview";
  }
  const auto has_nonempty = [&step](const char* key) {
    const auto found = step.find(key);
    return found != step.end() && found->is_array() && !found->empty();
  };
  if (!has_nonempty("verificationCriteria") && !has_nonempty("successCriteria")) {
    return "action step `" + step_id + "` must declare verification criteria";
  }
  const auto risk = json_text_field(step, "riskLevel");
  if ((risk == "high" || risk == "critical") && !json_bool_field(step, "requiresApproval")) {
    return "high-risk action step `" + step_id + "` must require approval";
  }
  if (json_text_field(step, "idempotency") == "unsafe" &&
      trim_ascii(json_text_field(step, "rollback")).empty()) {
    return "unsafe action step `" + step_id + "` must declare rollback details";
  }
  return "";
}

bool plan_binding_matches(const std::optional<nlohmann::json>& plan,
                          const nlohmann::json& step,
                          const std::optional<std::string>& expected_plan_id,
                          const std::optional<std::string>& expected_step_id,
                          const std::optional<nlohmann::json>& expected_evidence_ids) {
  if (!expected_plan_id.has_value() && !expected_step_id.has_value() &&
      !expected_evidence_ids.has_value()) {
    return true;
  }
  if (!plan.has_value()) return false;
  return expected_plan_id == std::optional<std::string>(json_text_field(*plan, "id")) &&
         expected_step_id == std::optional<std::string>(json_text_field(step, "id")) &&
         expected_evidence_ids ==
             std::optional<nlohmann::json>(step.value("evidenceIds", nlohmann::json::array()));
}

bool input_bindings_match(const nlohmann::json& step, const nlohmann::json& input) {
  const auto found = step.find("inputBindings");
  if (found == step.end() || found->is_null() || !found->is_object()) {
    return found == step.end() || found->is_null();
  }
  if (!input.is_object()) return false;
  for (auto it = found->begin(); it != found->end(); ++it) {
    if (!it.value().is_string()) return false;
    const auto expected = it.value().get<std::string>();
    const auto actual = input.find(it.key());
    if (actual == input.end()) return false;
    if (actual->is_string()) {
      if (actual->get<std::string>() != expected) return false;
      continue;
    }
    if (actual->is_array() || actual->is_object()) {
      try {
        if (nlohmann::json::parse(expected) == *actual) continue;
      } catch (const nlohmann::json::exception&) {
      }
      return false;
    }
    return false;
  }
  return true;
}

bool baseline_artifact_matches_plan(const nlohmann::json& artifacts,
                                    const nlohmann::json& plan) {
  if (!artifacts.is_array()) return false;
  const auto plan_id = json_text_field(plan, "id");
  const auto steps = plan.value("steps", nlohmann::json::array());
  for (const auto& artifact : artifacts) {
    if (json_text_field(artifact, "kind") != "baseline") continue;
    const auto status = json_text_field(artifact, "status");
    if (status != "ready" && status != "succeeded") continue;
    if (json_text_field(artifact, "planId") != plan_id) continue;
    const auto evidence = artifact.value("evidenceIds", nlohmann::json::array());
    if (!evidence.is_array() || evidence.empty()) continue;
    const auto plan_step_id = json_text_field(artifact, "planStepId");
    if (plan_step_id.empty() || !steps.is_array()) continue;
    for (const auto& step : steps) {
      if (json_text_field(step, "id") == plan_step_id &&
          json_text_field(step, "kind") == "evidence" &&
          json_text_field(step, "status") == "succeeded") {
        return true;
      }
    }
  }
  return false;
}

/** Returns the deviation to report, or `nullopt` when the call is allowed. */
std::optional<nlohmann::json> check_plan_for_tool(
    DesktopDatabase& database, const nlohmann::json& task, const std::string& tool_name,
    const nlohmann::json& target, const nlohmann::json& input,
    const std::optional<std::string>& binding_plan_id,
    const std::optional<std::string>& binding_step_id,
    const std::optional<nlohmann::json>& binding_evidence_ids) {
  if (auto violation = task_guardrail_violation(task, tool_name, input);
      violation.has_value()) {
    return violation;
  }
  const auto at = iso8601_now();
  const auto task_id = json_text_field(task, "id");
  auto plan = database.latest_investigation_plan(task_id);
  if (!plan.has_value()) {
    return plan_deviation_json(
        "missing_plan", "replan",
        "durable task has no active plan; create investigation.plan before using a task tool",
        tool_name, at, std::nullopt, std::nullopt);
  }
  const auto current_step_id = json_text_field(*plan, "currentStepId");
  if (current_step_id.empty()) {
    return plan_deviation_json("no_active_step", "replan", "active plan has no current step",
                               tool_name, at, json_text_field(*plan, "id"), std::nullopt);
  }
  const auto steps = plan->value("steps", nlohmann::json::array());
  const nlohmann::json* step = nullptr;
  for (const auto& candidate : steps) {
    if (json_text_field(candidate, "id") == current_step_id) {
      step = &candidate;
      break;
    }
  }
  if (step == nullptr) {
    return plan_deviation_json("no_active_step", "replan",
                               "active plan points to a missing current step", tool_name, at,
                               json_text_field(*plan, "id"), current_step_id);
  }
  const auto plan_id = json_text_field(*plan, "id");
  const auto step_id = json_text_field(*step, "id");
  const auto step_kind = json_text_field(*step, "kind");
  if (tool_name == kToolServerExec && step_kind != "action") {
    return plan_deviation_json("tool_not_allowed", "deny",
                               "server.exec is available only in an action plan step", tool_name,
                               at, plan_id, step_id);
  }
  if (!plan_binding_matches(plan, *step, binding_plan_id, binding_step_id,
                            binding_evidence_ids)) {
    return plan_deviation_json(
        "binding_mismatch", "deny",
        "tool request is not bound to the plan revision, current step and evidence set returned "
        "by the host",
        tool_name, at, plan_id, step_id);
  }
  const auto step_status = json_text_field(*step, "status");
  if (step_status != "pending" && step_status != "running") {
    return plan_deviation_json("no_active_step", "replan",
                               "current plan step is no longer runnable", tool_name, at, plan_id,
                               step_id);
  }
  if (step->value("attempts", 0) >= step->value("maxAttempts", 1)) {
    return plan_deviation_json("step_budget_exhausted", "wait_user",
                               "current plan step has exhausted its retry budget", tool_name, at,
                               plan_id, step_id);
  }
  if (plan->contains("observationWindow") && plan->at("observationWindow").is_object() &&
      json_text_field(plan->at("observationWindow"), "status") == "running") {
    const auto allowed = plan->at("observationWindow").value("allowedTools", nlohmann::json::array());
    bool matched = false;
    if (allowed.is_array()) {
      for (const auto& candidate : allowed) {
        if (candidate.is_string() && candidate.get<std::string>() == tool_name) matched = true;
      }
    }
    if (step_kind != "verification" || !matched) {
      return plan_deviation_json(
          "tool_not_allowed", "wait_user",
          "observation window only permits its declared verification read tools", tool_name, at,
          plan_id, step_id);
    }
  }
  if (step_kind == "action") {
    const auto automation = json_text_field(task, "automationLevel");
    const auto approval = plan->contains("approval") && plan->at("approval").is_object()
                              ? json_text_field(plan->at("approval"), "status")
                              : std::string();
    if (automation == "readonly") {
      return plan_deviation_json(
          "tool_not_allowed", "deny",
          "this task is read-only; action steps are available for planning but cannot execute",
          tool_name, at, plan_id, step_id);
    }
    if (automation == "propose" && approval != "approved") {
      return plan_deviation_json(
          "no_active_step", "wait_user",
          "proposal-mode action is waiting for the user to select and approve a decision option",
          tool_name, at, plan_id, step_id);
    }
  }
  if (step_kind == "action" && json_bool_field(*step, "requiresBaseline")) {
    auto artifacts = database.list_investigation_artifacts(task_id, 128);
    if (!baseline_artifact_matches_plan(artifacts.value("artifacts", nlohmann::json::array()),
                                        *plan)) {
      return plan_deviation_json(
          "evidence_missing", "replan",
          "current action step requires a ready baseline artifact bound to this plan revision",
          tool_name, at, plan_id, step_id);
    }
  }
  const auto allowed_tools = step->value("allowedTools", nlohmann::json::array());
  bool tool_allowed = false;
  if (allowed_tools.is_array()) {
    for (const auto& allowed : allowed_tools) {
      if (allowed.is_string() &&
          (allowed.get<std::string>() == "*" || allowed.get<std::string>() == tool_name)) {
        tool_allowed = true;
        break;
      }
    }
  }
  if (tool_name != kToolInvestigationArtifact && !tool_allowed) {
    return plan_deviation_json("tool_not_allowed", "replan",
                               "tool `" + tool_name + "` is not allowed by the current plan step",
                               tool_name, at, plan_id, step_id);
  }
  if (!input_bindings_match(*step, input)) {
    return plan_deviation_json(
        "binding_mismatch", "deny",
        "tool input does not match the exact arguments bound by the active plan step", tool_name,
        at, plan_id, step_id);
  }
  const auto expected = step->contains("target") && step->at("target").is_object()
                            ? step->at("target")
                            : task.value("scope", nlohmann::json::object());
  if (!same_investigation_target(expected, target)) {
    return plan_deviation_json("target_mismatch", "wait_user",
                               "tool target does not match the active plan step scope", tool_name,
                               at, plan_id, step_id);
  }
  const auto evidence_ids = step->value("evidenceIds", nlohmann::json::array());
  if (evidence_ids.is_array()) {
    for (const auto& evidence_id : evidence_ids) {
      if (!evidence_id.is_string()) continue;
      auto evidence = database.get_investigation_evidence(evidence_id.get<std::string>());
      if (!evidence.has_value() || json_text_field(*evidence, "taskId") != task_id) {
        return plan_deviation_json(
            "evidence_missing", "replan",
            "plan step requires missing evidence `" + evidence_id.get<std::string>() + "`",
            tool_name, at, plan_id, step_id);
      }
    }
  }
  return std::nullopt;
}

/* ── Host tool call idempotency (durable call ledger + bounded replay) ─────── */

nlohmann::json host_tool_failed(const std::string& code, const std::string& message,
                                bool retryable,
                                const std::optional<nlohmann::json>& detail) {
  nlohmann::json error = investigation_error(code, message, retryable);
  if (detail.has_value()) error["detail"] = *detail;
  return {{"status", "failed"}, {"error", std::move(error)}};
}

nlohmann::json host_tool_success(const nlohmann::json& output) {
  return {{"status", "success"}, {"output", output}};
}

nlohmann::json optional_json_field(const nlohmann::json& value, const char* key) {
  const auto found = value.find(key);
  if (found == value.end() || found->is_null()) return nullptr;
  return *found;
}

bool present_json_text(const nlohmann::json& value, const char* key) {
  const auto found = value.find(key);
  return found != value.end() && found->is_string();
}

nlohmann::json host_tool_scope_json(const nlohmann::json& target) {
  nlohmann::json scope = nlohmann::json::object();
  scope["host"] = json_text_field(target, "host");
  scope["serverId"] = optional_json_field(target, "serverId");
  scope["workspaceId"] = optional_json_field(target, "workspaceId");
  scope["environment"] = json_text_field(target, "environment");
  return scope;
}

std::string host_tool_request_fingerprint(const nlohmann::json& request) {
  nlohmann::json canonical = nlohmann::json::object();
  canonical["runId"] = optional_json_field(request, "runId");
  canonical["traceId"] = json_text_field(request, "traceId");
  canonical["callId"] = json_text_field(request, "callId");
  canonical["toolName"] = json_text_field(request, "toolName");
  canonical["input"] = request.contains("input") ? request.at("input") : nlohmann::json::object();
  canonical["target"] = host_tool_scope_json(request.value("target", nlohmann::json::object()));
  canonical["taskId"] = optional_json_field(request, "taskId");
  canonical["planId"] = optional_json_field(request, "planId");
  canonical["planStepId"] = optional_json_field(request, "planStepId");
  canonical["evidenceIds"] = optional_json_field(request, "evidenceIds");
  return content_revision_hex(canonical.dump());
}

std::string host_tool_action_fingerprint(const nlohmann::json& request) {
  nlohmann::json canonical = nlohmann::json::object();
  canonical["toolName"] = json_text_field(request, "toolName");
  canonical["input"] = request.contains("input") ? request.at("input") : nlohmann::json::object();
  canonical["target"] = host_tool_scope_json(request.value("target", nlohmann::json::object()));
  canonical["taskId"] = optional_json_field(request, "taskId");
  canonical["planId"] = optional_json_field(request, "planId");
  canonical["planStepId"] = optional_json_field(request, "planStepId");
  canonical["evidenceIds"] = optional_json_field(request, "evidenceIds");
  return content_revision_hex(canonical.dump());
}

bool host_tool_is_effectful(const std::string& tool_name) {
  return is_one_of(tool_name, {kToolDockerRestart, kToolServerExec, kToolSystemdRestart,
                               kToolPackageInstall, kToolFilesystemWrite, kToolFilesystemEdit,
                               kToolFilesystemBackup, kToolFilesystemBackupCleanup,
                               kToolFilesystemRestore});
}

bool host_tool_is_plan_bound_observation(const std::string& tool_name) {
  return is_one_of(tool_name, {kToolServerInfo, kToolServerLogs, kToolServerServices,
                               kToolDockerPs, kToolDockerLogs, kToolDockerInspect,
                               kToolSystemdInspect, kToolPackageInspect, kToolFilesystemRead,
                               kToolFilesystemBackupList, kToolFilesystemBackupRetention});
}

bool plan_allows_repeated_observation(const nlohmann::json& plan,
                                      const std::string& plan_step_id) {
  if (json_text_field(plan, "currentStepId") != plan_step_id) return false;
  bool verification_step = false;
  for (const auto& step : plan.value("steps", nlohmann::json::array())) {
    if (json_text_field(step, "id") == plan_step_id &&
        json_text_field(step, "kind") == "verification") {
      verification_step = true;
    }
  }
  if (!verification_step) return false;
  return plan.contains("observationWindow") && plan.at("observationWindow").is_object() &&
         json_text_field(plan.at("observationWindow"), "status") == "running";
}

/** Whether this host tool needs the durable idempotency ledger. */
bool host_tool_requires_idempotency(DesktopDatabase& database,
                                    const nlohmann::json& request) {
  const auto tool_name = json_text_field(request, "toolName");
  if (host_tool_is_effectful(tool_name)) return true;
  const bool planned = present_json_text(request, "taskId") &&
                       present_json_text(request, "planId") &&
                       present_json_text(request, "planStepId") &&
                       host_tool_is_plan_bound_observation(tool_name);
  if (!planned) return false;
  // Observation windows intentionally reuse one verification step while the host
  // advances its sample clock, so the ordinary one-call-per-step fence must not
  // reject a legitimate later sample.
  auto plan = database.latest_investigation_plan(request.at("taskId").get<std::string>());
  if (plan.has_value() &&
      plan_allows_repeated_observation(*plan, request.at("planStepId").get<std::string>())) {
    return false;
  }
  return true;
}

struct HostToolCallDecision {
  bool respond = false;
  nlohmann::json response;
  bool execute = false;
  std::optional<std::pair<std::string, std::string>> token;
};

std::string host_tool_response_status(const nlohmann::json& response) {
  const auto status = json_text_field(response, "status");
  if (status == "success") return "success";
  if (status == "cancelled") return "uncertain";
  if (status == "failed") {
    const auto code =
        json_text_field(response.value("error", nlohmann::json::object()), "code");
    if (is_one_of(code, {"transport", "timeout", "cancelled", "execution_failed"})) {
      return "uncertain";
    }
    return "failed";
  }
  return "uncertain";
}

HostToolCallDecision prepare_host_tool_call(
    DesktopDatabase& database, const nlohmann::json& request, std::mutex& replay_mutex,
    std::map<std::pair<std::string, std::string>, nlohmann::json>& replays) {
  HostToolCallDecision decision;
  if (!host_tool_requires_idempotency(database, request)) {
    decision.execute = true;
    return decision;
  }
  const auto trace_id = json_text_field(request, "traceId");
  const auto call_id = json_text_field(request, "callId");
  if (trim_ascii(trace_id).empty() || trim_ascii(call_id).empty()) {
    decision.respond = true;
    decision.response = host_tool_failed(
        "invalid_input",
        "guarded host tool requests require non-empty traceId and callId", false, std::nullopt);
    return decision;
  }
  const auto fingerprint = host_tool_request_fingerprint(request);
  std::optional<std::string> action_fingerprint;
  if (present_json_text(request, "taskId") && present_json_text(request, "planId") &&
      present_json_text(request, "planStepId")) {
    action_fingerprint = host_tool_action_fingerprint(request);
  }
  nlohmann::json claim_input = nlohmann::json::object();
  claim_input["traceId"] = trace_id;
  claim_input["callId"] = call_id;
  for (const char* key : {"taskId", "planId", "planStepId"}) {
    if (present_json_text(request, key)) claim_input[key] = request.at(key);
  }
  claim_input["toolName"] = json_text_field(request, "toolName");
  claim_input["requestFingerprint"] = fingerprint;
  if (action_fingerprint.has_value()) claim_input["actionFingerprint"] = *action_fingerprint;
  claim_input["startedAt"] = iso8601_now();

  const auto claim = database.claim_host_tool_call(claim_input);
  if (claim.value("claimed", false)) {
    decision.execute = true;
    decision.token = std::make_pair(trace_id, call_id);
    return decision;
  }
  const auto existing_fingerprint = claim.value("requestFingerprint", std::string());
  const auto status = claim.value("status", std::string());
  decision.respond = true;
  if (existing_fingerprint != fingerprint) {
    nlohmann::json detail = nlohmann::json::object();
    detail["code"] = "duplicate_call";
    detail["action"] = "deny";
    detail["status"] = status;
    decision.response = host_tool_failed(
        "plan_deviation",
        "callId was already used for a different host action; refusing to guess which request "
        "is authoritative",
        false, detail);
    return decision;
  }
  {
    std::lock_guard lock(replay_mutex);
    const auto found = replays.find({trace_id, call_id});
    if (found != replays.end()) {
      decision.response = found->second;
      return decision;
    }
  }
  nlohmann::json detail = nlohmann::json::object();
  detail["code"] = "duplicate_call";
  detail["action"] = "wait_user";
  detail["status"] = status;
  detail["traceId"] = trace_id;
  detail["callId"] = call_id;
  decision.response = host_tool_failed(
      "plan_deviation",
      "this host action was already started, but its response is no longer available; reconcile "
      "the target before issuing a new call",
      false, detail);
  return decision;
}

void finish_host_tool_call(
    DesktopDatabase& database, const nlohmann::json& /*request*/,
    const std::optional<std::pair<std::string, std::string>>& token,
    const nlohmann::json& response, std::mutex& replay_mutex,
    std::map<std::pair<std::string, std::string>, nlohmann::json>& replays) {
  if (!token.has_value()) return;
  const auto status = host_tool_response_status(response);
  database.finish_host_tool_call(token->first, token->second, status, iso8601_now());
  // Never persist raw remote output: a successful or deterministic validation
  // failure may be replayed only while this process still holds the response.
  if (status == "uncertain") return;
  std::lock_guard lock(replay_mutex);
  if (replays.size() >= kMaxHostToolReplayCache && replays.find(*token) == replays.end()) {
    replays.erase(replays.begin());
  }
  replays[*token] = response;
}

nlohmann::json handle_plan_record(DesktopDatabase& database, const nlohmann::json& params) {
  if (!params.is_object() || !params.contains("plan") || !params.at("plan").is_object()) {
    return record_failure_json("invalid_input", "invalid investigation plan", false);
  }
  nlohmann::json plan = params.at("plan");
  const auto task_id = json_text_field(plan, "taskId");
  const auto plan_id = json_text_field(plan, "id");
  if (task_id.empty() || plan_id.empty()) {
    return record_failure_json("invalid_input", "invalid investigation plan", false);
  }
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return record_failure_json("not_found", "investigation task was not found", false);
  }
  if (!plan.contains("steps") || !plan.at("steps").is_array() || plan.at("steps").empty()) {
    return record_failure_json("invalid_input", "investigation plan needs at least one step",
                               false);
  }
  if (plan.at("steps").size() > 64) {
    return record_failure_json("invalid_input", "investigation plan has too many steps", false);
  }
  auto previous = database.latest_investigation_plan(task_id);
  auto existing_by_id = database.get_investigation_plan(plan_id);
  const bool same_plan = previous.has_value() && json_text_field(*previous, "id") == plan_id;
  if (existing_by_id.has_value() && !same_plan) {
    return record_failure_json(
        "plan_deviation",
        "plan id already belongs to an older revision; create a new plan id", false);
  }
  if (previous.has_value() && existing_by_id.has_value()) {
    if (json_text_field(*previous, "id") != json_text_field(*existing_by_id, "id") ||
        plan_definition(*previous) != plan_definition(plan)) {
      return record_failure_json(
          "plan_deviation",
          "an existing plan id cannot be reused with a different definition; create a new plan id",
          false);
    }
    merge_plan_runtime(plan, *previous);
  } else {
    plan["revision"] = previous.has_value() ? previous->value("revision", std::int64_t{0}) + 1
                                            : std::int64_t{1};
    plan["status"] = "active";
    plan["approval"] = {{"status", "pending"}};
  }
  if (plan.contains("observationWindow") && plan.at("observationWindow").is_object()) {
    if (auto error = validate_observation_window(plan.at("observationWindow"), plan.at("steps"));
        !error.empty()) {
      return record_failure_json("invalid_input", error, false);
    }
    bool preserved = false;
    if (previous.has_value() && previous->contains("observationWindow") &&
        previous->at("observationWindow").is_object()) {
      const auto& prior = previous->at("observationWindow");
      const auto& current = plan.at("observationWindow");
      const bool matches = prior.value("durationSeconds", 0) ==
                               current.value("durationSeconds", -1) &&
                           prior.value("intervalSeconds", 0) ==
                               current.value("intervalSeconds", -1) &&
                           prior.value("allowedTools", nlohmann::json::array()) ==
                               current.value("allowedTools", nlohmann::json::array()) &&
                           prior.value("successCriteria", nlohmann::json::array()) ==
                               current.value("successCriteria", nlohmann::json::array());
      if (matches) {
        plan["observationWindow"] = prior;
        preserved = true;
      }
    }
    if (!preserved) {
      plan["observationWindow"] =
          plan.value("observationWindow", nlohmann::json::object());
      auto& window = plan["observationWindow"];
      window["status"] = "pending";
      window["sampleCount"] = 0;
      for (const char* key : {"startedAt", "deadlineAt", "deadlineEpochSeconds", "lastSampleAt",
                              "lastSampleEpochSeconds", "lastFailure"}) {
        window.erase(key);
      }
    }
  }
  if (!plan.contains("currentStepId") && !same_plan) {
    plan["currentStepId"] = plan.at("steps")[0].value("id", std::string());
  }
  std::set<std::string> ids;
  int index = 0;
  for (const auto& step : plan.at("steps")) {
    const auto step_id = json_text_field(step, "id");
    if (!ids.insert(step_id).second) {
      return record_failure_json("invalid_input",
                                 "investigation plan step ids must be unique", false);
    }
    if (step.value("ordinal", -1) != index) {
      return record_failure_json("invalid_input",
                                 "investigation plan step ordinals must be consecutive", false);
    }
    const auto allowed = step.value("allowedTools", nlohmann::json::array());
    if (!allowed.is_array() || allowed.empty()) {
      return record_failure_json("invalid_input",
                                 "plan step `" + step_id + "` must allow at least one tool",
                                 false);
    }
    if (auto error = validate_playbook_step(step); !error.empty()) {
      return record_failure_json("invalid_input", error, false);
    }
    if (json_text_field(step, "kind") == "action" &&
        !json_bool_field(step, "requiresApproval") &&
        !task_allows_auto_medium_action(*task, step)) {
      return record_failure_json(
          "denied_by_policy",
          "action step `" + step_id +
              "` may omit approval only for an auto executable goal on a remote development or "
              "staging target, and only for a medium-risk action using one of {filesystem.backup, "
              "filesystem.edit, filesystem.write} (or the backup+write pair)",
          false);
    }
    if (auto error = validate_input_bindings(step); !error.empty()) {
      return record_failure_json("invalid_input", error, false);
    }
    if (auto error = validate_task_guardrails_for_plan_step(*task, step); !error.empty()) {
      return record_failure_json("denied_by_policy", error, false);
    }
    if (step.contains("target") && step.at("target").is_object() &&
        !same_investigation_target(task->value("scope", nlohmann::json::object()),
                                   step.at("target"))) {
      return record_failure_json(
          "denied_by_policy", "plan step `" + step_id + "` expands the task target scope", false);
    }
    const auto evidence_ids = step.value("evidenceIds", nlohmann::json::array());
    if (evidence_ids.is_array()) {
      for (const auto& evidence_id : evidence_ids) {
        if (!evidence_id.is_string()) continue;
        auto evidence = database.get_investigation_evidence(evidence_id.get<std::string>());
        if (!evidence.has_value()) {
          return record_failure_json(
              "evidence_missing", "evidence `" + evidence_id.get<std::string>() + "` was not found",
              true);
        }
        if (json_text_field(*evidence, "taskId") != task_id) {
          return record_failure_json(
              "denied_by_policy",
              "plan step `" + step_id + "` references evidence from another task", false);
        }
      }
    }
    ++index;
  }
  if (plan.contains("currentStepId") && plan.at("currentStepId").is_string() &&
      !ids.count(plan.at("currentStepId").get<std::string>())) {
    return record_failure_json("invalid_input", "currentStepId is not a plan step", false);
  }
  if (!same_plan) normalize_new_plan_runtime(plan, iso8601_now());
  try {
    database.save_investigation_plan(plan);
  } catch (const std::invalid_argument& error) {
    return record_failure_json("invalid_input", error.what(), false);
  }
  nlohmann::json evidence_ids = nlohmann::json::array();
  for (const auto& step : plan.at("steps")) {
    const auto ids_field = step.value("evidenceIds", nlohmann::json::array());
    if (ids_field.is_array()) {
      for (const auto& evidence_id : ids_field) evidence_ids.push_back(evidence_id);
    }
  }
  nlohmann::json artifact = {{"id", "artifact_change_plan_" + plan_id},
                             {"taskId", task_id},
                             {"phase", "decision"},
                             {"kind", "investigation_plan"},
                             {"status", "ready"},
                             {"title", "Pending change plan"},
                             {"summary", "Plan revision saved; action steps run only after scope, "
                                         "baseline and approval conditions are met."},
                             {"content", plan},
                             {"evidenceIds", evidence_ids},
                             {"createdAt", plan.value("createdAt", std::string())},
                             {"updatedAt", plan.value("updatedAt", std::string())}};
  const auto kind = "change_plan";
  artifact["kind"] = kind;
  if (plan.contains("currentStepId")) artifact["planStepId"] = plan.at("currentStepId");
  if (task->contains("activeRunId")) artifact["runId"] = task->at("activeRunId");
  artifact["planId"] = plan_id;
  try {
    database.upsert_investigation_artifact(artifact);
  } catch (const std::exception& error) {
    return record_failure_json("internal", error.what(), false);
  }
  return {{"recorded", true}, {"plan", plan}};
}

nlohmann::json handle_plan_check(DesktopDatabase& database, const nlohmann::json& params) {
  if (!params.is_object()) {
    return {{"status", "failed"},
            {"error", investigation_error("invalid_input", "invalid plan check request", false)}};
  }
  const auto task_id = json_text_field(params, "taskId");
  const auto tool_name = json_text_field(params, "toolName");
  if (task_id.empty() || tool_name.empty() || !params.contains("input") ||
      !params.contains("target") || !params.at("target").is_object()) {
    return {{"status", "failed"},
            {"error", investigation_error("invalid_input", "invalid plan check request", false)}};
  }
  const auto host = json_text_field(params.at("target"), "host");
  if (host != "local" && host != "remote") {
    return {{"status", "failed"},
            {"error", investigation_error("invalid_input", "unknown tool target host", false)}};
  }
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return {{"status", "failed"},
            {"error", investigation_error("not_found", "investigation task was not found", false)}};
  }
  if (auto deviation = check_plan_for_tool(database, *task, tool_name, params.at("target"),
                                           params.at("input"), std::nullopt, std::nullopt,
                                           std::nullopt);
      deviation.has_value()) {
    return {{"status", "deviation"}, {"deviation", *deviation}};
  }
  auto plan = database.latest_investigation_plan(task_id);
  if (!plan.has_value()) {
    return {{"status", "failed"},
            {"error", investigation_error("internal", "active plan disappeared during check", false)}};
  }
  const auto current_step_id = json_text_field(*plan, "currentStepId");
  const auto steps = plan->value("steps", nlohmann::json::array());
  for (const auto& step : steps) {
    if (json_text_field(step, "id") != current_step_id) continue;
    return {{"status", "allowed"},
            {"planId", json_text_field(*plan, "id")},
            {"stepId", current_step_id},
            {"stepKind", json_text_field(step, "kind")},
            {"evidenceIds", step.value("evidenceIds", nlohmann::json::array())},
            {"requiresApproval", json_bool_field(step, "requiresApproval")}};
  }
  return {{"status", "failed"},
          {"error", investigation_error("internal", "active plan step disappeared during check",
                                        false)}};
}

bool task_can_transition(const std::string& from, const std::string& to) {
  if (from == to) return true;
  if (from == "pending") {
    return to == "investigating" || to == "stopped" || to == "expired";
  }
  if (from == "investigating") {
    return to == "waiting_user" || to == "executing" || to == "verifying" || to == "completed" ||
           to == "failed" || to == "stopped" || to == "expired";
  }
  if (from == "waiting_user") {
    return to == "investigating" || to == "executing" || to == "stopped" || to == "expired";
  }
  if (from == "executing") {
    return to == "verifying" || to == "failed" || to == "stopped" || to == "expired";
  }
  if (from == "verifying") {
    return to == "waiting_user" || to == "completed" || to == "failed" || to == "stopped" ||
           to == "expired";
  }
  return false;
}

nlohmann::json handle_plan_step_result(DesktopDatabase& database,
                                       const nlohmann::json& params) {
  if (!params.is_object()) {
    return record_failure_json("invalid_input", "invalid plan step result request", false);
  }
  const auto task_id = json_text_field(params, "taskId");
  const auto plan_id = json_text_field(params, "planId");
  const auto step_id = json_text_field(params, "stepId");
  const auto status = json_text_field(params, "status");
  if (task_id.empty() || plan_id.empty() || step_id.empty() || status.empty()) {
    return record_failure_json("invalid_input", "invalid plan step result request", false);
  }
  if (status != "success" && status != "failed" && status != "cancelled") {
    return record_failure_json("invalid_input", "unknown plan step result status", false);
  }
  auto plan = database.get_investigation_plan(plan_id);
  if (!plan.has_value()) {
    return record_failure_json("not_found", "investigation plan was not found", false);
  }
  if (json_text_field(*plan, "taskId") != task_id) {
    return record_failure_json("denied_by_policy",
                               "plan does not belong to the current investigation task", false);
  }
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return record_failure_json("not_found", "investigation task was not found", false);
  }
  if (json_text_field(*plan, "currentStepId") != step_id) {
    return record_failure_json("plan_deviation",
                               "plan step result does not match the current active step", false);
  }
  const auto now = iso8601_now();
  auto& steps = (*plan)["steps"];
  int step_index = -1;
  for (size_t i = 0; i < steps.size(); ++i) {
    if (json_text_field(steps[i], "id") == step_id) {
      step_index = static_cast<int>(i);
      break;
    }
  }
  if (step_index < 0) {
    return record_failure_json("not_found", "plan step was not found", false);
  }
  const auto step_kind = json_text_field(steps[step_index], "kind");
  const auto retryable = json_bool_field(params, "retryable");
  const auto output_summary = json_text_field(params, "outputSummary");
  const bool requires_baseline = json_bool_field(steps[step_index], "requiresBaseline");
  if (status == "success" && requires_baseline) {
    auto artifacts = database.list_investigation_artifacts(task_id, 128);
    if (!baseline_artifact_matches_plan(artifacts.value("artifacts", nlohmann::json::array()),
                                        *plan)) {
      return record_failure_json(
          "evidence_missing",
          "action step completed without a ready baseline bound to this plan revision", true);
    }
  }
  if (status == "success" && step_kind == "verification") {
    auto artifacts = database.list_investigation_artifacts(task_id, 128);
    bool has_verification = false;
    for (const auto& artifact : artifacts.value("artifacts", nlohmann::json::array())) {
      if (json_text_field(artifact, "kind") == "verification" &&
          json_text_field(artifact, "status") == "succeeded" &&
          json_text_field(artifact, "planId") == plan_id &&
          json_text_field(artifact, "planStepId") == step_id) {
        has_verification = true;
      }
    }
    if (!has_verification) {
      return record_failure_json(
          "evidence_missing",
          "verification step must record a succeeded verification artifact before it can advance",
          true);
    }
  }
  if (step_kind == "verification" && plan->contains("observationWindow") &&
      plan->at("observationWindow").is_object()) {
    auto& window = (*plan)["observationWindow"];
    const auto window_status = json_text_field(window, "status");
    if (status != "success" && window_status == "running") {
      const auto failure_message = output_summary.empty() ? std::string("observation sample failed")
                                                          : output_summary;
      window["status"] = "failed";
      window["lastFailure"] = failure_message;
      (*plan)["updatedAt"] = now;
      database.save_investigation_plan(*plan);
      nlohmann::json failure = {{"code", "command_failed"},
                                {"message", failure_message},
                                {"retryable", false},
                                {"attempt", steps[step_index].value("attempts", 0) + 1},
                                {"at", now},
                                {"detail", {{"planId", plan_id},
                                            {"stepId", step_id},
                                            {"observation", "failed"},
                                            {"status", status}}}};
      nlohmann::json failure_artifact = {
          {"id", "artifact_observation_failure_" + plan_id + "_" + step_id},
          {"taskId", task_id},
          {"planId", plan_id},
          {"planStepId", step_id},
          {"phase", "recovery"},
          {"kind", "failure"},
          {"status", "failed"},
          {"title", "Observation window failure"},
          {"summary", failure_message},
          {"content", failure},
          {"evidenceIds", steps[step_index].value("evidenceIds", nlohmann::json::array())},
          {"createdAt", now},
          {"updatedAt", now}};
      if (task->contains("activeRunId")) failure_artifact["runId"] = task->at("activeRunId");
      database.upsert_investigation_artifact(failure_artifact);
      const auto current_status = json_text_field(*task, "status");
      if (task_can_transition(current_status, "waiting_user")) {
        if (!database
                 .update_investigation_task_progress(
                     task_id, "waiting_user", "recovery",
                     task->contains("activeRunId")
                         ? std::optional<std::string>(task->at("activeRunId").get<std::string>())
                         : std::nullopt,
                     failure, now, std::nullopt)
                 .has_value()) {
          return record_failure_json("not_found", "investigation task was not found", false);
        }
      }
      return {{"recorded", true}, {"observation", "failed"}, {"plan", *plan}};
    }
    if (status == "success") {
      const auto epoch = now_epoch_seconds();
      bool sampled = false;
      std::optional<std::string> next_sample_at;
      if (window_status == "pending") {
        const auto deadline = epoch + window.value("durationSeconds", std::int64_t{0});
        window["status"] = "running";
        window["sampleCount"] = 1;
        window["startedAt"] = now;
        window["deadlineEpochSeconds"] = deadline;
        window["deadlineAt"] = iso8601_from_epoch(deadline);
        window["lastSampleEpochSeconds"] = epoch;
        window["lastSampleAt"] = now;
        sampled = true;
        next_sample_at = iso8601_from_epoch(epoch + window.value("intervalSeconds", std::int64_t{0}));
      } else if (window_status == "running") {
        const auto deadline = window.value("deadlineEpochSeconds", epoch);
        if (epoch >= deadline) {
          window["status"] = "succeeded";
          window["sampleCount"] = window.value("sampleCount", 0) + 1;
          window["lastSampleEpochSeconds"] = epoch;
          window["lastSampleAt"] = now;
        } else {
          const auto last = window.value("lastSampleEpochSeconds", std::int64_t{0});
          const auto interval = window.value("intervalSeconds", std::int64_t{0});
          if (epoch - last < interval) {
            next_sample_at = iso8601_from_epoch(last + interval);
          } else {
            window["sampleCount"] = window.value("sampleCount", 0) + 1;
            window["lastSampleEpochSeconds"] = epoch;
            window["lastSampleAt"] = now;
            sampled = true;
            next_sample_at = iso8601_from_epoch(epoch + interval);
          }
        }
      } else if (window_status == "failed" || window_status == "cancelled") {
        return record_failure_json(
            "invalid_input",
            "observation window is no longer runnable; re-plan before sampling again", false);
      }
      if (json_text_field(window, "status") == "running" &&
          (sampled || next_sample_at.has_value())) {
        (*plan)["updatedAt"] = now;
        database.save_investigation_plan(*plan);
        nlohmann::json response = {{"recorded", true},
                                   {"observation", "running"},
                                   {"sampleAccepted", sampled},
                                   {"plan", *plan}};
        if (next_sample_at.has_value()) response["nextSampleAt"] = *next_sample_at;
        return response;
      }
    }
  }
  auto& step = steps[step_index];
  const auto current_step_status = json_text_field(step, "status");
  if (current_step_status != "pending" && current_step_status != "running") {
    return record_failure_json("invalid_input", "plan step is no longer active", false);
  }
  step["attempts"] = step.value("attempts", 0) + 1;
  if (!step.contains("startedAt")) step["startedAt"] = now;
  step["endedAt"] = now;
  bool blocked = false;
  if (status == "success") {
    step["status"] = "succeeded";
    step.erase("lastDeviation");
    const size_t next_index = static_cast<size_t>(step_index) + 1;
    if (next_index < steps.size()) {
      steps[next_index]["status"] = "running";
      if (!steps[next_index].contains("startedAt")) steps[next_index]["startedAt"] = now;
      (*plan)["currentStepId"] = steps[next_index].value("id", std::string());
    } else {
      (*plan)["currentStepId"] = nullptr;
      (*plan)["status"] = "completed";
    }
  } else {
    const auto max_attempts = step.value("maxAttempts", 1);
    if (!retryable || step.value("attempts", 0) >= max_attempts) {
      step["status"] = "blocked";
      blocked = true;
    } else {
      step["status"] = "pending";
    }
  }
  (*plan)["updatedAt"] = now;
  try {
    database.save_investigation_plan(*plan);
  } catch (const std::invalid_argument& error) {
    return record_failure_json("invalid_input", error.what(), false);
  }
  if (blocked) {
    const auto failure_code = status == "cancelled" ? "cancelled" : "command_failed";
    const auto failure_message =
        output_summary.empty() ? std::string("Plan step did not complete; waiting for a decision")
                               : output_summary;
    nlohmann::json failure = {{"code", failure_code},
                              {"message", failure_message},
                              {"retryable", false},
                              {"attempt", steps[step_index].value("attempts", 0)},
                              {"at", now},
                              {"detail", {{"planId", plan_id},
                                          {"stepId", step_id},
                                          {"status", status},
                                          {"retryable", retryable}}}};
    nlohmann::json failure_artifact = {
        {"id", "artifact_failure_" + plan_id + "_" + step_id},
        {"taskId", task_id},
        {"planId", plan_id},
        {"planStepId", step_id},
        {"phase", "recovery"},
        {"kind", "failure"},
        {"status", "failed"},
        {"title", "Plan step failed"},
        {"summary", failure_message},
        {"content", failure},
        {"evidenceIds", steps[step_index].value("evidenceIds", nlohmann::json::array())},
        {"createdAt", now},
        {"updatedAt", now}};
    if (task->contains("activeRunId")) failure_artifact["runId"] = task->at("activeRunId");
    database.upsert_investigation_artifact(failure_artifact);
    const auto current_status = json_text_field(*task, "status");
    if (task_can_transition(current_status, "failed")) {
      if (!database
               .update_investigation_task_progress(
                   task_id, "failed", "recovery",
                   task->contains("activeRunId")
                       ? std::optional<std::string>(task->at("activeRunId").get<std::string>())
                       : std::nullopt,
                   failure, now, std::nullopt)
               .has_value()) {
        return record_failure_json("not_found", "investigation task was not found", false);
      }
    }
    return {{"recorded", true}, {"plan", *plan}};
  }
  std::string desired_status = "waiting_user";
  if (json_text_field(*plan, "status") == "completed" && step_kind == "verification") {
    desired_status = "completed";
  } else if ((*plan).contains("currentStepId") && (*plan).at("currentStepId").is_string()) {
    const auto next_id = (*plan).at("currentStepId").get<std::string>();
    for (const auto& candidate : steps) {
      if (json_text_field(candidate, "id") != next_id) continue;
      const auto kind = json_text_field(candidate, "kind");
      desired_status = kind == "action"        ? "executing"
                       : kind == "verification" ? "verifying"
                       : kind == "decision"     ? "waiting_user"
                                                : "investigating";
      break;
    }
  }
  const auto current_status = json_text_field(*task, "status");
  if (current_status != desired_status && task_can_transition(current_status, desired_status)) {
    const auto completed_at = desired_status == "completed"
                                  ? std::optional<std::string>(now)
                                  : std::nullopt;
    if (!database.set_investigation_task_status(task_id, desired_status, completed_at, now)
             .has_value()) {
      return record_failure_json("not_found", "investigation task was not found", false);
    }
  }
  return {{"recorded", true}, {"plan", *plan}};
}

constexpr int kMaxEvidenceSearchLimit = 64;
constexpr std::int64_t kDefaultEvidenceCorrelationWindowSeconds = 300;
constexpr std::int64_t kMaxEvidenceCorrelationWindowSeconds = 3600;
constexpr std::int64_t kDefaultRetentionDays = 30;
constexpr int kDefaultRetentionLimit = 64;
constexpr int kMaxRetentionLimit = 128;
// Evidence is a point-in-time observation. The host exposes this conservative,
// versioned policy as metadata; it never mutates or deletes old rows here.
constexpr std::int64_t kEvidenceFreshnessStaleAfterSeconds = 15 * 60;
constexpr std::int64_t kEvidenceFreshnessExpiresAfterSeconds = 24 * 60 * 60;
constexpr std::size_t kMaxComparePaths = 64;
constexpr std::int64_t kMaxComparePathCount = 10000;
constexpr std::size_t kMaxCompareNodes = 4096;
constexpr std::size_t kMaxCompareDepth = 8;
constexpr std::int64_t kMaxCompareLines = 100000;

/** Host-derived, versioned freshness for one observation timestamp. */
nlohmann::json evidence_freshness_json(const std::string& collected_at,
                                       std::int64_t evaluated_at_epoch) {
  nlohmann::json freshness = {{"policy", "default-v1"},
                              {"evaluatedAt", iso8601_from_epoch(evaluated_at_epoch)},
                              {"staleAfterSeconds", kEvidenceFreshnessStaleAfterSeconds},
                              {"expiresAfterSeconds", kEvidenceFreshnessExpiresAfterSeconds}};
  const auto collected = parse_iso8601_utc_seconds(collected_at);
  if (!collected.has_value()) {
    freshness["status"] = "unknown";
    freshness["reason"] = "collection timestamp is not a valid UTC ISO-8601 value";
    return freshness;
  }
  if (*collected > evaluated_at_epoch) {
    freshness["status"] = "unknown";
    freshness["reason"] = "collection timestamp is in the future relative to the host clock";
    return freshness;
  }
  const auto age_seconds = evaluated_at_epoch - *collected;
  freshness["ageSeconds"] = age_seconds;
  freshness["status"] = age_seconds <= kEvidenceFreshnessStaleAfterSeconds
                            ? "fresh"
                            : age_seconds <= kEvidenceFreshnessExpiresAfterSeconds ? "stale"
                                                                                  : "expired";
  return freshness;
}

nlohmann::json handle_evidence_record(DesktopDatabase& database, const nlohmann::json& params) {
  if (!params.is_object() || !params.contains("evidence") ||
      !params.at("evidence").is_object()) {
    return record_failure_json("invalid_input", "invalid evidence", false);
  }
  nlohmann::json evidence = params.at("evidence");
  const auto task_id = json_text_field(evidence, "taskId");
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return record_failure_json("not_found",
                               "investigation task `" + task_id + "` was not found", false);
  }
  if (!same_investigation_target(task->value("scope", nlohmann::json::object()),
                                 evidence.value("scope", nlohmann::json::object()))) {
    return record_failure_json(
        "denied_by_policy", "evidence target does not match the investigation task scope", false);
  }
  // The host owns the run association; an untrusted run id is overwritten.
  if (task->contains("activeRunId") && task->at("activeRunId").is_string()) {
    evidence["runId"] = task->at("activeRunId");
  } else {
    evidence.erase("runId");
  }
  if (!evidence.contains("content")) {
    return record_failure_json("invalid_input", "evidence content is required", false);
  }
  if (evidence.at("content").dump().size() > 1048576) {
    return record_failure_json("invalid_input", "evidence content exceeds 1048576 bytes", false);
  }
  if (json_text_field(evidence, "redactionStatus", "unknown") == "unknown") {
    return record_failure_json("invalid_input", "evidence must be redacted before persistence",
                               false);
  }
  if (DesktopDatabase::content_hash(evidence.at("content")) !=
      json_text_field(evidence, "contentHash")) {
    return record_failure_json("invalid_input", "evidence contentHash does not match content",
                               false);
  }
  auto existing = database.find_investigation_evidence_in_run(evidence);
  if (existing.has_value()) {
    return {{"recorded", true},
            {"evidenceId", json_text_field(*existing, "id")},
            {"reused", true}};
  }
  try {
    database.record_investigation_evidence(evidence);
  } catch (const std::invalid_argument& error) {
    return record_failure_json("invalid_input", error.what(), false);
  }
  return {{"recorded", true}, {"evidenceId", json_text_field(evidence, "id")}, {"reused", false}};
}

nlohmann::json evidence_failed(std::string code, std::string message) {
  return {{"status", "failed"},
          {"error", investigation_error(std::move(code), std::move(message), false)}};
}

nlohmann::json handle_evidence_fetch(DesktopDatabase& database, const nlohmann::json& params) {
  const auto task_id = params.is_object() ? json_text_field(params, "taskId") : std::string();
  const auto evidence_id =
      params.is_object() ? json_text_field(params, "evidenceId") : std::string();
  if (task_id.empty() || evidence_id.empty()) {
    return evidence_failed("invalid_input", "taskId and evidenceId are required");
  }
  auto evidence = database.get_investigation_evidence(evidence_id);
  if (!evidence.has_value()) return {{"status", "not_found"}};
  if (json_text_field(*evidence, "taskId") != task_id) {
    return evidence_failed("denied_by_policy",
                           "evidence does not belong to the current investigation task");
  }
  // Freshness is host-derived and recalculated on presentation; ignore any
  // value a caller may have supplied with the stored row.
  nlohmann::json enriched = *evidence;
  enriched["freshness"] = evidence_freshness_json(json_text_field(*evidence, "collectedAt"),
                                                 now_epoch_seconds());
  return {{"status", "success"}, {"evidence", enriched}};
}

/** Metadata-only projection: content is dropped, host freshness is added. */
nlohmann::json evidence_summary(const nlohmann::json& evidence) {
  nlohmann::json summary = evidence;
  summary.erase("content");
  summary["freshness"] =
      evidence_freshness_json(json_text_field(evidence, "collectedAt"), now_epoch_seconds());
  return summary;
}

nlohmann::json handle_evidence_search(DesktopDatabase& database, const nlohmann::json& params) {
  const auto task_id = params.is_object() ? json_text_field(params, "taskId") : std::string();
  if (task_id.empty() || task_id.size() > 256) {
    return evidence_failed("invalid_input", "taskId must be between 1 and 256 characters");
  }
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return evidence_failed("not_found", "investigation task was not found");
  }
  const auto limit = params.value("limit", 32);
  if (limit < 1 || limit > 64) {
    return evidence_failed("invalid_input", "limit must be between 1 and 64");
  }
  auto query = nlohmann::json::object();
  query["limit"] = limit;
  query["scope"] = task->value("scope", nlohmann::json::object());
  if (params.contains("target") && params.at("target").is_object()) {
    if (!same_investigation_target(task->value("scope", nlohmann::json::object()),
                                   params.at("target"))) {
      return evidence_failed("denied_by_policy",
                             "evidence target does not match the investigation task scope");
    }
    query["scope"] = params.at("target");
  }
  for (const char* key : {"sourceTool", "kind", "from", "to"}) {
    if (params.contains(key) && params.at(key).is_string()) query[key] = params.at(key);
  }
  nlohmann::json results;
  try {
    results = database.search_investigation_evidence(task_id, query);
  } catch (const std::invalid_argument& error) {
    return evidence_failed("invalid_input", error.what());
  }
  nlohmann::json summaries = nlohmann::json::array();
  for (const auto& item : results) summaries.push_back(evidence_summary(item));
  return {{"status", "success"}, {"evidence", summaries}};
}

struct JsonDiffAccumulator {
  nlohmann::json paths = nlohmann::json::array();
  std::int64_t count = 0;
  std::size_t nodes = 0;
  bool truncated = false;
};

void record_json_diff_path(const std::string& path, JsonDiffAccumulator& diff) {
  diff.count = std::min(diff.count + 1, kMaxComparePathCount);
  if (diff.paths.size() < kMaxComparePaths) {
    diff.paths.push_back(path.size() > 512 ? path.substr(0, 512) : path);
  } else {
    diff.truncated = true;
  }
}

/** Record the difference paths between two JSON values without returning raw values. */
void collect_json_diff(const nlohmann::json& left, const nlohmann::json& right,
                       const std::string& path, std::size_t depth, JsonDiffAccumulator& diff) {
  if (left == right) return;
  if (diff.nodes >= kMaxCompareNodes) {
    diff.truncated = true;
    return;
  }
  ++diff.nodes;
  if (depth >= kMaxCompareDepth || diff.count >= kMaxComparePathCount) {
    record_json_diff_path(path, diff);
    diff.truncated = true;
    return;
  }
  if (left.is_object() && right.is_object()) {
    std::set<std::string> keys;
    for (auto iterator = left.begin(); iterator != left.end(); ++iterator) keys.insert(iterator.key());
    for (auto iterator = right.begin(); iterator != right.end(); ++iterator) keys.insert(iterator.key());
    for (const auto& key : keys) {
      std::string child_path;
      const bool simple_key = !key.empty() &&
                              std::all_of(key.begin(), key.end(), [](unsigned char character) {
                                return std::isalnum(character) != 0 || character == '_' ||
                                       character == '-';
                              });
      if (simple_key) {
        child_path = path + "." + key;
      } else {
        child_path = path + "[\"" + key + "\"]";
      }
      const auto left_child = left.find(key);
      const auto right_child = right.find(key);
      if (left_child != left.end() && right_child != right.end()) {
        collect_json_diff(*left_child, *right_child, child_path, depth + 1, diff);
      } else {
        record_json_diff_path(child_path, diff);
      }
      if (diff.truncated && diff.nodes >= kMaxCompareNodes) break;
    }
    return;
  }
  if (left.is_array() && right.is_array()) {
    const auto length = std::max(left.size(), right.size());
    for (std::size_t index = 0; index < length; ++index) {
      const auto child_path = path + "[" + std::to_string(index) + "]";
      if (index < left.size() && index < right.size()) {
        collect_json_diff(left.at(index), right.at(index), child_path, depth + 1, diff);
      } else {
        record_json_diff_path(child_path, diff);
      }
      if (diff.truncated && diff.nodes >= kMaxCompareNodes) break;
    }
    return;
  }
  record_json_diff_path(path, diff);
}

std::vector<std::string> split_compare_lines(const std::string& text) {
  std::vector<std::string> lines;
  std::size_t start = 0;
  while (lines.size() <= static_cast<std::size_t>(kMaxCompareLines)) {
    const auto newline = text.find('\n', start);
    if (newline == std::string::npos) {
      lines.push_back(text.substr(start));
      break;
    }
    lines.push_back(text.substr(start, newline - start));
    start = newline + 1;
  }
  return lines;
}

/** Bounded line-count comparison for two text bodies. */
std::pair<nlohmann::json, bool> compare_text_content(const nlohmann::json& left,
                                                     const nlohmann::json& right) {
  if (!left.is_string() || !right.is_string()) return {nlohmann::json(), false};
  const auto left_lines = split_compare_lines(left.get<std::string>());
  const auto right_lines = split_compare_lines(right.get<std::string>());
  const bool truncated = left_lines.size() > static_cast<std::size_t>(kMaxCompareLines) ||
                         right_lines.size() > static_cast<std::size_t>(kMaxCompareLines);
  const auto left_count = std::min<std::int64_t>(static_cast<std::int64_t>(left_lines.size()),
                                                 kMaxCompareLines);
  const auto right_count = std::min<std::int64_t>(static_cast<std::int64_t>(right_lines.size()),
                                                  kMaxCompareLines);
  const auto paired_limit = std::min(
      {static_cast<std::int64_t>(left_lines.size()),
       static_cast<std::int64_t>(right_lines.size()), kMaxCompareLines});
  std::int64_t paired_changes = 0;
  for (std::int64_t index = 0; index < paired_limit; ++index) {
    if (left_lines[static_cast<std::size_t>(index)] != right_lines[static_cast<std::size_t>(index)]) {
      ++paired_changes;
    }
  }
  const auto difference = left_count > right_count ? left_count - right_count : right_count - left_count;
  nlohmann::json text = {{"leftLineCount", left_count},
                         {"rightLineCount", right_count},
                         {"changedLineCount", paired_changes + difference},
                         {"addedLineCount", std::max<std::int64_t>(0, right_count - left_count)},
                         {"removedLineCount", std::max<std::int64_t>(0, left_count - right_count)}};
  return {text, truncated};
}

/** Compare two persisted observations without returning either body. */
nlohmann::json handle_evidence_compare(DesktopDatabase& database, const nlohmann::json& params) {
  const auto task_id = params.is_object() ? json_text_field(params, "taskId") : std::string();
  const auto left_id =
      params.is_object() ? json_text_field(params, "leftEvidenceId") : std::string();
  const auto right_id =
      params.is_object() ? json_text_field(params, "rightEvidenceId") : std::string();
  if (task_id.empty() || task_id.size() > 256 || left_id.empty() || left_id.size() > 256 ||
      right_id.empty() || right_id.size() > 256) {
    return evidence_failed(
        "invalid_input", "taskId and both evidence ids must be between 1 and 256 characters");
  }
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) return evidence_failed("not_found", "investigation task was not found");
  const auto scope = task->value("scope", nlohmann::json::object());
  nlohmann::json load_error;
  const auto load = [&](const std::string& id, const char* side) -> std::optional<nlohmann::json> {
    auto evidence = database.get_investigation_evidence(id);
    if (!evidence.has_value()) {
      load_error = evidence_failed("not_found",
                                   std::string(side) + " evidence was not found in the current task");
      return std::nullopt;
    }
    if (json_text_field(*evidence, "taskId") != task_id ||
        !same_investigation_target(scope, evidence->value("scope", nlohmann::json::object()))) {
      load_error = evidence_failed(
          "denied_by_policy",
          std::string(side) + " evidence does not belong to the current investigation scope");
      return std::nullopt;
    }
    return evidence;
  };
  auto left = load(left_id, "left");
  if (!left.has_value()) return load_error;
  auto right = load(right_id, "right");
  if (!right.has_value()) return load_error;

  const auto left_summary = evidence_summary(*left);
  const auto right_summary = evidence_summary(*right);
  const auto status = json_text_field(*left, "contentHash") == json_text_field(*right, "contentHash")
                          ? std::string("identical")
                          : std::string("changed");
  const auto left_type = json_text_field(*left, "contentType");
  const auto right_type = json_text_field(*right, "contentType");
  const std::string shape = left_type == "json" && right_type == "json"   ? "json"
                            : left_type == "text" && right_type == "text" ? "text"
                                                                          : "mixed";

  nlohmann::json warnings = nlohmann::json::array();
  if (json_bool_field(*left, "truncated") || json_bool_field(*right, "truncated")) {
    warnings.push_back("one or both evidence bodies were truncated at collection");
  }
  if (json_text_field(*left, "sourceTool") != json_text_field(*right, "sourceTool") ||
      json_text_field(*left, "kind") != json_text_field(*right, "kind")) {
    warnings.push_back("source or evidence kind differs; interpret correlation cautiously");
  }
  const std::pair<const char*, const nlohmann::json*> sides[] = {{"left", &left_summary},
                                                                 {"right", &right_summary}};
  for (const auto& side : sides) {
    const auto freshness_status =
        side.second->value("freshness", nlohmann::json::object()).value("status", std::string());
    if (freshness_status != "fresh") {
      warnings.push_back(std::string(side.first) + " evidence is " + freshness_status +
                         " under default-v1 freshness");
    }
  }

  JsonDiffAccumulator diff;
  nlohmann::json text_comparison;
  if (status != "identical") {
    if (shape == "json") {
      collect_json_diff(left->at("content"), right->at("content"), "$", 0, diff);
    } else if (shape == "text") {
      auto compared = compare_text_content(left->at("content"), right->at("content"));
      text_comparison = compared.first;
      diff.truncated = compared.second;
    } else {
      record_json_diff_path("$", diff);
    }
  }
  if (diff.truncated) {
    warnings.push_back(
        "comparison details were bounded; inspect individual evidence ids for more context");
  }
  if (warnings.size() > 8) {
    nlohmann::json bounded = nlohmann::json::array();
    for (std::size_t index = 0; index < 8; ++index) bounded.push_back(warnings.at(index));
    warnings = std::move(bounded);
  }
  nlohmann::json comparison = {{"status", status},
                               {"shape", shape},
                               {"left", left_summary},
                               {"right", right_summary},
                               {"changedPaths", diff.paths},
                               {"changedPathCount", diff.count},
                               {"diffTruncated", diff.truncated},
                               {"warnings", warnings}};
  if (!text_comparison.is_null()) comparison["text"] = text_comparison;
  return {{"status", "success"}, {"comparison", comparison}};
}

/** Deterministic host-side grouping used to inspect one observation across sources. */
nlohmann::json handle_evidence_correlation(DesktopDatabase& database, const nlohmann::json& params) {
  const auto task_id = params.is_object() ? json_text_field(params, "taskId") : std::string();
  const auto anchor_id =
      params.is_object() ? json_text_field(params, "anchorEvidenceId") : std::string();
  if (task_id.empty() || task_id.size() > 256 || anchor_id.empty() || anchor_id.size() > 256) {
    return evidence_failed("invalid_input",
                           "taskId and anchorEvidenceId must be between 1 and 256 characters");
  }
  std::int64_t window_seconds = kDefaultEvidenceCorrelationWindowSeconds;
  if (params.contains("windowSeconds")) {
    if (!params.at("windowSeconds").is_number_integer()) {
      return evidence_failed("invalid_input", "windowSeconds must be between 1 and 3600");
    }
    window_seconds = params.at("windowSeconds").get<std::int64_t>();
    if (window_seconds < 1 || window_seconds > kMaxEvidenceCorrelationWindowSeconds) {
      return evidence_failed("invalid_input", "windowSeconds must be between 1 and 3600");
    }
  }
  int limit = 32;
  if (params.contains("limit")) {
    if (!params.at("limit").is_number_integer()) {
      return evidence_failed("invalid_input", "limit must be between 1 and 64");
    }
    const auto requested = params.at("limit").get<std::int64_t>();
    if (requested < 1 || requested > kMaxEvidenceSearchLimit) {
      return evidence_failed("invalid_input", "limit must be between 1 and 64");
    }
    limit = static_cast<int>(requested);
  }
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) return evidence_failed("not_found", "investigation task was not found");
  const auto scope = task->value("scope", nlohmann::json::object());
  auto anchor = database.get_investigation_evidence(anchor_id);
  if (!anchor.has_value()) {
    return evidence_failed("not_found", "anchor evidence was not found in the current task");
  }
  if (json_text_field(*anchor, "taskId") != task_id ||
      !same_investigation_target(scope, anchor->value("scope", nlohmann::json::object()))) {
    return evidence_failed("denied_by_policy",
                           "anchor evidence does not belong to the current investigation scope");
  }

  nlohmann::json related;
  std::string matched_by;
  const auto anchor_run = json_text_field(*anchor, "runId");
  if (!anchor_run.empty()) {
    related = database.list_investigation_evidence_for_run(anchor_run, limit);
    matched_by = "same_run";
  } else {
    const auto anchor_epoch = parse_iso8601_utc_seconds(json_text_field(*anchor, "collectedAt"));
    if (!anchor_epoch.has_value()) {
      return evidence_failed(
          "invalid_input",
          "anchor evidence has no valid UTC collection timestamp for time-window correlation");
    }
    const auto from = iso8601_from_epoch(*anchor_epoch - window_seconds);
    const auto to = iso8601_from_epoch(*anchor_epoch + window_seconds);
    nlohmann::json query = {{"from", from}, {"to", to}, {"scope", scope}, {"limit", limit}};
    related = database.search_investigation_evidence(task_id, query);
    matched_by = "time_window";
  }

  nlohmann::json warnings = nlohmann::json::array();
  const auto anchor_summary = evidence_summary(*anchor);
  const auto anchor_freshness =
      anchor_summary.value("freshness", nlohmann::json::object()).value("status", std::string());
  if (anchor_freshness != "fresh") {
    warnings.push_back("anchor evidence is " + anchor_freshness + " under default-v1 freshness");
  }
  if (json_bool_field(*anchor, "truncated")) {
    warnings.push_back("anchor evidence was truncated at collection");
  }
  std::set<std::string> source_tools;
  nlohmann::json summaries = nlohmann::json::array();
  for (const auto& item : related) {
    if (!item.is_object()) continue;
    if (static_cast<std::int64_t>(summaries.size()) >= limit) break;
    if (json_text_field(item, "taskId") != task_id) continue;
    if (!same_investigation_target(scope, item.value("scope", nlohmann::json::object()))) continue;
    const auto source_tool = json_text_field(item, "sourceTool");
    source_tools.insert(source_tool);
    const auto summary = evidence_summary(item);
    const auto freshness_status =
        summary.value("freshness", nlohmann::json::object()).value("status", std::string());
    if (freshness_status != "fresh" && warnings.size() < 8) {
      warnings.push_back(source_tool + " evidence is " + freshness_status +
                         " under default-v1 freshness");
    }
    if (json_bool_field(item, "truncated") && warnings.size() < 8) {
      warnings.push_back(source_tool + " evidence was truncated at collection");
    }
    summaries.push_back(summary);
  }
  source_tools.insert(json_text_field(*anchor, "sourceTool"));
  if (summaries.empty()) {
    warnings.push_back("no other evidence matched the host-owned run or time window");
  }
  if (warnings.size() > 8) {
    nlohmann::json bounded = nlohmann::json::array();
    for (std::size_t index = 0; index < 8; ++index) bounded.push_back(warnings.at(index));
    warnings = std::move(bounded);
  }
  nlohmann::json source_list = nlohmann::json::array();
  for (const auto& tool : source_tools) {
    if (source_list.size() >= 32) break;
    source_list.push_back(tool);
  }
  nlohmann::json correlation = {{"anchor", anchor_summary},
                                {"evidence", summaries},
                                {"matchedBy", matched_by},
                                {"windowSeconds", window_seconds},
                                {"sourceTools", source_list},
                                {"warnings", warnings}};
  return {{"status", "success"}, {"correlation", correlation}};
}

/** Report local retention candidates to the Agent without a delete capability. */
nlohmann::json handle_retention_preview(DesktopDatabase& database, const nlohmann::json& params) {
  const auto task_id = params.is_object() ? json_text_field(params, "taskId") : std::string();
  if (task_id.empty() || task_id.size() > 256 ||
      task_id.find_first_of("\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f"
                            "\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f") !=
          std::string::npos) {
    return evidence_failed("invalid_input", "taskId must be between 1 and 256 visible characters");
  }
  std::string cutoff_at;
  if (params.contains("cutoffAt")) {
    if (!params.at("cutoffAt").is_string()) {
      return evidence_failed("invalid_input", "cutoffAt must be a bounded UTC ISO-8601 timestamp");
    }
    cutoff_at = params.at("cutoffAt").get<std::string>();
    if (cutoff_at.empty() || cutoff_at.size() > 80 ||
        !parse_iso8601_utc_seconds(cutoff_at).has_value()) {
      return evidence_failed("invalid_input", "cutoffAt must be a UTC ISO-8601 timestamp");
    }
  } else {
    cutoff_at = iso8601_from_epoch(now_epoch_seconds() - kDefaultRetentionDays * 86400);
  }
  int limit = kDefaultRetentionLimit;
  if (params.contains("limit")) {
    if (!params.at("limit").is_number_integer()) {
      return evidence_failed("invalid_input", "limit must be between 1 and 128");
    }
    const auto requested = params.at("limit").get<std::int64_t>();
    if (requested < 1 || requested > kMaxRetentionLimit) {
      return evidence_failed("invalid_input", "limit must be between 1 and 128");
    }
    limit = static_cast<int>(requested);
  }
  std::optional<nlohmann::json> preview;
  try {
    preview = database.investigation_retention_preview(task_id, cutoff_at, limit);
  } catch (const std::invalid_argument& error) {
    return evidence_failed("invalid_input", error.what());
  }
  if (!preview.has_value()) {
    return evidence_failed("not_found", "investigation task was not found");
  }
  return {{"status", "success"}, {"preview", *preview}};
}

/** Read-only context projection for the sidecar: never executes remote tools. */
nlohmann::json handle_context_request(DesktopDatabase& database, const nlohmann::json& params) {
  const auto context_failure = [](const std::string& code, const std::string& message,
                                  bool retryable) {
    return nlohmann::json{
        {"status", "failed"},
        {"error", investigation_error(code, message, retryable)}};
  };
  if (!params.is_object()) {
    return context_failure("invalid_input", "invalid host context request", false);
  }
  const auto kind = json_text_field(params, "kind");
  const auto id = json_text_field(params, "id");
  if (id.empty() || id.size() > 160) {
    return context_failure("invalid_input", "context id must be between 1 and 160 characters",
                           true);
  }
  if (kind != "server" && kind != "snapshot" && kind != "workspace" &&
      kind != "investigation") {
    return context_failure("invalid_input", "invalid host context request", false);
  }
  if ((kind == "server" || kind == "snapshot") && id.rfind("srv_", 0) != 0) {
    return context_failure("invalid_input", "server context requires an opaque srv_ id", false);
  }
  const auto context_success = [](nlohmann::json data) {
    return nlohmann::json{{"status", "success"}, {"data", std::move(data)}};
  };
  try {
    if (kind == "server") {
      const auto servers = database.list_servers().at("servers");
      for (const auto& server : servers) {
        if (json_text_field(server, "id") == id) return context_success(server);
      }
      return nlohmann::json{{"status", "not_found"}};
    }
    if (kind == "workspace") {
      const auto workspaces = database.list_workspaces().at("workspaces");
      for (const auto& workspace : workspaces) {
        if (json_text_field(workspace, "id") == id) return context_success(workspace);
      }
      return nlohmann::json{{"status", "not_found"}};
    }
    if (kind == "snapshot") {
      auto snapshot = database.latest_snapshot(id);
      if (!snapshot.has_value()) return nlohmann::json{{"status", "not_found"}};
      return context_success(std::move(*snapshot));
    }
    auto detail = database.get_investigation_task(id);
    if (!detail.has_value()) return nlohmann::json{{"status", "not_found"}};
    nlohmann::json context = nlohmann::json::object();
    context["task"] = detail->at("task");
    nlohmann::json evidence = nlohmann::json::array();
    for (const auto& item : detail->at("evidence")) {
      evidence.push_back(evidence_summary(item));
    }
    context["evidence"] = std::move(evidence);
    context["findings"] = detail->at("findings");
    context["runs"] = detail->at("runs");
    context["steps"] = detail->at("steps");
    nlohmann::json artifacts = nlohmann::json::array();
    for (const auto& item : detail->at("artifacts")) {
      nlohmann::json summary = item;
      summary.erase("content");
      artifacts.push_back(std::move(summary));
    }
    context["artifacts"] = std::move(artifacts);
    if (detail->contains("plan")) context["plan"] = detail->at("plan");
    if (detail->contains("decisionBrief")) context["decisionBrief"] = detail->at("decisionBrief");
    return context_success(std::move(context));
  } catch (const nlohmann::json::exception& error) {
    return context_failure("internal", error.what(), false);
  } catch (const std::exception& error) {
    return context_failure("internal", error.what(), false);
  }
}

// ── SSH filesystem action binding + host-owned backup ledger ──────────────────
// A durable task, an active action plan step and the exact bound inputs are the
// prerequisites for every effectful filesystem mutation. This mirrors the retired
// Rust `validate_ssh_filesystem_action_binding`.
struct FilesystemActionBinding {
  std::string server_id;
  std::string task_id;
  std::string tool_name;
  nlohmann::json input;
  nlohmann::json target;
  std::string plan_id;
  std::string plan_step_id;
  nlohmann::json evidence_ids;
};

std::string required_host_text(const nlohmann::json& params, const char* key);

nlohmann::json json_field_or(const nlohmann::json& params, const char* key,
                             const nlohmann::json& fallback) {
  const auto found = params.find(key);
  if (found == params.end() || found->is_null()) return fallback;
  return *found;
}

bool is_call_identity(const std::string& value) {
  if (value.empty() || value.size() > 256) return false;
  return std::none_of(value.begin(), value.end(), [](unsigned char byte) {
    return byte < 0x20 || byte == 0x7f;
  });
}

bool ascii_lower_equal(const std::string& left, const std::string& right) {
  if (left.size() != right.size()) return false;
  return std::equal(left.begin(), left.end(), right.begin(),
                    [](unsigned char a, unsigned char b) {
                      return std::tolower(a) == std::tolower(b);
                    });
}

void validate_ssh_filesystem_action_binding(DesktopDatabase& database,
                                            const FilesystemActionBinding& binding) {
  const auto reject = [](const char* message) { return std::runtime_error(message); };
  if (json_text_field(binding.target, "host") != "remote" ||
      json_text_field(binding.target, "serverId") != binding.server_id ||
      !binding.evidence_ids.is_array() || binding.evidence_ids.size() > 128) {
    throw reject("the SSH filesystem action target binding is invalid");
  }
  {
    std::set<std::string> seen;
    for (const auto& item : binding.evidence_ids) {
      if (!item.is_string() || !seen.insert(item.get<std::string>()).second) {
        throw reject("the SSH filesystem action target binding is invalid");
      }
    }
  }
  auto task = load_investigation_task(database, binding.task_id);
  if (!task.has_value()) {
    throw reject("the durable investigation task was not found");
  }
  const nlohmann::json* server = nullptr;
  const auto servers = database.list_servers().value("servers", nlohmann::json::array());
  for (const auto& candidate : servers) {
    if (json_text_field(candidate, "id") == binding.server_id) {
      server = &candidate;
      break;
    }
  }
  if (server == nullptr) {
    throw reject("the selected SSH server is not available");
  }
  const auto metadata = server->value("metadata", nlohmann::json::object());
  if (json_text_field(metadata, "environment") !=
      json_text_field(binding.target, "environment")) {
    throw reject("the SSH filesystem action environment does not match the server");
  }
  const auto workspace_id = json_text_field(binding.target, "workspaceId");
  if (!workspace_id.empty()) {
    bool attached = false;
    const auto workspace_ids = metadata.value("workspaceIds", nlohmann::json::array());
    if (workspace_ids.is_array()) {
      for (const auto& id : workspace_ids) {
        if (id.is_string() && id.get<std::string>() == workspace_id) attached = true;
      }
    }
    if (!attached) {
      throw reject("the SSH filesystem action target is not attached to that workspace");
    }
  }
  if (auto deviation = check_plan_for_tool(database, *task, binding.tool_name, binding.target,
                                           binding.input, binding.plan_id, binding.plan_step_id,
                                           binding.evidence_ids);
      deviation.has_value()) {
    throw std::runtime_error(json_text_field(*deviation, "message"));
  }
  auto plan = database.latest_investigation_plan(binding.task_id);
  if (!plan.has_value()) {
    throw reject("the active filesystem action plan is unavailable");
  }
  constexpr const char* kMutationDenied =
      "filesystem mutation is available only in the current action plan step";
  if (json_text_field(*plan, "currentStepId") != binding.plan_step_id) {
    throw reject(kMutationDenied);
  }
  bool action_step = false;
  for (const auto& step : plan->value("steps", nlohmann::json::array())) {
    if (json_text_field(step, "id") == binding.plan_step_id &&
        json_text_field(step, "kind") == "action") {
      action_step = true;
    }
  }
  if (!action_step) throw reject(kMutationDenied);
}

std::string required_host_text(const nlohmann::json& params, const char* key) {
  const auto found = params.find(key);
  if (found == params.end() || !found->is_string()) {
    throw std::runtime_error("The SSH filesystem backup request is invalid");
  }
  return found->get<std::string>();
}

struct BackupCleanupItem {
  std::string path;
  std::string backup_path;
  std::string expected_revision;
};

BackupCleanupItem parse_backup_cleanup_item(const nlohmann::json& value) {
  if (!value.is_object()) {
    throw std::runtime_error("filesystem.backup.cleanup input is invalid");
  }
  const auto read = [&value](const char* key) {
    const auto found = value.find(key);
    if (found == value.end() || !found->is_string()) {
      throw std::runtime_error("filesystem.backup.cleanup input is invalid");
    }
    return found->get<std::string>();
  };
  return BackupCleanupItem{read("path"), read("backupPath"), read("expectedRevision")};
}

struct BackupCleanupRequest {
  bool batch = false;
  std::vector<BackupCleanupItem> items;
};

// The single-item shape and the batch shape share one object so a call that mixes
// both is rejected with a named problem rather than a silent half-parse.
BackupCleanupRequest resolve_backup_cleanup_input(const nlohmann::json& input) {
  const auto present = [&input](const char* key) {
    return input.contains(key) && !input.at(key).is_null();
  };
  if (!input.is_object()) {
    throw std::runtime_error("filesystem.backup.cleanup input is invalid");
  }
  const auto has_items = present("items");
  const auto has_path = present("path");
  const auto has_backup = present("backupPath");
  const auto has_revision = present("expectedRevision");
  if (has_items && !has_path && !has_backup && !has_revision) {
    const auto& raw = input.at("items");
    if (!raw.is_array() || raw.empty() || raw.size() > 32) {
      throw std::runtime_error(
          "filesystem.backup.cleanup items must contain between 1 and 32 entries");
    }
    BackupCleanupRequest request;
    request.batch = true;
    std::set<std::string> seen;
    for (const auto& entry : raw) {
      auto item = parse_backup_cleanup_item(entry);
      if (!seen.insert(item.path + '\0' + item.backup_path).second) {
        throw std::runtime_error(
            "filesystem.backup.cleanup items must be unique by path and backupPath");
      }
      request.items.push_back(std::move(item));
    }
    return request;
  }
  if (!has_items && has_path && has_backup && has_revision) {
    BackupCleanupRequest request;
    request.items.push_back(parse_backup_cleanup_item(
        {{"path", input.at("path")},
         {"backupPath", input.at("backupPath")},
         {"expectedRevision", input.at("expectedRevision")}}));
    return request;
  }
  throw std::runtime_error(
      "filesystem.backup.cleanup accepts either one exact backup (path, backupPath, "
      "expectedRevision) or a bounded items array, never both");
}

bool cross_task_cleanup_is_plan_bound(DesktopDatabase& database, const std::string& task_id,
                                      const std::string& plan_id,
                                      const std::string& plan_step_id,
                                      const BackupCleanupItem& item) {
  auto plan = database.latest_investigation_plan(task_id);
  if (!plan.has_value() || json_text_field(*plan, "id") != plan_id) return false;
  const auto steps = plan->value("steps", nlohmann::json::array());
  const nlohmann::json* step = nullptr;
  for (const auto& candidate : steps) {
    if (json_text_field(candidate, "id") == plan_step_id) step = &candidate;
  }
  if (step == nullptr || json_text_field(*step, "kind") != "action" ||
      !json_bool_field(*step, "requiresApproval")) {
    return false;
  }
  bool allows_cleanup = false;
  const auto allowed = step->value("allowedTools", nlohmann::json::array());
  if (allowed.is_array()) {
    for (const auto& tool : allowed) {
      if (tool.is_string() && tool.get<std::string>() == "filesystem.backup.cleanup") {
        allows_cleanup = true;
      }
    }
  }
  if (!allows_cleanup) return false;
  const auto bindings = step->value("inputBindings", nlohmann::json::object());
  if (!bindings.is_object()) return false;
  const auto found = bindings.find("items");
  if (found == bindings.end() || !found->is_string()) return false;
  nlohmann::json expected_items;
  try {
    expected_items = nlohmann::json::parse(found->get<std::string>());
  } catch (const nlohmann::json::exception&) {
    return false;
  }
  if (!expected_items.is_array()) return false;
  for (const auto& candidate : expected_items) {
    if (!candidate.is_object()) continue;
    if (json_text_field(candidate, "path") == item.path &&
        json_text_field(candidate, "backupPath") == item.backup_path &&
        ascii_lower_equal(json_text_field(candidate, "expectedRevision"),
                          item.expected_revision)) {
      return true;
    }
  }
  return false;
}

nlohmann::json handle_filesystem_backup_register(DesktopDatabase& database,
                                                 const nlohmann::json& params) {
  if (!params.is_object()) {
    throw std::runtime_error("The SSH filesystem backup request is invalid");
  }
  const auto server_id = required_host_text(params, "serverId");
  const auto path = required_host_text(params, "path");
  const auto backup_path = required_host_text(params, "backupPath");
  const auto revision = required_host_text(params, "revision");
  const auto task_id = required_host_text(params, "taskId");
  const auto plan_id = required_host_text(params, "planId");
  const auto plan_step_id = required_host_text(params, "planStepId");
  const auto trace_id = required_host_text(params, "traceId");
  const auto call_id = required_host_text(params, "callId");
  const auto bytes_field = params.find("bytesBackedUp");
  if (bytes_field == params.end() || !bytes_field->is_number_integer()) {
    throw std::runtime_error("filesystem backup exceeds the host size limit");
  }
  const auto bytes_backed_up = bytes_field->get<std::int64_t>();
  if (bytes_backed_up < 0 ||
      static_cast<std::uint64_t>(bytes_backed_up) > kMaxAgentBackupBytes) {
    throw std::runtime_error("filesystem backup exceeds the host size limit");
  }
  require_agent_cleanup_request(path, backup_path, revision);
  if (!is_call_identity(trace_id) || !is_call_identity(call_id)) {
    throw std::runtime_error(
        "filesystem backup registration is missing its durable call identity");
  }
  const nlohmann::json input = {{"path", path}};
  validate_ssh_filesystem_action_binding(
      database,
      FilesystemActionBinding{
          server_id, task_id, "filesystem.backup", input,
          json_field_or(params, "target", nlohmann::json::object()), plan_id, plan_step_id,
          json_field_or(params, "evidenceIds", nlohmann::json::array())});
  const auto now = iso8601_now();
  const nlohmann::json record = {
      {"id", backup_record_id(server_id, backup_path)},
      {"serverId", server_id},
      {"taskId", task_id},
      {"planId", plan_id},
      {"planStepId", plan_step_id},
      {"traceId", trace_id},
      {"callId", call_id},
      {"path", path},
      {"backupPath", backup_path},
      {"revision", revision},
      {"bytesBackedUp", bytes_backed_up},
      {"status", "available"},
      {"createdAt", now},
      {"updatedAt", now},
  };
  try {
    database.insert_filesystem_backup(record);
  } catch (const std::exception&) {
    throw std::runtime_error(
        "backup exists remotely but could not be registered in the host ledger; preserve it and "
        "reconcile before restoring");
  }
  return {{"registered", true}, {"backupPath", backup_path}};
}

nlohmann::json handle_filesystem_backup_restored(DesktopDatabase& database,
                                                 const nlohmann::json& params) {
  if (!params.is_object()) {
    throw std::runtime_error("The SSH filesystem restore request is invalid");
  }
  const auto server_id = required_host_text(params, "serverId");
  const auto path = required_host_text(params, "path");
  const auto backup_path = required_host_text(params, "backupPath");
  const auto expected_revision = required_host_text(params, "expectedRevision");
  const auto revision = required_host_text(params, "revision");
  const auto task_id = required_host_text(params, "taskId");
  const auto plan_id = required_host_text(params, "planId");
  const auto plan_step_id = required_host_text(params, "planStepId");
  const auto trace_id = required_host_text(params, "traceId");
  const auto call_id = required_host_text(params, "callId");
  const auto read_bytes = [&params](const char* key) {
    const auto found = params.find(key);
    if (found == params.end() || !found->is_number_integer()) {
      throw std::runtime_error("filesystem restore result is invalid");
    }
    const auto value = found->get<std::int64_t>();
    if (value < 0 || static_cast<std::uint64_t>(value) > kMaxAgentBackupBytes) {
      throw std::runtime_error("filesystem restore result is invalid");
    }
    return value;
  };
  const auto bytes_before = read_bytes("bytesBefore");
  const auto bytes_after = read_bytes("bytesAfter");
  if (!is_call_identity(trace_id) || !is_call_identity(call_id)) {
    throw std::runtime_error("filesystem restore result or durable call identity is invalid");
  }
  require_agent_restore_request(path, backup_path, expected_revision);
  const nlohmann::json input = {{"path", path},
                                {"backupPath", backup_path},
                                {"expectedRevision", expected_revision}};
  validate_ssh_filesystem_action_binding(
      database,
      FilesystemActionBinding{
          server_id, task_id, "filesystem.restore", input,
          json_field_or(params, "target", nlohmann::json::object()), plan_id, plan_step_id,
          json_field_or(params, "evidenceIds", nlohmann::json::array())});
  const auto backup = database.find_available_filesystem_backup(server_id, path, backup_path);
  if (!backup.has_value()) {
    throw std::runtime_error("filesystem backup is no longer available");
  }
  if (json_text_field(*backup, "taskId") != task_id) {
    throw std::runtime_error(
        "filesystem.restore cannot use a backup owned by another investigation task");
  }
  if (!ascii_lower_equal(json_text_field(*backup, "revision"), revision) ||
      bytes_after != backup->value("bytesBackedUp", std::int64_t{-1})) {
    throw std::runtime_error("filesystem restore result does not match the host backup ledger");
  }
  try {
    database.mark_filesystem_backup_restored(server_id, path, backup_path, iso8601_now());
  } catch (const std::exception&) {
    throw std::runtime_error(
        "the remote file was restored but its host backup ledger could not be consumed; reconcile "
        "before another restore");
  }
  return {{"restored", true},
          {"path", path},
          {"backupPath", backup_path},
          {"revision", revision},
          {"bytesBefore", bytes_before},
          {"bytesAfter", bytes_after}};
}

nlohmann::json handle_filesystem_backup_deleted(DesktopDatabase& database,
                                                const nlohmann::json& params) {
  if (!params.is_object()) {
    throw std::runtime_error("The SSH filesystem cleanup request is invalid");
  }
  const auto server_id = required_host_text(params, "serverId");
  const auto revision = required_host_text(params, "revision");
  const auto task_id = required_host_text(params, "taskId");
  const auto plan_id = required_host_text(params, "planId");
  const auto plan_step_id = required_host_text(params, "planStepId");
  const auto trace_id = required_host_text(params, "traceId");
  const auto call_id = required_host_text(params, "callId");
  const auto bytes_field = params.find("bytesDeleted");
  if (bytes_field == params.end() || !bytes_field->is_number_integer()) {
    throw std::runtime_error("filesystem cleanup result is invalid");
  }
  const auto bytes_deleted = bytes_field->get<std::int64_t>();
  if (bytes_deleted < 0 || static_cast<std::uint64_t>(bytes_deleted) > kMaxAgentBackupBytes) {
    throw std::runtime_error("filesystem cleanup result is invalid");
  }
  if (!is_call_identity(trace_id) || !is_call_identity(call_id)) {
    throw std::runtime_error("filesystem cleanup result or durable call identity is invalid");
  }
  const auto input = json_field_or(params, "input", nlohmann::json::object());
  const auto request = resolve_backup_cleanup_input(input);
  const auto item_field = params.find("item");
  if (item_field == params.end()) {
    throw std::runtime_error("filesystem cleanup result is invalid");
  }
  const auto item = parse_backup_cleanup_item(*item_field);
  bool part_of_input = false;
  for (const auto& candidate : request.items) {
    if (candidate.path == item.path && candidate.backup_path == item.backup_path &&
        ascii_lower_equal(candidate.expected_revision, item.expected_revision)) {
      part_of_input = true;
    }
  }
  if (!part_of_input) {
    throw std::runtime_error("the deleted backup is not part of the approved cleanup input");
  }
  require_agent_cleanup_request(item.path, item.backup_path, item.expected_revision);
  validate_ssh_filesystem_action_binding(
      database,
      FilesystemActionBinding{
          server_id, task_id, "filesystem.backup.cleanup", input,
          json_field_or(params, "target", nlohmann::json::object()), plan_id, plan_step_id,
          json_field_or(params, "evidenceIds", nlohmann::json::array())});
  const auto backup =
      database.find_available_filesystem_backup(server_id, item.path, item.backup_path);
  if (!backup.has_value()) {
    throw std::runtime_error("filesystem backup is no longer available");
  }
  const auto same_task = json_text_field(*backup, "taskId") == task_id;
  if (!same_task &&
      !(request.batch &&
        cross_task_cleanup_is_plan_bound(database, task_id, plan_id, plan_step_id, item))) {
    throw std::runtime_error(
        "filesystem.backup.cleanup cannot use a backup owned by another investigation task");
  }
  const auto ledger_revision = json_text_field(*backup, "revision");
  if (!ascii_lower_equal(ledger_revision, item.expected_revision) ||
      !ascii_lower_equal(ledger_revision, revision) ||
      bytes_deleted != backup->value("bytesBackedUp", std::int64_t{-1})) {
    throw std::runtime_error("filesystem cleanup result does not match the host backup ledger");
  }
  try {
    database.mark_filesystem_backup_deleted(server_id, item.path, item.backup_path,
                                            iso8601_now());
  } catch (const std::exception&) {
    throw std::runtime_error("filesystem backup could not be consumed in the host ledger");
  }
  return {{"deleted", true},
          {"path", item.path},
          {"backupPath", item.backup_path},
          {"revision", ledger_revision},
          {"bytesDeleted", bytes_deleted}};
}

// ── SSH filesystem action targets ────────────────────────────────────────────
// Resolve a runnable C++ service target only after the durable plan, the owned
// backup path and (for cleanup) every requested item have been checked.
nlohmann::json handle_ssh_filesystem_backup_target(DesktopDatabase& database,
                                                   const CredentialStore& credentials,
                                                   const nlohmann::json& params) {
  if (!params.is_object()) {
    throw std::runtime_error("the SSH filesystem backup target request is invalid");
  }
  const auto server_id = required_host_text(params, "serverId");
  const auto path = required_host_text(params, "path");
  const auto task_id = required_host_text(params, "taskId");
  const auto plan_id = required_host_text(params, "planId");
  const auto plan_step_id = required_host_text(params, "planStepId");
  const nlohmann::json input = {{"path", path}};
  validate_ssh_filesystem_action_binding(
      database,
      FilesystemActionBinding{
          server_id, task_id, "filesystem.backup", input,
          json_field_or(params, "target", nlohmann::json::object()), plan_id, plan_step_id,
          json_field_or(params, "evidenceIds", nlohmann::json::array())});
  const auto token = backup_token();
  require_agent_backup_request(path, token);
  auto payload = resolve_ssh_service_target(database, credentials, server_id);
  payload["filesystemBackup"] = {{"path", path},
                                 {"backupPath", backup_path_for(path, token)}};
  return payload;
}

nlohmann::json handle_ssh_filesystem_restore_target(DesktopDatabase& database,
                                                    const CredentialStore& credentials,
                                                    const nlohmann::json& params) {
  if (!params.is_object()) {
    throw std::runtime_error("the SSH filesystem restore target request is invalid");
  }
  const auto server_id = required_host_text(params, "serverId");
  const auto path = required_host_text(params, "path");
  const auto backup_path = required_host_text(params, "backupPath");
  const auto expected_revision = required_host_text(params, "expectedRevision");
  const auto task_id = required_host_text(params, "taskId");
  const auto plan_id = required_host_text(params, "planId");
  const auto plan_step_id = required_host_text(params, "planStepId");
  require_agent_restore_request(path, backup_path, expected_revision);
  const nlohmann::json input = {{"path", path},
                                {"backupPath", backup_path},
                                {"expectedRevision", expected_revision}};
  validate_ssh_filesystem_action_binding(
      database,
      FilesystemActionBinding{
          server_id, task_id, "filesystem.restore", input,
          json_field_or(params, "target", nlohmann::json::object()), plan_id, plan_step_id,
          json_field_or(params, "evidenceIds", nlohmann::json::array())});
  const auto backup = database.find_available_filesystem_backup(server_id, path, backup_path);
  if (!backup.has_value()) {
    throw std::runtime_error("filesystem.restore requires an available host-owned backup");
  }
  if (json_text_field(*backup, "taskId") != task_id) {
    throw std::runtime_error(
        "filesystem.restore cannot use a backup owned by another investigation task");
  }
  const auto bytes_backed_up = backup->value("bytesBackedUp", std::int64_t{-1});
  if (bytes_backed_up < 0 ||
      static_cast<std::uint64_t>(bytes_backed_up) > kMaxAgentBackupBytes) {
    throw std::runtime_error("filesystem backup exceeds the host restore limit");
  }
  auto payload = resolve_ssh_service_target(database, credentials, server_id);
  payload["filesystemRestore"] = {{"path", path},
                                  {"backupPath", backup_path},
                                  {"expectedRevision", expected_revision},
                                  {"revision", json_text_field(*backup, "revision")},
                                  {"bytesBackedUp", bytes_backed_up}};
  return payload;
}

nlohmann::json handle_ssh_filesystem_cleanup_target(DesktopDatabase& database,
                                                    const CredentialStore& credentials,
                                                    const nlohmann::json& params) {
  if (!params.is_object()) {
    throw std::runtime_error("the SSH filesystem cleanup target request is invalid");
  }
  const auto server_id = required_host_text(params, "serverId");
  const auto task_id = required_host_text(params, "taskId");
  const auto plan_id = required_host_text(params, "planId");
  const auto plan_step_id = required_host_text(params, "planStepId");
  const auto run_id = required_host_text(params, "runId");
  const auto trace_id = required_host_text(params, "traceId");
  const auto call_id = required_host_text(params, "callId");
  if (!is_call_identity(run_id) || !is_call_identity(trace_id) || !is_call_identity(call_id)) {
    throw std::runtime_error("filesystem cleanup durable call identity is invalid");
  }
  const auto input_field = params.find("input");
  if (input_field == params.end() || !input_field->is_object()) {
    throw std::runtime_error("filesystem.backup.cleanup input is invalid");
  }
  const auto input = *input_field;
  const auto request = resolve_backup_cleanup_input(input);
  validate_ssh_filesystem_action_binding(
      database,
      FilesystemActionBinding{
          server_id, task_id, "filesystem.backup.cleanup", input,
          json_field_or(params, "target", nlohmann::json::object()), plan_id, plan_step_id,
          json_field_or(params, "evidenceIds", nlohmann::json::array())});
  // Reject any malformed path for the whole request before reporting any item
  // as executable.
  for (const auto& item : request.items) {
    require_agent_cleanup_request(item.path, item.backup_path, item.expected_revision);
  }
  nlohmann::json bindings = nlohmann::json::array();
  bool has_ready_item = false;
  for (const auto& item : request.items) {
    nlohmann::json binding = {{"path", item.path},
                              {"backupPath", item.backup_path},
                              {"expectedRevision", item.expected_revision}};
    std::optional<nlohmann::json> backup;
    try {
      backup = database.find_available_filesystem_backup(server_id, item.path, item.backup_path);
    } catch (const std::exception& error) {
      if (!request.batch) {
        throw std::runtime_error("could not read the filesystem backup ledger: " +
                                 std::string(error.what()));
      }
      auto reason = "could not read the filesystem backup ledger: " + std::string(error.what());
      if (reason.size() > 1024) reason.resize(1024);
      binding["status"] = "failed";
      binding["reason"] = reason;
      bindings.push_back(std::move(binding));
      continue;
    }
    if (!backup.has_value()) {
      if (!request.batch) {
        throw std::runtime_error(
            "filesystem.backup.cleanup requires an available host-owned backup for this server and "
            "target path");
      }
      binding["status"] = "skipped";
      binding["reason"] = "no available host ledger record for this server and backup path";
      bindings.push_back(std::move(binding));
      continue;
    }
    const auto same_task = json_text_field(*backup, "taskId") == task_id;
    const bool cross_task_allowed =
        request.batch && !same_task &&
        cross_task_cleanup_is_plan_bound(database, task_id, plan_id, plan_step_id, item);
    if (!same_task && !cross_task_allowed) {
      if (!request.batch) {
        throw std::runtime_error(
            "filesystem.backup.cleanup cannot use a backup owned by another investigation task");
      }
      binding["status"] = "skipped";
      binding["reason"] =
          "owned by another investigation task and not bound by the approved rotation step";
      bindings.push_back(std::move(binding));
      continue;
    }
    if (!ascii_lower_equal(json_text_field(*backup, "revision"), item.expected_revision)) {
      if (!request.batch) {
        throw std::runtime_error(
            "expectedRevision does not match the revision recorded for this backup; preserve it "
            "and re-read the ledger");
      }
      binding["status"] = "failed";
      binding["reason"] = "expectedRevision does not match the revision recorded for this backup";
      bindings.push_back(std::move(binding));
      continue;
    }
    const auto ledger_bytes = backup->value("bytesBackedUp", std::int64_t{-1});
    if (ledger_bytes < 0 || static_cast<std::uint64_t>(ledger_bytes) > kMaxAgentBackupBytes) {
      if (!request.batch) {
        throw std::runtime_error("the host backup ledger has an invalid byte count");
      }
      binding["status"] = "failed";
      binding["reason"] = "the host backup ledger has an invalid byte count";
      bindings.push_back(std::move(binding));
      continue;
    }
    binding["status"] = "ready";
    binding["revision"] = json_text_field(*backup, "revision");
    binding["bytesBackedUp"] = ledger_bytes;
    if (const auto owner = backup->find("taskId"); owner != backup->end()) {
      binding["ownerTaskId"] = *owner;
    } else {
      binding["ownerTaskId"] = nullptr;
    }
    has_ready_item = true;
    bindings.push_back(std::move(binding));
  }
  nlohmann::json payload = nlohmann::json::object();
  if (has_ready_item) {
    payload = resolve_ssh_service_target(database, credentials, server_id);
  }
  payload["filesystemCleanup"] = {{"items", std::move(bindings)}};
  return payload;
}

/* ── SSH effectful action targets (systemd/docker/package/server.exec) ─────── */
// A durable task, an active ChangePlan step and a host-minted approval ticket are
// the prerequisites for the four effectful SSH actions. The host validates the
// exact input, the task scope, the plan step and the one-shot ticket before it
// resolves a runnable C++ service target. This mirrors the retired Rust
// `resolve_ssh_effectful_action_target`/`finish_ssh_effectful_action`.

constexpr int kDefaultSystemdRestartTimeout = 30;
constexpr int kMaxSystemdRestartTimeout = 120;
constexpr int kDefaultDockerRestartTimeout = 10;
constexpr int kMaxDockerRestartTimeout = 120;
constexpr int kDefaultPackageInstallTimeout = 180;
constexpr int kMaxPackageInstallTimeout = 600;
constexpr std::size_t kServerExecMaxCommandChars = 16384;
constexpr std::size_t kServerExecMaxPurposeChars = 512;
constexpr std::uint64_t kServerExecMaxTimeoutMs = 120000;
constexpr std::uint64_t kServerExecMaxOutputBytes = 65536;
constexpr std::size_t kServerExecMaxEnvVars = 16;

bool is_ascii_alphanumeric(unsigned char byte) {
  return (byte >= '0' && byte <= '9') || (byte >= 'A' && byte <= 'Z') ||
         (byte >= 'a' && byte <= 'z');
}

bool json_uint_value(const nlohmann::json& value, std::uint64_t& out) {
  if (value.is_number_unsigned()) {
    out = value.get<std::uint64_t>();
    return true;
  }
  if (value.is_number_integer()) {
    const auto signed_value = value.get<std::int64_t>();
    if (signed_value < 0) return false;
    out = static_cast<std::uint64_t>(signed_value);
    return true;
  }
  return false;
}

bool json_int32_value(const nlohmann::json& value, int& out) {
  if (value.is_number_unsigned()) {
    const auto unsigned_value = value.get<std::uint64_t>();
    if (unsigned_value > 2147483647ULL) return false;
    out = static_cast<int>(unsigned_value);
    return true;
  }
  if (value.is_number_integer()) {
    const auto signed_value = value.get<std::int64_t>();
    if (signed_value < -2147483648LL || signed_value > 2147483647LL) return false;
    out = static_cast<int>(signed_value);
    return true;
  }
  return false;
}

/** Returns false when a present, non-null field is not a non-negative integer. */
bool optional_uint_field(const nlohmann::json& input, const char* key, bool& present,
                         std::uint64_t& out) {
  present = false;
  const auto found = input.find(key);
  if (found == input.end() || found->is_null()) return true;
  present = true;
  return json_uint_value(*found, out);
}

bool safe_systemd_service_ref(const std::string& value) {
  if (value.size() < 8 || value.size() > 128 ||
      value.compare(value.size() - 8, 8, ".service") != 0) {
    return false;
  }
  if (!is_ascii_alphanumeric(static_cast<unsigned char>(value.front()))) return false;
  return std::all_of(value.begin() + 1, value.end(), [](unsigned char byte) {
    return is_ascii_alphanumeric(byte) || byte == '_' || byte == '.' || byte == '@' ||
           byte == ':' || byte == '-';
  });
}

bool safe_container_ref(const std::string& value) {
  if (value.empty() || value.size() > 128) return false;
  if (!is_ascii_alphanumeric(static_cast<unsigned char>(value.front()))) return false;
  return std::all_of(value.begin() + 1, value.end(), [](unsigned char byte) {
    return is_ascii_alphanumeric(byte) || byte == '_' || byte == '.' || byte == '-';
  });
}

bool safe_package_ref(const std::string& value) {
  if (value.empty() || value.size() > 128) return false;
  if (!is_ascii_alphanumeric(static_cast<unsigned char>(value.front()))) return false;
  return std::all_of(value.begin() + 1, value.end(), [](unsigned char byte) {
    return is_ascii_alphanumeric(byte) || byte == '+' || byte == '.' || byte == '_' ||
           byte == ':' || byte == '-';
  });
}

bool safe_package_version(const std::string& value) {
  if (value.empty() || value.size() > 128) return false;
  if (!is_ascii_alphanumeric(static_cast<unsigned char>(value.front()))) return false;
  return std::all_of(value.begin() + 1, value.end(), [](unsigned char byte) {
    return is_ascii_alphanumeric(byte) || byte == '+' || byte == '.' || byte == '_' ||
           byte == ':' || byte == '~' || byte == '^' || byte == '-';
  });
}

/** Parsed `server.exec` input; the fields the host re-checks on completion. */
struct ServerExecInput {
  std::string command;
  std::string purpose;
  std::uint64_t timeout_ms = 0;
  std::uint64_t max_output_bytes = 0;
  std::optional<std::string> workdir;
};

bool valid_server_exec_workdir(const std::string& workdir) {
  if (workdir.empty() || workdir.size() > 4096 || workdir.front() != '/' ||
      workdir.find('\0') != std::string::npos || has_control_characters(workdir)) {
    return false;
  }
  std::size_t begin = 0;
  for (;;) {
    const auto end = workdir.find('/', begin);
    const auto part = workdir.substr(begin, end == std::string::npos
                                                ? std::string::npos
                                                : end - begin);
    if (part == "." || part == "..") return false;
    if (end == std::string::npos) break;
    begin = end + 1;
  }
  return true;
}

/** Mirrors the retired Rust `validate_server_exec_input`; empty = valid. */
std::string parse_server_exec_input(const nlohmann::json& input, ServerExecInput& out) {
  if (!only_fields(input,
                   {"command", "purpose", "timeoutMs", "maxOutputBytes", "workdir", "env"},
                   {"command", "purpose", "timeoutMs", "maxOutputBytes"})) {
    return "server.exec input is invalid";
  }
  if (!input.at("command").is_string() || !input.at("purpose").is_string() ||
      !json_uint_value(input.at("timeoutMs"), out.timeout_ms) ||
      !json_uint_value(input.at("maxOutputBytes"), out.max_output_bytes)) {
    return "server.exec input is invalid";
  }
  out.command = input.at("command").get<std::string>();
  out.purpose = input.at("purpose").get<std::string>();
  if (trim_ascii(out.command).empty() ||
      utf8_character_count(out.command) > kServerExecMaxCommandChars ||
      out.command.find('\0') != std::string::npos) {
    return "command must contain 1 to " + std::to_string(kServerExecMaxCommandChars) +
           " characters and no NUL";
  }
  if (trim_ascii(out.purpose).empty() ||
      utf8_character_count(out.purpose) > kServerExecMaxPurposeChars ||
      has_control_characters(out.purpose)) {
    return "purpose must contain 1 to " + std::to_string(kServerExecMaxPurposeChars) +
           " printable characters";
  }
  if (out.timeout_ms == 0 || out.timeout_ms > kServerExecMaxTimeoutMs) {
    return "timeoutMs must be between 1 and " + std::to_string(kServerExecMaxTimeoutMs);
  }
  if (out.max_output_bytes == 0 || out.max_output_bytes > kServerExecMaxOutputBytes) {
    return "maxOutputBytes must be between 1 and " +
           std::to_string(kServerExecMaxOutputBytes);
  }
  if (const auto found = input.find("workdir"); found != input.end() && !found->is_null()) {
    if (!found->is_string()) return "server.exec input is invalid";
    out.workdir = found->get<std::string>();
    if (!valid_server_exec_workdir(*out.workdir)) {
      return "workdir must be a bounded absolute POSIX path without dot traversal or control "
             "characters";
    }
  }
  if (const auto found = input.find("env"); found != input.end() && !found->is_null()) {
    if (!found->is_object()) return "server.exec input is invalid";
    if (found->size() > kServerExecMaxEnvVars) {
      return "env may contain at most " + std::to_string(kServerExecMaxEnvVars) + " variables";
    }
    for (auto item = found->begin(); item != found->end(); ++item) {
      const auto allowed_name =
          is_one_of(item.key(), {"LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES", "LC_TIME",
                                 "LC_NUMERIC", "LC_COLLATE", "TZ", "TERM"});
      const auto valid_value = item.value().is_string() &&
                               item.value().get<std::string>().size() <= 256 &&
                               !has_control_characters(item.value().get<std::string>());
      if (!allowed_name || !valid_value) {
        return "environment variable `" + item.key() +
               "` is not on the non-sensitive allowlist or has an invalid value";
      }
    }
  }
  return {};
}

/** Returns the `invalid_input` envelope to report, or `nullopt` when valid. */
std::optional<nlohmann::json> validate_ssh_effectful_action_input(
    const nlohmann::json& tool_call) {
  const auto tool_name = json_text_field(tool_call, "toolName");
  const auto input = tool_call.value("input", nlohmann::json::object());
  const auto fail = [](const std::string& message) {
    return host_tool_failed("invalid_input", message, false, std::nullopt);
  };
  if (tool_name == kToolSystemdRestart) {
    if (!only_fields(input, {"service", "timeoutSeconds"}, {"service"}) ||
        !input.at("service").is_string()) {
      return fail("systemd.restart input is invalid");
    }
    if (!safe_systemd_service_ref(input.at("service").get<std::string>())) {
      return fail("service must be a .service unit without shell metacharacters");
    }
    bool present = false;
    std::uint64_t timeout = 0;
    if (!optional_uint_field(input, "timeoutSeconds", present, timeout)) {
      return fail("systemd.restart input is invalid");
    }
    if (present && (timeout < 1 || timeout > kMaxSystemdRestartTimeout)) {
      return fail("timeoutSeconds must be between 1 and " +
                  std::to_string(kMaxSystemdRestartTimeout));
    }
    return std::nullopt;
  }
  if (tool_name == kToolDockerRestart) {
    if (!only_fields(input, {"container", "timeoutSeconds"}, {"container"}) ||
        !input.at("container").is_string()) {
      return fail("docker.restart input is invalid");
    }
    if (!safe_container_ref(input.at("container").get<std::string>())) {
      return fail("container must be a Docker name or id without shell metacharacters");
    }
    bool present = false;
    std::uint64_t timeout = 0;
    if (!optional_uint_field(input, "timeoutSeconds", present, timeout)) {
      return fail("docker.restart input is invalid");
    }
    if (present && (timeout < 1 || timeout > kMaxDockerRestartTimeout)) {
      return fail("timeoutSeconds must be between 1 and " +
                  std::to_string(kMaxDockerRestartTimeout));
    }
    return std::nullopt;
  }
  if (tool_name == kToolPackageInstall) {
    if (!only_fields(input, {"manager", "package", "version", "timeoutSeconds"},
                     {"manager", "package"}) ||
        !input.at("manager").is_string() || !input.at("package").is_string()) {
      return fail("package.install input is invalid");
    }
    if (!is_one_of(input.at("manager").get<std::string>(), {"apt", "dnf"})) {
      return fail("package manager must be apt or dnf");
    }
    if (!safe_package_ref(input.at("package").get<std::string>())) {
      return fail("package must be a safe reference without shell metacharacters");
    }
    if (const auto found = input.find("version"); found != input.end() && !found->is_null()) {
      if (!found->is_string()) return fail("package.install input is invalid");
      if (!safe_package_version(found->get<std::string>())) {
        return fail("package version contains unsupported characters");
      }
    }
    bool present = false;
    std::uint64_t timeout = 0;
    if (!optional_uint_field(input, "timeoutSeconds", present, timeout)) {
      return fail("package.install input is invalid");
    }
    if (present && (timeout < 1 || timeout > kMaxPackageInstallTimeout)) {
      return fail("timeoutSeconds must be between 1 and " +
                  std::to_string(kMaxPackageInstallTimeout));
    }
    return std::nullopt;
  }
  if (tool_name == kToolServerExec) {
    ServerExecInput parsed;
    const auto error = parse_server_exec_input(input, parsed);
    if (!error.empty()) return fail(error);
    return std::nullopt;
  }
  return host_tool_failed("denied_by_policy",
                          "the requested SSH action is not supported by this route", false,
                          std::nullopt);
}

/** Validated `server.exec` result; bounds mirror the retired Rust parser. */
struct HostServerExecResult {
  std::string state;
  int exit_code = 0;
  std::string stdout_text;
  std::string stderr_text;
  bool stdout_truncated = false;
  bool stderr_truncated = false;
  std::uint64_t duration_ms = 0;
};

bool parse_host_server_exec_result(const nlohmann::json& value,
                                   const ServerExecInput& input,
                                   HostServerExecResult& out) {
  const std::set<std::string> fields = {"state",       "exitCode",        "stdout",
                                        "stderr",      "stdoutTruncated", "stderrTruncated",
                                        "durationMs"};
  if (!only_fields(value, fields, fields)) return false;
  if (!value.at("state").is_string() || !value.at("stdout").is_string() ||
      !value.at("stderr").is_string() || !value.at("stdoutTruncated").is_boolean() ||
      !value.at("stderrTruncated").is_boolean() ||
      !json_int32_value(value.at("exitCode"), out.exit_code) ||
      !json_uint_value(value.at("durationMs"), out.duration_ms)) {
    return false;
  }
  out.state = value.at("state").get<std::string>();
  out.stdout_text = value.at("stdout").get<std::string>();
  out.stderr_text = value.at("stderr").get<std::string>();
  out.stdout_truncated = value.at("stdoutTruncated").get<bool>();
  out.stderr_truncated = value.at("stderrTruncated").get<bool>();
  if (out.state != "completed" || out.exit_code < 0 || out.exit_code > 255) return false;
  const auto output_bytes = out.stdout_text.size() + out.stderr_text.size();
  if (output_bytes > input.max_output_bytes * 3) return false;
  if (out.duration_ms > input.timeout_ms + 10000) return false;
  return true;
}

nlohmann::json effectful_action_tool_call_object(const nlohmann::json& tool_call,
                                                 const char* method) {
  const std::set<std::string> allowed = {"callId",     "runId",       "traceId",  "toolName",
                                         "input",      "target",      "taskId",   "planId",
                                         "planStepId", "evidenceIds", "approvalId"};
  if (!only_fields(tool_call, allowed,
                   {"callId", "traceId", "toolName", "input", "target"}) ||
      !tool_call.at("callId").is_string() || !tool_call.at("traceId").is_string() ||
      !tool_call.at("toolName").is_string() || !tool_call.at("target").is_object()) {
    throw SidecarRpcError(-32602, std::string("the ") + method + " request is invalid");
  }
  return tool_call;
}

/**
 * Whether an effectful call omitted one of the three durable identifiers. Every
 * Agent-side effectful call must carry taskId, planId and planStepId.
 */
bool effectful_action_requires_durable_plan(const nlohmann::json& tool_call) {
  if (!host_tool_is_effectful(json_text_field(tool_call, "toolName"))) return false;
  const auto present = [&tool_call](const char* key) {
    const auto found = tool_call.find(key);
    return found != tool_call.end() && found->is_string() &&
           !trim_ascii(found->get<std::string>()).empty();
  };
  return !(present("taskId") && present("planId") && present("planStepId"));
}

nlohmann::json host_action_approval_failure(HostApprovalLedger::TicketError error) {
  std::string message;
  switch (error) {
    case HostApprovalLedger::TicketError::Missing:
      message = "host action has no host-issued approval ticket";
      break;
    case HostApprovalLedger::TicketError::Expired:
      message = "host action approval ticket has expired";
      break;
    case HostApprovalLedger::TicketError::Mismatch:
      message = "host action does not match the approved run, call, target, plan, or input";
      break;
    case HostApprovalLedger::TicketError::NotApproved:
      message = "host action approval has not been accepted";
      break;
  }
  return host_tool_failed("denied_by_policy", message, false,
                          nlohmann::json{{"code", "approval_ticket"}});
}

nlohmann::json handle_ssh_effectful_action_target(
    DesktopDatabase& database, const CredentialStore& credentials, HostApprovalLedger& approvals,
    const nlohmann::json& params, std::mutex& replay_mutex,
    std::map<std::pair<std::string, std::string>, nlohmann::json>& replays) {
  if (!only_fields(params, {"toolCall"}, {"toolCall"}) || !params.at("toolCall").is_object()) {
    throw SidecarRpcError(-32602, "the SSH action target request is invalid");
  }
  const auto tool_call =
      effectful_action_tool_call_object(params.at("toolCall"), "SSH action target");
  if (auto failure = validate_ssh_effectful_action_input(tool_call); failure.has_value()) {
    return *failure;
  }
  const auto tool_name = json_text_field(tool_call, "toolName");
  const auto target = tool_call.value("target", nlohmann::json::object());
  const auto server_id = json_text_field(target, "serverId");
  const auto task_id = json_text_field(tool_call, "taskId");
  if (server_id.empty() || task_id.empty()) {
    return host_tool_failed("durable_plan_required",
                            "this SSH action requires a durable remote action plan", false,
                            std::nullopt);
  }
  if (json_text_field(target, "host") != "remote" || server_id.rfind("srv_", 0) != 0) {
    return host_tool_failed("denied_by_policy",
                            "this SSH action requires a resolved remote server", false,
                            std::nullopt);
  }
  const auto run_id = json_text_field(tool_call, "runId");
  if (!is_call_identity(run_id) || trim_ascii(run_id).empty()) {
    return host_tool_failed("invalid_input",
                            "the SSH action requires a valid durable run identity", false,
                            std::nullopt);
  }
  if (effectful_action_requires_durable_plan(tool_call)) {
    return host_tool_failed("durable_plan_required",
                            "this SSH action requires a durable task, ChangePlan and action step",
                            false, nlohmann::json{{"code", "durable_plan_required"}});
  }
  const nlohmann::json* server = nullptr;
  const auto servers = database.list_servers().value("servers", nlohmann::json::array());
  for (const auto& candidate : servers) {
    if (json_text_field(candidate, "id") == server_id) {
      server = &candidate;
      break;
    }
  }
  if (server == nullptr) {
    return host_tool_failed("not_found", "the selected server was not found", false,
                            std::nullopt);
  }
  const auto metadata = server->value("metadata", nlohmann::json::object());
  if (json_text_field(metadata, "environment") != json_text_field(target, "environment")) {
    return host_tool_failed(
        "denied_by_policy",
        "the SSH action target environment does not match the selected server", false,
        std::nullopt);
  }
  const auto workspace_id = json_text_field(target, "workspaceId");
  if (!workspace_id.empty()) {
    bool attached = false;
    for (const auto& id : metadata.value("workspaceIds", nlohmann::json::array())) {
      if (id.is_string() && id.get<std::string>() == workspace_id) attached = true;
    }
    if (!attached) {
      return host_tool_failed(
          "denied_by_policy", "the SSH action target is not attached to that workspace", false,
          std::nullopt);
    }
  }
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return host_tool_failed("not_found", "the durable investigation task was not found", false,
                            std::nullopt);
  }
  if (json_text_field(*task, "serverId") != server_id) {
    return host_tool_failed(
        "denied_by_policy", "the SSH action target does not match the task's registered server",
        false, std::nullopt);
  }
  if (!same_investigation_target(task->value("scope", nlohmann::json::object()), target)) {
    return host_tool_failed("denied_by_policy",
                            "the SSH action target does not match the task scope", false,
                            std::nullopt);
  }
  const auto input = tool_call.value("input", nlohmann::json::object());
  if (auto violation = task_guardrail_violation(*task, tool_name, input);
      violation.has_value()) {
    const auto code = json_text_field(*violation, "code");
    std::optional<nlohmann::json> detail = std::nullopt;
    if (!code.empty()) detail = nlohmann::json{{"code", code}};
    return host_tool_failed("denied_by_policy", json_text_field(*violation, "message"), false,
                            detail);
  }
  const auto binding_field = [&tool_call](const char* key) -> std::optional<nlohmann::json> {
    const auto found = tool_call.find(key);
    if (found == tool_call.end() || found->is_null()) return std::nullopt;
    return *found;
  };
  const auto plan_id = json_text_field(tool_call, "planId");
  const auto plan_step_id = json_text_field(tool_call, "planStepId");
  if (auto deviation = check_plan_for_tool(
          database, *task, tool_name, target, input,
          plan_id.empty() ? std::nullopt : std::optional<std::string>(plan_id),
          plan_step_id.empty() ? std::nullopt : std::optional<std::string>(plan_step_id),
          binding_field("evidenceIds"));
      deviation.has_value()) {
    return host_tool_failed("plan_deviation", json_text_field(*deviation, "message"), false,
                            *deviation);
  }
  if (!supports_ssh_service_target(database, server_id)) {
    return host_tool_failed("unsupported_route",
                            "the selected SSH target is not supported by the C++ SSH service",
                            false, std::nullopt);
  }

  auto decision = prepare_host_tool_call(database, tool_call, replay_mutex, replays);
  if (decision.respond) {
    if (json_text_field(decision.response, "status") == "success") {
      if (tool_name == kToolServerExec) {
        ServerExecInput parsed;
        if (!parse_server_exec_input(input, parsed).empty()) {
          return host_tool_failed("invalid_response",
                                  "the cached server.exec result is invalid", false,
                                  std::nullopt);
        }
        if (!decision.response.contains("output")) {
          return host_tool_failed("invalid_response",
                                  "the cached server.exec result is unavailable", false,
                                  std::nullopt);
        }
        HostServerExecResult cached;
        if (!parse_host_server_exec_result(decision.response.at("output"), parsed, cached)) {
          return host_tool_failed("invalid_response",
                                  "the cached server.exec result is invalid", false,
                                  std::nullopt);
        }
        return {{"status", "already_completed"}, {"output", decision.response.at("output")}};
      }
      return {{"status", "already_completed"}};
    }
    return decision.response;
  }
  const auto token = decision.token;

  HostApprovalLedger::ExecutionBinding binding;
  binding.approval_id = json_text_field(tool_call, "approvalId");
  binding.run_id = std::optional<std::string>(run_id);
  binding.trace_id = json_text_field(tool_call, "traceId");
  binding.call_id = json_text_field(tool_call, "callId");
  binding.tool_name = tool_name;
  binding.input_fingerprint = content_revision_hex(input.dump());
  binding.target_host = json_text_field(target, "host");
  binding.server_id = std::optional<std::string>(server_id);
  binding.workspace_id =
      workspace_id.empty() ? std::nullopt : std::optional<std::string>(workspace_id);
  binding.environment = json_text_field(target, "environment");
  binding.task_id = std::optional<std::string>(task_id);
  binding.plan_id = plan_id.empty() ? std::nullopt : std::optional<std::string>(plan_id);
  binding.plan_step_id =
      plan_step_id.empty() ? std::nullopt : std::optional<std::string>(plan_step_id);
  if (tool_call.contains("evidenceIds") && tool_call.at("evidenceIds").is_array()) {
    std::vector<std::string> ids;
    for (const auto& item : tool_call.at("evidenceIds")) {
      if (item.is_string()) ids.push_back(item.get<std::string>());
    }
    binding.evidence_ids = std::move(ids);
  }
  const auto ticket =
      binding.approval_id.empty()
          ? std::optional<HostApprovalLedger::TicketError>(
                HostApprovalLedger::TicketError::Missing)
          : approvals.consume(binding, [] { return now_epoch_seconds(); });
  if (ticket.has_value()) {
    const auto response = host_action_approval_failure(*ticket);
    finish_host_tool_call(database, tool_call, token, response, replay_mutex, replays);
    return response;
  }

  nlohmann::json target_data;
  try {
    target_data = resolve_ssh_service_target(database, credentials, server_id);
  } catch (const std::exception& error) {
    const auto response = host_tool_failed("transport", error.what(), false, std::nullopt);
    finish_host_tool_call(database, tool_call, token, response, replay_mutex, replays);
    return response;
  }
  nlohmann::json response = {{"status", "success"}, {"targetData", target_data}};
  if (tool_name == kToolServerExec) {
    auto& resolved_target = response["targetData"]["target"];
    if (!resolved_target.is_object() ||
        json_text_field(resolved_target, "target_id") != server_id) {
      return host_tool_failed(
          "denied_by_policy",
          "the resolved SSH target does not match the approved server", false, std::nullopt);
    }
    resolved_target["allow_server_exec"] = true;
  }
  return response;
}

nlohmann::json handle_ssh_effectful_action_finished(
    DesktopDatabase& database, const nlohmann::json& params, std::mutex& replay_mutex,
    std::map<std::pair<std::string, std::string>, nlohmann::json>& replays) {
  if (!only_fields(params, {"toolCall", "outcome", "result"}, {"toolCall", "outcome"}) ||
      !params.at("toolCall").is_object() || !params.at("outcome").is_string()) {
    throw SidecarRpcError(-32602, "the SSH action completion request is invalid");
  }
  const auto tool_call =
      effectful_action_tool_call_object(params.at("toolCall"), "SSH action completion");
  if (auto failure = validate_ssh_effectful_action_input(tool_call); failure.has_value()) {
    return *failure;
  }
  const auto tool_name = json_text_field(tool_call, "toolName");
  const auto call_id = json_text_field(tool_call, "callId");
  const auto trace_id = json_text_field(tool_call, "traceId");
  if (!is_call_identity(call_id) || !is_call_identity(trace_id)) {
    return host_tool_failed("invalid_input", "SSH action completion call identity is invalid",
                            false, std::nullopt);
  }
  const auto outcome = params.at("outcome").get<std::string>();
  const bool has_result = params.contains("result") && !params.at("result").is_null();
  if (tool_name != kToolServerExec && has_result) {
    return host_tool_failed("invalid_input",
                            "only server.exec accepts a command result payload", false,
                            std::nullopt);
  }
  if (tool_name == kToolServerExec && outcome == "unknown" && has_result) {
    return host_tool_failed(
        "invalid_input", "an unknown server.exec outcome cannot include a completed result",
        false, std::nullopt);
  }
  const auto input = tool_call.value("input", nlohmann::json::object());
  std::optional<nlohmann::json> output;
  std::optional<int> server_exec_exit_code;
  if (tool_name == kToolSystemdRestart) {
    output = nlohmann::json{{"service", input.at("service")}, {"restarted", true}};
  } else if (tool_name == kToolDockerRestart) {
    output = nlohmann::json{{"container", input.at("container")}, {"restarted", true}};
  } else if (tool_name == kToolPackageInstall) {
    nlohmann::json installed = {{"manager", input.at("manager")},
                                {"package", input.at("package")},
                                {"installed", true}};
    if (input.contains("version") && !input.at("version").is_null()) {
      installed["version"] = input.at("version");
    }
    output = std::move(installed);
  } else if (tool_name == kToolServerExec) {
    ServerExecInput parsed;
    if (!parse_server_exec_input(input, parsed).empty()) {
      return host_tool_failed("invalid_input", "server.exec completion input is invalid", false,
                              std::nullopt);
    }
    if (has_result) {
      HostServerExecResult result;
      if (!parse_host_server_exec_result(params.at("result"), parsed, result)) {
        return host_tool_failed("invalid_input", "server.exec completion result is invalid",
                                false, std::nullopt);
      }
      output = params.at("result");
      server_exec_exit_code = result.exit_code;
    }
    if (!output.has_value() && outcome == "succeeded") {
      return host_tool_failed(
          "invalid_input", "server.exec success requires its bounded command result", false,
          std::nullopt);
    }
  } else {
    return host_tool_failed("denied_by_policy", "unsupported SSH action", false, std::nullopt);
  }

  nlohmann::json response;
  if (outcome == "succeeded") {
    if (tool_name == kToolServerExec && server_exec_exit_code != 0) {
      return host_tool_failed("invalid_input", "server.exec success must have exit code zero",
                              false, std::nullopt);
    }
    if (!output.has_value()) {
      return host_tool_failed("invalid_input", "SSH action result is missing", false,
                              std::nullopt);
    }
    response = host_tool_success(*output);
  } else if (outcome == "failed") {
    if (tool_name == kToolServerExec && server_exec_exit_code == 0) {
      return host_tool_failed(
          "invalid_input", "server.exec failure cannot carry a successful exit code", false,
          std::nullopt);
    }
    std::optional<nlohmann::json> detail = std::nullopt;
    if (tool_name == kToolServerExec && output.has_value()) detail = output;
    response = host_tool_failed("execution_failed",
                                tool_name + " did not complete successfully", false, detail);
  } else if (outcome == "unknown") {
    response = host_tool_failed("transport", tool_name + " outcome is unknown", true,
                                std::nullopt);
  } else {
    return host_tool_failed("invalid_input", "SSH action outcome is invalid", false,
                            std::nullopt);
  }
  const std::optional<std::pair<std::string, std::string>> token = std::make_pair(trace_id, call_id);
  finish_host_tool_call(database, tool_call, token, response, replay_mutex, replays);
  return {{"status", "success"}, {"finished", true}};
}

/* ── Filesystem backup metadata queries (list + retention scan) ────────────── */

bool valid_remote_query_path(const std::string& value) {
  if (value.empty() || value.size() > 4096) return false;
  if (value.front() != '/') return false;
  return !has_control_characters(value);
}

nlohmann::json filesystem_backup_query_tool_call(const nlohmann::json& params,
                                                 const char* what) {
  if (!only_fields(params, {"toolCall"}, {"toolCall"}) || !params.at("toolCall").is_object()) {
    throw SidecarRpcError(-32602, std::string("the ") + what + " is invalid");
  }
  const auto& tool_call = params.at("toolCall");
  const std::set<std::string> allowed = {"callId",     "runId",      "traceId", "toolName",
                                         "input",      "target",     "taskId",  "planId",
                                         "planStepId", "evidenceIds", "approvalId"};
  if (!only_fields(tool_call, allowed,
                   {"callId", "traceId", "toolName", "input", "target"})) {
    throw SidecarRpcError(-32602, std::string("the ") + what + " is invalid");
  }
  if (!tool_call.at("callId").is_string() || !tool_call.at("traceId").is_string() ||
      !tool_call.at("toolName").is_string() || !tool_call.at("input").is_object() ||
      !tool_call.at("target").is_object()) {
    throw SidecarRpcError(-32602, std::string("the ") + what + " is invalid");
  }
  return tool_call;
}

/** Returns the failure envelope to report, or `nullopt` when the scope is valid. */
std::optional<nlohmann::json> validate_filesystem_backup_query_scope(
    DesktopDatabase& database, const std::string& server_id, const std::string& task_id,
    const nlohmann::json& target) {
  if (json_text_field(target, "host") != "remote" ||
      json_text_field(target, "serverId") != server_id) {
    return host_tool_failed(
        "denied_by_policy",
        "filesystem backup metadata target does not match the selected server", false,
        std::nullopt);
  }
  const nlohmann::json* server = nullptr;
  const auto servers = database.list_servers().value("servers", nlohmann::json::array());
  for (const auto& candidate : servers) {
    if (json_text_field(candidate, "id") == server_id) {
      server = &candidate;
      break;
    }
  }
  if (server == nullptr) {
    return host_tool_failed("not_found",
                            "the selected filesystem backup server was not found", false,
                            std::nullopt);
  }
  const auto metadata = server->value("metadata", nlohmann::json::object());
  if (json_text_field(metadata, "environment") != json_text_field(target, "environment")) {
    return host_tool_failed(
        "denied_by_policy",
        "filesystem backup metadata environment does not match the selected server", false,
        std::nullopt);
  }
  const auto workspace_id = json_text_field(target, "workspaceId");
  if (!workspace_id.empty()) {
    bool attached = false;
    for (const auto& id : metadata.value("workspaceIds", nlohmann::json::array())) {
      if (id.is_string() && id.get<std::string>() == workspace_id) attached = true;
    }
    if (!attached) {
      return host_tool_failed(
          "denied_by_policy",
          "filesystem backup metadata target is not attached to that workspace", false,
          std::nullopt);
    }
  }
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return host_tool_failed("not_found", "the durable investigation task was not found", false,
                            std::nullopt);
  }
  if (json_text_field(*task, "serverId") != server_id) {
    return host_tool_failed(
        "denied_by_policy",
        "filesystem backup metadata target does not match the task's registered server", false,
        std::nullopt);
  }
  if (!same_investigation_target(task->value("scope", nlohmann::json::object()), target)) {
    return host_tool_failed("denied_by_policy",
                            "filesystem backup metadata target does not match the task scope",
                            false, std::nullopt);
  }
  return std::nullopt;
}

HostToolCallDecision prepare_filesystem_backup_query_call(
    DesktopDatabase& database, const nlohmann::json& request, const char* expected_tool,
    std::mutex& replay_mutex,
    std::map<std::pair<std::string, std::string>, nlohmann::json>& replays) {
  HostToolCallDecision decision;
  if (json_text_field(request, "toolName") != expected_tool) {
    decision.respond = true;
    decision.response = host_tool_failed(
        "denied_by_policy",
        "filesystem backup query tool name does not match its host endpoint", false,
        std::nullopt);
    return decision;
  }
  const auto target = request.value("target", nlohmann::json::object());
  const auto server_id = json_text_field(target, "serverId");
  const auto task_id = json_text_field(request, "taskId");
  if (server_id.empty() || task_id.empty()) {
    decision.respond = true;
    decision.response = host_tool_failed(
        "durable_task_required",
        "filesystem backup metadata requires a durable investigation task", false, std::nullopt);
    return decision;
  }
  if (auto failure = validate_filesystem_backup_query_scope(database, server_id, task_id, target);
      failure.has_value()) {
    decision.respond = true;
    decision.response = *failure;
    return decision;
  }
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    decision.respond = true;
    decision.response = host_tool_failed(
        "not_found", "the durable investigation task was not found", false, std::nullopt);
    return decision;
  }
  if (auto violation = task_guardrail_violation(*task, json_text_field(request, "toolName"),
                                                request.value("input", nlohmann::json::object()));
      violation.has_value()) {
    std::optional<nlohmann::json> detail = std::nullopt;
    const auto code = json_text_field(*violation, "code");
    if (!code.empty()) detail = nlohmann::json{{"code", code}};
    decision.respond = true;
    decision.response = host_tool_failed("denied_by_policy",
                                         json_text_field(*violation, "message"), false, detail);
    return decision;
  }
  const auto binding = [&request](const char* key) -> std::optional<nlohmann::json> {
    if (!request.contains(key) || request.at(key).is_null()) return std::nullopt;
    return request.at(key);
  };
  if (auto deviation = check_plan_for_tool(database, *task, json_text_field(request, "toolName"),
                                           target, request.value("input", nlohmann::json::object()),
                                           request.contains("planId") && request.at("planId").is_string()
                                               ? std::optional<std::string>(request.at("planId").get<std::string>())
                                               : std::nullopt,
                                           request.contains("planStepId") && request.at("planStepId").is_string()
                                               ? std::optional<std::string>(request.at("planStepId").get<std::string>())
                                               : std::nullopt,
                                           binding("evidenceIds"));
      deviation.has_value()) {
    decision.respond = true;
    decision.response = host_tool_failed("plan_deviation",
                                         json_text_field(*deviation, "message"), false, *deviation);
    return decision;
  }
  return prepare_host_tool_call(database, request, replay_mutex, replays);
}

nlohmann::json handle_filesystem_backup_list_query(
    DesktopDatabase& database, const nlohmann::json& params, std::mutex& replay_mutex,
    std::map<std::pair<std::string, std::string>, nlohmann::json>& replays) {
  const auto tool_call =
      filesystem_backup_query_tool_call(params, "filesystem backup list query");
  if (json_text_field(tool_call, "toolName") != kToolFilesystemBackupList) {
    return host_tool_failed("denied_by_policy",
                            "filesystem backup list query has an unexpected tool name", false,
                            std::nullopt);
  }
  const auto input = tool_call.value("input", nlohmann::json::object());
  if (!only_fields(input, {"status", "path", "limit"}, {})) {
    return host_tool_failed("invalid_input", "filesystem.backup.list input is invalid", false,
                            std::nullopt);
  }
  std::optional<std::string> path;
  if (input.contains("path") && !input.at("path").is_null()) {
    if (!input.at("path").is_string()) {
      return host_tool_failed("invalid_input", "filesystem.backup.list input is invalid", false,
                              std::nullopt);
    }
    path = input.at("path").get<std::string>();
    if (!valid_remote_query_path(*path)) {
      return host_tool_failed(
          "invalid_input",
          "remote path must be 1-4096 characters, absolute, and free of control characters",
          false, std::nullopt);
    }
  }
  std::optional<std::string> status;
  if (input.contains("status") && !input.at("status").is_null()) {
    if (!input.at("status").is_string()) {
      return host_tool_failed("invalid_input", "filesystem.backup.list input is invalid", false,
                              std::nullopt);
    }
    const auto raw = input.at("status").get<std::string>();
    if (!is_one_of(raw, {"available", "restored", "deleted"})) {
      return host_tool_failed(
          "invalid_input",
          "filesystem.backup.list status must be available, restored, or deleted", false,
          std::nullopt);
    }
    status = raw;
  }
  int limit = 64;
  if (input.contains("limit") && !input.at("limit").is_null()) {
    if (!input.at("limit").is_number_integer()) {
      return host_tool_failed("invalid_input", "filesystem.backup.list input is invalid", false,
                              std::nullopt);
    }
    limit = input.at("limit").get<int>();
  }
  if (limit < 1 || limit > 128) {
    return host_tool_failed("invalid_input",
                            "filesystem.backup.list limit must be between 1 and 128", false,
                            std::nullopt);
  }
  auto decision = prepare_filesystem_backup_query_call(
      database, tool_call, kToolFilesystemBackupList, replay_mutex, replays);
  if (decision.respond) return decision.response;
  const auto target = tool_call.value("target", nlohmann::json::object());
  const auto server_id = json_text_field(target, "serverId");
  const auto task_id = json_text_field(tool_call, "taskId");
  nlohmann::json response;
  try {
    response = host_tool_success(
        database.list_filesystem_backups_for_task(server_id, task_id, status, path, limit));
  } catch (const std::invalid_argument& error) {
    response = host_tool_failed("invalid_input", error.what(), false, std::nullopt);
  } catch (const std::exception& error) {
    response = host_tool_failed("internal", error.what(), false, std::nullopt);
  }
  finish_host_tool_call(database, tool_call, decision.token, response, replay_mutex, replays);
  return response;
}

nlohmann::json handle_filesystem_backup_retention_scan(
    DesktopDatabase& database, const nlohmann::json& params, std::mutex& replay_mutex,
    std::map<std::pair<std::string, std::string>, nlohmann::json>& replays) {
  const auto tool_call =
      filesystem_backup_query_tool_call(params, "filesystem backup retention scan");
  if (json_text_field(tool_call, "toolName") != kToolFilesystemBackupRetention) {
    return host_tool_failed("denied_by_policy",
                            "filesystem backup retention scan has an unexpected tool name",
                            false, std::nullopt);
  }
  const auto input = tool_call.value("input", nlohmann::json::object());
  if (!only_fields(input, {"pathPrefix", "keepLatest", "olderThanDays"}, {})) {
    return host_tool_failed("invalid_input", "filesystem.backup.retention input is invalid",
                            false, std::nullopt);
  }
  const auto has_keep_latest = input.contains("keepLatest") && !input.at("keepLatest").is_null();
  const auto has_older_than =
      input.contains("olderThanDays") && !input.at("olderThanDays").is_null();
  if (!has_keep_latest && !has_older_than) {
    return host_tool_failed(
        "invalid_input", "filesystem.backup.retention requires keepLatest or olderThanDays",
        false, std::nullopt);
  }
  if (has_keep_latest) {
    if (!input.at("keepLatest").is_number_integer() || input.at("keepLatest").get<int>() < 1 ||
        input.at("keepLatest").get<int>() > 64) {
      return host_tool_failed(
          "invalid_input", "filesystem.backup.retention keepLatest must be between 1 and 64",
          false, std::nullopt);
    }
  }
  if (has_older_than) {
    if (!input.at("olderThanDays").is_number_integer() ||
        input.at("olderThanDays").get<std::int64_t>() < 1 ||
        input.at("olderThanDays").get<std::int64_t>() > 3650) {
      return host_tool_failed(
          "invalid_input", "filesystem.backup.retention olderThanDays must be between 1 and 3650",
          false, std::nullopt);
    }
  }
  std::optional<std::string> path_prefix;
  if (input.contains("pathPrefix") && !input.at("pathPrefix").is_null()) {
    if (!input.at("pathPrefix").is_string()) {
      return host_tool_failed("invalid_input", "filesystem.backup.retention input is invalid",
                              false, std::nullopt);
    }
    path_prefix = input.at("pathPrefix").get<std::string>();
    if (!valid_remote_query_path(*path_prefix)) {
      return host_tool_failed(
          "invalid_input",
          "remote path must be 1-4096 characters, absolute, and free of control characters",
          false, std::nullopt);
    }
  }
  auto decision = prepare_filesystem_backup_query_call(
      database, tool_call, kToolFilesystemBackupRetention, replay_mutex, replays);
  if (decision.respond) return decision.response;
  const auto target = tool_call.value("target", nlohmann::json::object());
  const auto server_id = json_text_field(target, "serverId");
  nlohmann::json response;
  try {
    const auto scan = database.list_available_filesystem_backups_for_server(server_id, path_prefix,
                                                                           512);
    nlohmann::json records = nlohmann::json::array();
    for (const auto& record : scan.value("records", nlohmann::json::array())) {
      const auto path = json_text_field(record, "path");
      if (path_prefix.has_value() && path.rfind(*path_prefix, 0) != 0) continue;
      nlohmann::json item = nlohmann::json::object();
      item["id"] = json_text_field(record, "id");
      item["path"] = path;
      item["backupPath"] = json_text_field(record, "backupPath");
      item["revision"] = json_text_field(record, "revision");
      item["taskId"] = optional_json_field(record, "taskId");
      item["createdAt"] = json_text_field(record, "createdAt");
      item["bytesBackedUp"] = record.value("bytesBackedUp", 0);
      records.push_back(std::move(item));
    }
    nlohmann::json output = nlohmann::json::object();
    output["records"] = std::move(records);
    output["truncated"] = scan.value("truncated", false);
    output["evaluatedAtEpochSeconds"] = now_epoch_seconds();
    response = host_tool_success(output);
  } catch (const std::invalid_argument& error) {
    response = host_tool_failed("invalid_input", error.what(), false, std::nullopt);
  } catch (const std::exception& error) {
    response = host_tool_failed("internal", error.what(), false, std::nullopt);
  }
  finish_host_tool_call(database, tool_call, decision.token, response, replay_mutex, replays);
  return response;
}

nlohmann::json handle_finding_record(DesktopDatabase& database, const nlohmann::json& params) {
  if (!params.is_object() || !params.contains("finding") || !params.at("finding").is_object()) {
    return record_failure_json("invalid_input", "invalid finding", false);
  }
  nlohmann::json finding = params.at("finding");
  const auto task_id = json_text_field(finding, "taskId");
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return record_failure_json("not_found", "investigation task was not found", false);
  }
  const auto evidence_ids = finding.value("evidenceIds", nlohmann::json::array());
  if (evidence_ids.empty() && json_text_field(finding, "kind") != "unknown") {
    return record_failure_json(
        "invalid_input", "fact and inference findings require at least one evidence reference",
        false);
  }
  for (const auto& evidence_id : evidence_ids) {
    if (!evidence_id.is_string()) continue;
    auto evidence = database.get_investigation_evidence(evidence_id.get<std::string>());
    if (!evidence.has_value()) {
      return record_failure_json(
          "invalid_input", "evidence `" + evidence_id.get<std::string>() + "` was not found", false);
    }
    if (json_text_field(*evidence, "taskId") != task_id) {
      return record_failure_json("denied_by_policy",
                                 "finding references evidence from another task", false);
    }
  }
  try {
    database.record_investigation_finding(finding);
  } catch (const std::invalid_argument& error) {
    return record_failure_json("invalid_input", error.what(), false);
  }
  return {{"recorded", true}, {"finding", finding}};
}

nlohmann::json handle_brief_record(DesktopDatabase& database, const nlohmann::json& params) {
  if (!params.is_object() || !params.contains("brief") || !params.at("brief").is_object()) {
    return record_failure_json("invalid_input", "invalid decision brief", false);
  }
  nlohmann::json brief = params.at("brief");
  const auto task_id = json_text_field(brief, "taskId");
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return record_failure_json("not_found", "investigation task was not found", false);
  }
  const auto plan_id = json_text_field(brief, "planId");
  if (!plan_id.empty()) {
    auto current_plan = database.latest_investigation_plan(task_id);
    if (!current_plan.has_value() || json_text_field(*current_plan, "id") != plan_id) {
      return record_failure_json("plan_deviation",
                                 "decision brief must describe the current plan revision", false);
    }
  }
  nlohmann::json finding_ids = nlohmann::json::array();
  const auto top_findings = brief.value("findingIds", nlohmann::json::array());
  if (top_findings.is_array()) {
    for (const auto& id : top_findings) finding_ids.push_back(id);
  }
  const auto options = brief.value("options", nlohmann::json::array());
  if (options.is_array()) {
    for (const auto& option : options) {
      const auto option_findings = option.value("findingIds", nlohmann::json::array());
      if (option_findings.is_array()) {
        for (const auto& id : option_findings) finding_ids.push_back(id);
      }
    }
  }
  for (const auto& finding_id : finding_ids) {
    if (!finding_id.is_string()) continue;
    auto finding = database.get_investigation_finding(finding_id.get<std::string>());
    if (!finding.has_value()) {
      return record_failure_json(
          "invalid_input", "finding `" + finding_id.get<std::string>() + "` was not found", false);
    }
    if (json_text_field(*finding, "taskId") != task_id) {
      return record_failure_json("denied_by_policy",
                                 "decision brief references a finding from another task", false);
    }
  }
  try {
    database.record_investigation_brief(brief);
  } catch (const std::invalid_argument& error) {
    return record_failure_json("invalid_input", error.what(), false);
  }
  return {{"recorded", true}, {"brief", brief}};
}

nlohmann::json handle_artifact_record(DesktopDatabase& database, const nlohmann::json& params) {
  if (!params.is_object() || !params.contains("artifact") ||
      !params.at("artifact").is_object()) {
    return record_failure_json("invalid_input", "invalid investigation artifact", false);
  }
  nlohmann::json artifact = params.at("artifact");
  artifact["planId"] = params.contains("planId") ? params.at("planId") : nlohmann::json(nullptr);
  artifact["planStepId"] =
      params.contains("planStepId") ? params.at("planStepId") : nlohmann::json(nullptr);
  if (artifact.at("planId").is_null()) artifact.erase("planId");
  if (artifact.at("planStepId").is_null()) artifact.erase("planStepId");
  const auto task_id = json_text_field(artifact, "taskId");
  auto task = load_investigation_task(database, task_id);
  if (!task.has_value()) {
    return record_failure_json("not_found", "investigation task was not found", false);
  }
  const auto evidence_ids = artifact.value("evidenceIds", nlohmann::json::array());
  if (evidence_ids.is_array()) {
    for (const auto& evidence_id : evidence_ids) {
      if (!evidence_id.is_string()) continue;
      auto evidence = database.get_investigation_evidence(evidence_id.get<std::string>());
      if (!evidence.has_value()) {
        return record_failure_json(
            "invalid_input", "evidence `" + evidence_id.get<std::string>() + "` was not found",
            false);
      }
      if (json_text_field(*evidence, "taskId") != task_id) {
        return record_failure_json("denied_by_policy",
                                   "artifact references evidence from another task", false);
      }
    }
  }
  if (!artifact.contains("content")) {
    return record_failure_json("invalid_input", "artifact content is required", false);
  }
  if (artifact.at("content").dump().size() > 1048576) {
    return record_failure_json("invalid_input",
                               "investigation artifact content exceeds the 1 MiB limit", false);
  }
  try {
    database.upsert_investigation_artifact(artifact);
  } catch (const std::invalid_argument& error) {
    return record_failure_json("invalid_input", error.what(), false);
  }
  return {{"recorded", true}, {"artifact", artifact}};
}

// ── Investigation task creation ────────────────────────────────────────────
// Mirrors the Rust `validate_create_input` policy seam: bounded normalization,
// scope/server/workspace reconciliation and host-issued command delegation.
constexpr size_t kMaxObjectiveChars = 8192;
constexpr size_t kMaxSuccessCriteria = 16;
constexpr size_t kMaxSuccessCriterionChars = 1024;
constexpr std::int64_t kMaxBudgetSteps = 10000;
constexpr std::int64_t kMaxBudgetRunMs = 86400000;
constexpr std::int64_t kMaxBudgetAttempts = 32;
constexpr std::int64_t kDefaultBudgetSteps = 25;
constexpr std::int64_t kDefaultBudgetRunMs = 900000;
constexpr std::int64_t kDefaultBudgetAttempts = 3;
constexpr size_t kMaxGuardrailTools = 128;
constexpr size_t kMaxGuardrailPathPrefixes = 128;
constexpr size_t kMaxGuardrailNameChars = 256;
constexpr size_t kMaxGuardrailPathChars = 4096;
constexpr std::int64_t kMaxGuardrailWindowSeconds = 1095LL * 86400;
constexpr int kScheduleListLimit = 100;
constexpr std::int64_t kDefaultScheduleCooldownSeconds = 300;
constexpr std::int64_t kDefaultScheduleDedupeSeconds = 300;
constexpr std::int64_t kDefaultScheduleMaxConcurrentRuns = 1;
constexpr int kDefaultScheduleTickLimit = 16;

std::optional<std::string> normalized_opaque_id(const std::string& raw) {
  const auto value = trim_ascii(raw);
  if (value.empty() || utf8_character_count(value) > 256 || has_control_characters(value)) {
    return std::nullopt;
  }
  return value;
}

// Reconciles an optional explicit id with the id carried by the task scope.
bool reconcile_investigation_reference(const nlohmann::json& input, const char* explicit_key,
                                       const nlohmann::json& scope, const char* scoped_key,
                                       const char* label, std::optional<std::string>& out,
                                       std::string& error) {
  std::optional<std::string> explicit_value;
  if (input.contains(explicit_key)) {
    if (!input.at(explicit_key).is_string()) {
      error = std::string(label) + " id must be a string";
      return false;
    }
    explicit_value = normalized_opaque_id(input.at(explicit_key).get<std::string>());
    if (!explicit_value.has_value()) {
      error = std::string(label) + " id must be between 1 and 256 visible characters";
      return false;
    }
  }
  std::optional<std::string> scoped_value;
  if (scope.contains(scoped_key)) {
    if (!scope.at(scoped_key).is_string()) {
      error = std::string("task scope ") + scoped_key + " must be a string";
      return false;
    }
    scoped_value = normalized_opaque_id(scope.at(scoped_key).get<std::string>());
    if (!scoped_value.has_value()) {
      error = std::string(label) + " id must be between 1 and 256 visible characters";
      return false;
    }
  }
  if (explicit_value.has_value() && scoped_value.has_value() &&
      *explicit_value != *scoped_value) {
    error = std::string(label) + " id disagrees with the task scope";
    return false;
  }
  out = explicit_value.has_value() ? explicit_value : scoped_value;
  return true;
}

// Collapses a forbidden path prefix and rejects anything the Rust policy rejects.
std::optional<std::string> strict_guardrail_path_prefix(const std::string& raw) {
  const auto value = trim_ascii(raw);
  if (value.empty() || utf8_character_count(value) > kMaxGuardrailPathChars) return std::nullopt;
  if (value[0] != '/') return std::nullopt;
  if (value.find('\0') != std::string::npos || value.find('\r') != std::string::npos ||
      value.find('\n') != std::string::npos) {
    return std::nullopt;
  }
  std::vector<std::string> parts;
  std::stringstream stream(value);
  std::string part;
  while (std::getline(stream, part, '/')) {
    if (part.empty() || part == ".") continue;
    if (part == "..") return std::nullopt;
    parts.push_back(part);
  }
  if (parts.empty()) return std::string("/");
  std::string result;
  for (const auto& item : parts) {
    result += "/";
    result += item;
  }
  return result;
}

std::optional<std::string> normalized_guardrail_timestamp(const nlohmann::json& raw,
                                                          const char* field, std::string& error) {
  if (!raw.is_string()) {
    error = std::string("guardrails.") + field + " must be a string";
    return std::nullopt;
  }
  const auto value = trim_ascii(raw.get<std::string>());
  if (value.empty()) {
    error = std::string("guardrails.") + field + " cannot be empty";
    return std::nullopt;
  }
  if (!parse_iso8601_utc_seconds(value).has_value()) {
    error = std::string("guardrails.") + field + " must be a UTC ISO-8601 timestamp";
    return std::nullopt;
  }
  return value;
}

// Normalizes guardrails exactly as the Rust policy does. A renderer-supplied
// `commandGrant` is dropped here; `build_investigation_task` installs one only
// after the explicit top-level user choice and registered server checks pass.
std::optional<nlohmann::json> validate_investigation_guardrails(const nlohmann::json* raw,
                                                                std::string& error) {
  nlohmann::json guardrails = {{"forbiddenTools", nlohmann::json::array()},
                               {"forbiddenPathPrefixes", nlohmann::json::array()}};
  std::optional<std::string> not_before;
  std::optional<std::string> expires;
  if (raw != nullptr) {
    if (!raw->is_object()) {
      error = "guardrails must be an object";
      return std::nullopt;
    }
    if (!only_fields(*raw, {"notBeforeAt", "expiresAt", "forbiddenTools",
                            "forbiddenPathPrefixes", "commandGrant"},
                     {})) {
      error = "guardrails contains an unexpected field";
      return std::nullopt;
    }
    if (raw->contains("notBeforeAt")) {
      auto value = normalized_guardrail_timestamp(raw->at("notBeforeAt"), "notBeforeAt", error);
      if (!value.has_value()) return std::nullopt;
      not_before = *value;
    }
    if (raw->contains("expiresAt")) {
      auto value = normalized_guardrail_timestamp(raw->at("expiresAt"), "expiresAt", error);
      if (!value.has_value()) return std::nullopt;
      expires = *value;
    }
    if (raw->contains("forbiddenTools")) {
      const auto& tools = raw->at("forbiddenTools");
      if (!tools.is_array() || tools.size() > kMaxGuardrailTools) {
        error = "guardrails.forbiddenTools must contain at most 128 items";
        return std::nullopt;
      }
      nlohmann::json normalized = nlohmann::json::array();
      std::vector<std::string> seen;
      for (const auto& entry : tools) {
        if (!entry.is_string()) {
          error = "each guardrails.forbiddenTools item must be a tool name string";
          return std::nullopt;
        }
        const auto tool = trim_ascii(entry.get<std::string>());
        if (tool.empty() || utf8_character_count(tool) > kMaxGuardrailNameChars ||
            std::any_of(tool.begin(), tool.end(),
                        [](unsigned char byte) { return std::isspace(byte) != 0; })) {
          error =
              "each guardrails.forbiddenTools item must be a non-empty internal tool name of at "
              "most 256 characters";
          return std::nullopt;
        }
        if (std::find(seen.begin(), seen.end(), tool) == seen.end()) {
          seen.push_back(tool);
          normalized.push_back(tool);
        }
      }
      guardrails["forbiddenTools"] = std::move(normalized);
    }
    if (raw->contains("forbiddenPathPrefixes")) {
      const auto& prefixes = raw->at("forbiddenPathPrefixes");
      if (!prefixes.is_array() || prefixes.size() > kMaxGuardrailPathPrefixes) {
        error = "guardrails.forbiddenPathPrefixes must contain at most 128 items";
        return std::nullopt;
      }
      nlohmann::json normalized = nlohmann::json::array();
      std::vector<std::string> seen;
      for (const auto& entry : prefixes) {
        if (!entry.is_string()) {
          error = "each guardrails.forbiddenPathPrefixes item must be a string";
          return std::nullopt;
        }
        auto prefix = strict_guardrail_path_prefix(entry.get<std::string>());
        if (!prefix.has_value()) {
          error =
              "each guardrails.forbiddenPathPrefixes item must be an absolute path of at most "
              "4096 characters without `..`";
          return std::nullopt;
        }
        if (std::find(seen.begin(), seen.end(), *prefix) == seen.end()) {
          seen.push_back(*prefix);
          normalized.push_back(*prefix);
        }
      }
      guardrails["forbiddenPathPrefixes"] = std::move(normalized);
    }
  }
  if (not_before.has_value()) guardrails["notBeforeAt"] = *not_before;
  if (expires.has_value()) guardrails["expiresAt"] = *expires;
  if (not_before.has_value() && expires.has_value()) {
    const auto start = parse_iso8601_utc_seconds(*not_before);
    const auto end = parse_iso8601_utc_seconds(*expires);
    if (!start.has_value() || !end.has_value()) {
      error = "guardrails timestamps must be UTC ISO-8601 timestamps";
      return std::nullopt;
    }
    if (*end <= *start) {
      error = "guardrails.expiresAt must be later than guardrails.notBeforeAt";
      return std::nullopt;
    }
    if (*end - *start > kMaxGuardrailWindowSeconds) {
      error = "guardrails time window cannot exceed 1095 days";
      return std::nullopt;
    }
  }
  return guardrails;
}

// Host-issued command delegation ticket. The counters are persisted with the
// task and enforced by the SSH capability once remote execution lands.
std::optional<nlohmann::json> issue_command_grant(
    const std::string& task_id, const std::string& server_id, const std::string& environment,
    const std::optional<std::string>& task_expires, std::string& error) {
  const auto granted_at = iso8601_now();
  const auto granted_epoch = parse_iso8601_utc_seconds(granted_at);
  if (!granted_epoch.has_value()) {
    error = "host clock could not issue a valid command delegation timestamp";
    return std::nullopt;
  }
  std::int64_t expires_epoch = *granted_epoch + 4 * 3600;
  if (task_expires.has_value()) {
    const auto parsed = parse_iso8601_utc_seconds(*task_expires);
    if (!parsed.has_value()) {
      error = "guardrails.expiresAt must be a valid UTC timestamp";
      return std::nullopt;
    }
    if (*parsed < expires_epoch) expires_epoch = *parsed;
  }
  if (expires_epoch <= *granted_epoch) {
    error = "task command delegation would already be expired";
    return std::nullopt;
  }
  return nlohmann::json{
      {"grantId", next_id("cmdgrant")},
      {"taskId", task_id},
      {"serverId", server_id},
      {"environment", environment},
      {"grantedBy", "user"},
      {"grantedAt", granted_at},
      {"expiresAt", iso8601_from_epoch(expires_epoch)},
      {"maxCalls", 12},
      {"callsUsed", 0},
      {"maxTotalDurationMs", 900000},
      {"totalDurationMs", 0},
      {"maxTotalOutputBytes", 1048576},
      {"totalOutputBytes", 0},
  };
}

// Builds a validated `InvestigationTask` row from a create input. Returns
// `std::nullopt` and fills `error` when the input is rejected.
// Mirrors the Rust `validate_scheduler_task`: a durable trigger may only target a
// live, read-only investigation task.
std::optional<std::string> scheduler_task_error(const nlohmann::json& task) {
  if (json_text_field(task, "mode") != "readonly" ||
      json_text_field(task, "automationLevel") != "readonly") {
    return "scheduler triggers can only target read-only tasks";
  }
  if (is_one_of(json_text_field(task, "status"),
                {"completed", "failed", "stopped", "expired"})) {
    return "terminal investigation tasks cannot be scheduled";
  }
  return std::nullopt;
}

std::optional<std::string> schedule_budget_error(const nlohmann::json& budget) {
  if (!budget.is_object() ||
      !only_fields(budget, {"maxSteps", "maxRunMs", "maxAttempts"},
                   {"maxSteps", "maxRunMs", "maxAttempts"}) ||
      !budget.at("maxSteps").is_number_integer() || !budget.at("maxRunMs").is_number_integer() ||
      !budget.at("maxAttempts").is_number_integer()) {
    return "schedule budget is invalid";
  }
  const auto steps = budget.at("maxSteps").get<std::int64_t>();
  const auto run_ms = budget.at("maxRunMs").get<std::int64_t>();
  const auto attempts = budget.at("maxAttempts").get<std::int64_t>();
  if (steps < 1 || steps > kMaxBudgetSteps || run_ms < 1 || run_ms > kMaxBudgetRunMs ||
      attempts < 1 || attempts > kMaxBudgetAttempts) {
    return "schedule budget is outside the supported bounds";
  }
  return std::nullopt;
}

// Mirrors the Rust `validate_schedule_input`: the last host-owned bounds check
// before a schedule is persisted.
std::optional<std::string> schedule_input_error(const nlohmann::json& schedule) {
  const auto interval = schedule.value("intervalSeconds", std::int64_t{0});
  if (interval < 1 || interval > 86400) return "intervalSeconds must be between 1 and 86400";
  const auto cooldown = schedule.value("cooldownSeconds", std::int64_t{0});
  if (cooldown > 86400) return "cooldownSeconds must be at most 86400";
  const auto dedupe = schedule.value("dedupeWindowSeconds", std::int64_t{0});
  if (dedupe < 1 || dedupe > 86400) return "dedupeWindowSeconds must be between 1 and 86400";
  const auto concurrent = schedule.value("maxConcurrentRuns", std::int64_t{0});
  if (concurrent < 1 || concurrent > 16) return "maxConcurrentRuns must be between 1 and 16";
  if (!schedule.contains("budget")) return "schedule budget is required";
  if (const auto budget_error = schedule_budget_error(schedule.at("budget"));
      budget_error.has_value()) {
    return budget_error;
  }
  if (!schedule.contains("nextRunAt") || !schedule.at("nextRunAt").is_string() ||
      !parse_iso8601_utc_seconds(schedule.at("nextRunAt").get<std::string>()).has_value()) {
    return "nextRunAt must be a UTC timestamp";
  }
  return std::nullopt;
}

std::optional<nlohmann::json> build_investigation_task(const nlohmann::json& input,
                                                       DesktopDatabase& database,
                                                       std::string& error) {
  if (!input.is_object() ||
      !only_fields(input,
                   {"id", "workspaceId", "serverId", "objective", "successCriteria", "scope",
                    "guardrails", "mode", "permissionMode", "automationLevel", "budget",
                    "grantTaskCommands"},
                   {"objective", "successCriteria", "scope", "mode", "permissionMode",
                    "automationLevel"})) {
    error = "investigation task input is missing required fields or has unexpected fields";
    return std::nullopt;
  }
  if (!input.at("objective").is_string()) {
    error = "objective must be a string";
    return std::nullopt;
  }
  const auto objective = trim_ascii(input.at("objective").get<std::string>());
  if (objective.empty() || utf8_character_count(objective) > kMaxObjectiveChars) {
    error = "objective must be between 1 and 8192 characters";
    return std::nullopt;
  }
  const auto& raw_criteria = input.at("successCriteria");
  if (!raw_criteria.is_array() || raw_criteria.empty() ||
      raw_criteria.size() > kMaxSuccessCriteria) {
    error = "successCriteria must contain 1 to 16 items";
    return std::nullopt;
  }
  nlohmann::json success_criteria = nlohmann::json::array();
  for (const auto& criterion : raw_criteria) {
    if (!criterion.is_string()) {
      error = "each success criterion must be a string";
      return std::nullopt;
    }
    const auto normalized = trim_ascii(criterion.get<std::string>());
    if (normalized.empty() || utf8_character_count(normalized) > kMaxSuccessCriterionChars) {
      error = "each success criterion must be between 1 and 1024 characters";
      return std::nullopt;
    }
    success_criteria.push_back(normalized);
  }
  const auto& scope = input.at("scope");
  if (!scope.is_object() ||
      !only_fields(scope, {"host", "serverId", "workspaceId", "environment"},
                   {"host", "environment"}) ||
      !scope.at("host").is_string() || !scope.at("environment").is_string() ||
      !is_one_of(scope.at("host").get<std::string>(), {"local", "remote"}) ||
      !is_one_of(scope.at("environment").get<std::string>(),
                 {"local", "development", "staging", "production", "unknown"})) {
    error = "task scope is invalid";
    return std::nullopt;
  }
  std::optional<std::string> task_id;
  if (input.contains("id")) {
    if (!input.at("id").is_string()) {
      error = "task id must be a string";
      return std::nullopt;
    }
    task_id = normalized_opaque_id(input.at("id").get<std::string>());
    if (!task_id.has_value()) {
      error = "task id must be between 1 and 256 visible characters";
      return std::nullopt;
    }
  } else {
    task_id = next_id("task");
  }
  std::optional<std::string> workspace_id;
  if (!reconcile_investigation_reference(input, "workspaceId", scope, "workspaceId", "workspace",
                                         workspace_id, error)) {
    return std::nullopt;
  }
  std::optional<std::string> server_id;
  if (!reconcile_investigation_reference(input, "serverId", scope, "serverId", "server", server_id,
                                         error)) {
    return std::nullopt;
  }
  const auto host = scope.at("host").get<std::string>();
  const auto environment = scope.at("environment").get<std::string>();
  if (host == "remote" && !server_id.has_value()) {
    error = "remote investigation tasks require a server scope";
    return std::nullopt;
  }
  if (host == "local" && server_id.has_value()) {
    error = "local investigation tasks cannot carry a server scope";
    return std::nullopt;
  }
  std::optional<nlohmann::json> server_record;
  if (server_id.has_value()) {
    if (!valid_server_id(*server_id)) {
      error = "server scope must be an opaque srv_ id";
      return std::nullopt;
    }
    nlohmann::json servers;
    try {
      servers = database.list_servers();
    } catch (...) {
      error = "server scope is not available";
      return std::nullopt;
    }
    const auto list = servers.value("servers", nlohmann::json::array());
    if (list.is_array()) {
      for (const auto& candidate : list) {
        if (json_text_field(candidate, "id") == *server_id) {
          server_record = candidate;
          break;
        }
      }
    }
    if (!server_record.has_value()) {
      error = "server scope is not available";
      return std::nullopt;
    }
  }
  if (workspace_id.has_value()) {
    nlohmann::json workspaces;
    try {
      workspaces = database.list_workspaces();
    } catch (...) {
      error = "workspace scope is not available";
      return std::nullopt;
    }
    bool found = false;
    const auto list = workspaces.value("workspaces", nlohmann::json::array());
    if (list.is_array()) {
      for (const auto& candidate : list) {
        if (json_text_field(candidate, "id") == *workspace_id) {
          found = true;
          break;
        }
      }
    }
    if (!found) {
      error = "workspace scope is not available";
      return std::nullopt;
    }
  }
  if (!input.at("mode").is_string() || !input.at("permissionMode").is_string() ||
      !input.at("automationLevel").is_string()) {
    error = "task mode, permission mode and automation level must be strings";
    return std::nullopt;
  }
  const auto mode = input.at("mode").get<std::string>();
  const auto permission_mode = input.at("permissionMode").get<std::string>();
  const auto automation = input.at("automationLevel").get<std::string>();
  if (!is_one_of(mode, {"goal", "plan", "readonly"})) {
    error = "task mode is invalid";
    return std::nullopt;
  }
  if (!is_one_of(permission_mode, {"ask", "auto"})) {
    error = "task permission mode is invalid";
    return std::nullopt;
  }
  if (!is_one_of(automation, {"readonly", "propose", "execute"})) {
    error = "task automation level is invalid";
    return std::nullopt;
  }
  const nlohmann::json* raw_guardrails =
      input.contains("guardrails") && !input.at("guardrails").is_null() ? &input.at("guardrails")
                                                                       : nullptr;
  auto guardrails = validate_investigation_guardrails(raw_guardrails, error);
  if (!guardrails.has_value()) return std::nullopt;
  if (mode == "readonly" && automation != "readonly") {
    error = "readonly run mode cannot create an executable task";
    return std::nullopt;
  }
  if (mode == "plan" && automation == "execute") {
    error = "plan mode can propose a change but cannot authorize execution";
    return std::nullopt;
  }
  if (json_bool_field(input, "grantTaskCommands")) {
    const bool grantable = mode == "goal" && permission_mode == "auto" && automation == "execute" &&
                           host == "remote" &&
                           (environment == "development" || environment == "staging");
    if (!grantable) {
      error =
          "task command delegation is available only for remote development/staging goal tasks "
          "configured to execute";
      return std::nullopt;
    }
    if (!server_id.has_value()) {
      error = "task command delegation requires a resolved server";
      return std::nullopt;
    }
    const auto server_environment =
        server_record->value("metadata", nlohmann::json::object()).value("environment",
                                                                          std::string());
    if (server_environment != environment) {
      error = "task command delegation environment does not match the registered server";
      return std::nullopt;
    }
    const auto task_expires = guardrails->contains("expiresAt") &&
                                      guardrails->at("expiresAt").is_string()
                                  ? std::optional<std::string>(
                                        guardrails->at("expiresAt").get<std::string>())
                                  : std::nullopt;
    std::string grant_error;
    auto grant = issue_command_grant(*task_id, *server_id, environment, task_expires, grant_error);
    if (!grant.has_value()) {
      error = grant_error;
      return std::nullopt;
    }
    (*guardrails)["commandGrant"] = std::move(*grant);
  }
  std::int64_t max_steps = kDefaultBudgetSteps;
  std::int64_t max_run_ms = kDefaultBudgetRunMs;
  std::int64_t max_attempts = kDefaultBudgetAttempts;
  if (input.contains("budget") && !input.at("budget").is_null()) {
    const auto& budget = input.at("budget");
    if (!budget.is_object() ||
        !only_fields(budget, {"maxSteps", "maxRunMs", "maxAttempts"},
                     {"maxSteps", "maxRunMs", "maxAttempts"}) ||
        !budget.at("maxSteps").is_number_integer() ||
        !budget.at("maxRunMs").is_number_integer() ||
        !budget.at("maxAttempts").is_number_integer()) {
      error = "budget is invalid";
      return std::nullopt;
    }
    max_steps = budget.at("maxSteps").get<std::int64_t>();
    max_run_ms = budget.at("maxRunMs").get<std::int64_t>();
    max_attempts = budget.at("maxAttempts").get<std::int64_t>();
  }
  if (max_steps < 1 || max_steps > kMaxBudgetSteps) {
    error = "budget.maxSteps must be between 1 and 10000";
    return std::nullopt;
  }
  if (max_run_ms < 1 || max_run_ms > kMaxBudgetRunMs) {
    error = "budget.maxRunMs must be between 1 and 86400000";
    return std::nullopt;
  }
  if (max_attempts < 1 || max_attempts > kMaxBudgetAttempts) {
    error = "budget.maxAttempts must be between 1 and 32";
    return std::nullopt;
  }
  nlohmann::json scope_out = {{"host", host}, {"environment", environment}};
  if (server_id.has_value()) scope_out["serverId"] = *server_id;
  if (workspace_id.has_value()) scope_out["workspaceId"] = *workspace_id;
  const auto now = iso8601_now();
  nlohmann::json task = {
      {"id", *task_id},
      {"objective", objective},
      {"successCriteria", std::move(success_criteria)},
      {"scope", std::move(scope_out)},
      {"guardrails", std::move(*guardrails)},
      {"mode", mode},
      {"permissionMode", permission_mode},
      {"automationLevel", automation},
      {"createdBy", "user"},
      {"phase", "investigating"},
      {"status", "pending"},
      {"budget",
       {{"maxSteps", max_steps}, {"maxRunMs", max_run_ms}, {"maxAttempts", max_attempts}}},
      {"createdAt", now},
      {"updatedAt", now},
  };
  if (workspace_id.has_value()) task["workspaceId"] = *workspace_id;
  if (server_id.has_value()) task["serverId"] = *server_id;
  return task;
}

// ── Investigation task start / recover ─────────────────────────────────────
// Mirrors `commands/investigation/policy.rs`, `commands/agent_run.rs` and
// `commands/investigation.rs`: the durable task owns its Agent prompt and
// safety envelope, and the host alone binds a run id to it.
constexpr size_t kMaxPromptTextChars = 100000;
constexpr size_t kMaxFailureMessageChars = 4096;

std::int64_t current_epoch_seconds() {
  return static_cast<std::int64_t>(std::chrono::duration_cast<std::chrono::seconds>(
                                       std::chrono::system_clock::now().time_since_epoch())
                                       .count());
}

// The single host-owned retryability rule; failure options, the recovery
// predicate and the UI retry affordance must all agree with it.
bool task_failure_retryable(const std::string& code) {
  return code == "timeout" || code == "transport" || code == "authentication";
}

nlohmann::json failure_option_json(const char* id, const char* action, const char* title,
                                   const char* description, bool requires_approval) {
  return {{"id", id},
          {"action", action},
          {"title", title},
          {"description", description},
          {"requiresApproval", requires_approval}};
}

// Mirrors the Rust `failure_options` taxonomy so the UI offers the same choices.
nlohmann::json investigation_failure_options(const std::string& code) {
  nlohmann::json options = nlohmann::json::array();
  if (task_failure_retryable(code)) {
    options.push_back(failure_option_json(
        "retry", "retry", "重试当前阶段",
        "重新校验目标后再尝试一次；不会自动重放不可安全重试的写入。", false));
  }
  if (code == "plan_deviation" || code == "evidence_missing") {
    options.push_back(failure_option_json("replan", "replan", "要求 Agent 重新规划",
                                          "保留现有证据与失败现场，要求生成新的计划修订。",
                                          false));
  } else if (code == "approval_required" || code == "approval_rejected" ||
             code == "permission_denied") {
    options.push_back(failure_option_json("wait_user", "wait_user", "等待我重新决定",
                                          "不自动重试；由用户补充批准、缩小范围或终止任务。",
                                          true));
  } else if (code == "cancelled") {
    options.push_back(failure_option_json("resume", "resume", "从检查点恢复",
                                          "重新检查连接、计划和目标后从未完成阶段继续。", false));
  } else if (code == "outcome_unknown") {
    options.push_back(failure_option_json(
        "wait_user", "wait_user", "先核对远端状态",
        "命令或变更可能已经生效。先查看执行摘要并重新采集只读证据；确认现场后再手动继续任务。",
        false));
  } else if (code == "budget_exhausted") {
    options.push_back(failure_option_json("replan", "replan", "缩小范围后重新规划",
                                          "预算已耗尽，必须由用户确认新的范围或预算。", true));
  }
  options.push_back(failure_option_json("inspect", "inspect", "查看现场证据",
                                        "保持任务停止，先查看已保存的证据、执行结果和失败工件。",
                                        false));
  options.push_back(failure_option_json("stop", "stop", "结束任务",
                                        "保留审计与现场证据，不再继续自动化。", false));
  return options;
}

std::string truncate_utf8(const std::string& value, size_t max_characters) {
  if (utf8_character_count(value) <= max_characters) return value;
  size_t bytes = 0;
  size_t count = 0;
  while (bytes < value.size() && count < max_characters) {
    const unsigned char lead = static_cast<unsigned char>(value[bytes]);
    size_t width = 1;
    if ((lead & 0x80) == 0) width = 1;
    else if ((lead & 0xE0) == 0xC0) width = 2;
    else if ((lead & 0xF0) == 0xE0) width = 3;
    else if ((lead & 0xF8) == 0xF0) width = 4;
    if (bytes + width > value.size()) break;
    bytes += width;
    ++count;
  }
  return value.substr(0, bytes);
}

nlohmann::json build_task_failure(const std::string& code, const std::string& message,
                                  std::int64_t attempt, const std::string& at,
                                  const std::optional<nlohmann::json>& detail) {
  nlohmann::json failure = {{"code", code},
                            {"message", truncate_utf8(message, kMaxFailureMessageChars)},
                            {"retryable", task_failure_retryable(code)},
                            {"attempt", attempt},
                            {"at", at},
                            {"options", investigation_failure_options(code)}};
  if (detail.has_value() && !detail->is_null()) failure["detail"] = *detail;
  return failure;
}

std::optional<std::string> task_time_window_error(const nlohmann::json& task) {
  if (!task.contains("guardrails") || !task.at("guardrails").is_object()) return std::nullopt;
  const auto& guardrails = task.at("guardrails");
  const auto now = current_epoch_seconds();
  if (guardrails.contains("notBeforeAt") && guardrails.at("notBeforeAt").is_string()) {
    const auto raw = guardrails.at("notBeforeAt").get<std::string>();
    const auto parsed = parse_iso8601_utc_seconds(raw);
    if (!parsed.has_value()) {
      return std::string(
          "task guardrail notBeforeAt is invalid; task is blocked until it is repaired");
    }
    if (now < *parsed) return "task execution is not allowed before " + raw;
  }
  if (guardrails.contains("expiresAt") && guardrails.at("expiresAt").is_string()) {
    const auto raw = guardrails.at("expiresAt").get<std::string>();
    const auto parsed = parse_iso8601_utc_seconds(raw);
    if (!parsed.has_value()) {
      return std::string(
          "task guardrail expiresAt is invalid; task is blocked until it is repaired");
    }
    if (now >= *parsed) return "task execution time window expired at " + raw;
  }
  return std::nullopt;
}

bool task_failure_detail_flag(const nlohmann::json& task, const char* key) {
  if (!task.contains("lastFailure") || !task.at("lastFailure").is_object()) return false;
  const auto& failure = task.at("lastFailure");
  if (!failure.contains("detail") || !failure.at("detail").is_object()) return false;
  const auto found = failure.at("detail").find(key);
  return found != failure.at("detail").end() && found->is_boolean() && found->get<bool>();
}

std::string format_guardrails_for_prompt(const nlohmann::json& guardrails) {
  const auto join = [](const nlohmann::json& array) {
    std::string out;
    if (!array.is_array()) return out;
    for (const auto& item : array) {
      if (!item.is_string()) continue;
      if (!out.empty()) out += ", ";
      out += item.get<std::string>();
    }
    return out;
  };
  std::vector<std::string> lines;
  if (guardrails.contains("notBeforeAt") && guardrails.at("notBeforeAt").is_string()) {
    lines.push_back("不得早于 " + guardrails.at("notBeforeAt").get<std::string>());
  }
  if (guardrails.contains("expiresAt") && guardrails.at("expiresAt").is_string()) {
    lines.push_back("不得晚于 " + guardrails.at("expiresAt").get<std::string>());
  }
  if (guardrails.contains("forbiddenTools") && guardrails.at("forbiddenTools").is_array() &&
      !guardrails.at("forbiddenTools").empty()) {
    lines.push_back("禁止工具：" + join(guardrails.at("forbiddenTools")));
  }
  if (guardrails.contains("forbiddenPathPrefixes") &&
      guardrails.at("forbiddenPathPrefixes").is_array() &&
      !guardrails.at("forbiddenPathPrefixes").empty()) {
    lines.push_back("禁止路径前缀：" + join(guardrails.at("forbiddenPathPrefixes")));
  }
  if (lines.empty()) return std::string();
  std::string joined;
  for (const auto& line : lines) {
    if (!joined.empty()) joined += "；";
    joined += line;
  }
  return "\n\n宿主硬边界（不能通过提示词或重试绕过）：" + joined + "。";
}

std::string autonomous_task_prompt(const nlohmann::json& task) {
  std::string criteria;
  const auto criteria_list = task.value("successCriteria", nlohmann::json::array());
  if (criteria_list.is_array()) {
    int index = 1;
    for (const auto& criterion : criteria_list) {
      if (criterion.is_string()) {
        if (!criteria.empty()) criteria += "\n";
        criteria += std::to_string(index) + ". " + criterion.get<std::string>();
      }
      ++index;
    }
  }
  const auto mode = json_text_field(task, "mode");
  const auto permission = json_text_field(task, "permissionMode");
  const auto automation = json_text_field(task, "automationLevel");
  const auto scope = task.value("scope", nlohmann::json::object());
  const auto scope_host = json_text_field(scope, "host");
  const auto environment = json_text_field(scope, "environment");
  std::string mode_instruction;
  if (mode == "readonly") {
    mode_instruction = "只调用任务范围内已允许的只读工具；不要写入、重启、删除或改变目标状态。";
  } else if (mode == "plan") {
    mode_instruction =
        "先调查并生成宿主可验证的 dry-run 计划；没有用户选择和审批时不要执行行动步骤。";
  } else if (mode == "goal" && permission == "auto" && automation == "execute" &&
             scope_host == "remote" && (environment == "development" || environment == "staging")) {
    mode_instruction =
        "按宿主保存的计划逐步推进；本任务已明确启用受限 auto 委托，medium 风险的配置备份、编辑与整文件写入（可先备份再写）可在宿主复核后自动推进；备份清理、恢复、重启、包安装及其他高风险或未明确授权的动作必须停下等待用户审批。";
  } else {
    mode_instruction = "按宿主保存的计划逐步推进；高风险或未明确授权的动作必须停下等待用户审批。";
  }
  std::string command_grant_instruction;
  if (task.contains("guardrails") && task.at("guardrails").is_object() &&
      task.at("guardrails").contains("commandGrant") &&
      task.at("guardrails").at("commandGrant").is_object()) {
    const auto& grant = task.at("guardrails").at("commandGrant");
    const auto calls_used = grant.value("callsUsed", std::int64_t{0});
    const auto max_calls = grant.value("maxCalls", std::int64_t{0});
    const auto remaining_ms =
        std::max<std::int64_t>(0, grant.value("maxTotalDurationMs", std::int64_t{0}) -
                                      grant.value("totalDurationMs", std::int64_t{0}));
    const auto remaining_bytes =
        std::max<std::int64_t>(0, grant.value("maxTotalOutputBytes", std::int64_t{0}) -
                                      grant.value("totalOutputBytes", std::int64_t{0}));
    command_grant_instruction =
        "\n\n用户已为本任务授权 server.exec 在这台开发/预发布服务器执行非关键危险命令；命令预算为已用 " +
        std::to_string(calls_used) + "/" + std::to_string(max_calls) + " 次、剩余总时长 " +
        std::to_string(remaining_ms) + "ms、剩余输出 " + std::to_string(remaining_bytes) +
        " 字节，授权截止 " + json_text_field(grant, "expiresAt") +
        "。仍须先建立 ChangePlan 并按步骤调用工具。关键危险命令仍需逐次批准。";
  }
  std::string recovery_instruction;
  const auto failure_code = task.contains("lastFailure") && task.at("lastFailure").is_object()
                                ? json_text_field(task.at("lastFailure"), "code")
                                : std::string();
  if (failure_code == "outcome_unknown") {
    recovery_instruction =
        "\n\n上一轮远端命令或变更的结果未知，可能已经生效。旧命令授权已撤销。先只调用只读工具重新采样目标状态，结合实际执行记录判断变更是否发生；证据明确前禁止重放、补发或改写同一行动。需要继续时，先生成新的计划并让用户逐项批准高风险命令。";
  } else if (task_failure_detail_flag(task, "rollbackRequested")) {
    recovery_instruction =
        "\n\n用户选择了“先规划回退”。这不是对旧回退文本或旧工具调用的授权：请只生成一份新的、独立的回退 dry-run 计划，重新校验当前基线、目标范围和精确参数绑定；涉及写入时仍须取得本轮审批，禁止直接重放旧动作。";
  } else if (task_failure_detail_flag(task, "requiresFreshBaseline")) {
    recovery_instruction =
        "\n\n这是一次中断后的恢复运行：旧计划审批、旧变更前基线和正在进行的观察窗口已经失效。请先重新校验目标身份、收集新的只读证据并生成/确认新的计划；在新基线和本轮审批完成前不要重放旧行动。";
  }
  const auto guardrails = task.value("guardrails", nlohmann::json::object());
  const auto guardrail_instruction =
      guardrails.is_object() ? format_guardrails_for_prompt(guardrails) : std::string();
  return "你正在自主推进一项由用户创建的可恢复运维任务。\n\n目标：\n" +
         json_text_field(task, "objective") + "\n\n完成标准：\n" + criteria + "\n\n边界：\n" +
         mode_instruction + command_grant_instruction + recovery_instruction +
         guardrail_instruction +
         "\n\n先调用 investigation.plan 声明有序步骤；如果目标属于只读健康检查、受保护配置编辑、容器重启、systemd 服务重启、明确的 apt/dnf 包安装或受限 deploy_sequence，可以调用 investigation.playbook 生成宿主拥有的 dry-run 计划。playbook 只记录计划，不直接执行写入。之后按计划收集证据、记录 Finding 和决策摘要。每个事实都要引用证据；证据不足时明确停在等待用户，而不是猜测已经完成。";
}

// The task's current attempt number is one past the highest persisted attempt.
std::int64_t next_investigation_attempt(DesktopDatabase& database, const std::string& task_id) {
  const auto runs = database.list_investigation_runs(task_id, 64);
  std::int64_t highest = 0;
  if (runs.is_array()) {
    for (const auto& run : runs) {
      const auto attempt = run.value("attempt", std::int64_t{0});
      if (attempt > highest) highest = attempt;
    }
  }
  return highest + 1;
}

// Marks a persisted run failed when `agent.run.start` cannot be honoured.
void fail_admitted_investigation_run(DesktopDatabase& database, const std::string& task_id,
                                     const std::string& run_id, const std::string& message,
                                     const char* code) {
  try {
    auto run = database.get_investigation_run(run_id);
    if (!run.has_value() || json_text_field(*run, "taskId") != task_id) return;
    const auto now = iso8601_now();
    const auto attempt = run->value("attempt", std::int64_t{1});
    (*run)["status"] = "failed";
    (*run)["updatedAt"] = now;
    (*run)["endedAt"] = now;
    (*run)["failure"] = build_task_failure(code, message, attempt, now, std::nullopt);
    (void)database.update_investigation_run(*run);
    (void)database.update_investigation_task_progress(
        task_id, "failed", "recovery", std::nullopt, run->at("failure"), now, now);
  } catch (...) {
  }
}

// Interrupts a stale active run before a new attempt replaces it.
bool interrupt_stale_investigation_run(DesktopDatabase& database, const std::string& task_id,
                                       const std::string& run_id, std::string& error) {
  std::optional<nlohmann::json> run;
  try {
    run = database.get_investigation_run(run_id);
  } catch (...) {
    error = "failed to read active investigation run";
    return false;
  }
  if (!run.has_value()) return true;
  if (json_text_field(*run, "taskId") != task_id) {
    error = "active investigation run belongs to another task";
    return false;
  }
  const auto status = json_text_field(*run, "status");
  if (!is_one_of(status, {"admitted", "running", "waiting_user"})) return true;
  const auto now = iso8601_now();
  const auto attempt = run->value("attempt", std::int64_t{1});
  (*run)["status"] = "interrupted";
  (*run)["updatedAt"] = now;
  (*run)["endedAt"] = now;
  (*run)["failure"] = build_task_failure("transport",
                                         "上一轮 sidecar 运行未完成，本次恢复前已标记为中断",
                                         attempt, now, std::nullopt);
  try {
    (void)database.update_investigation_run(*run);
  } catch (...) {
    error = "failed to interrupt stale investigation run";
    return false;
  }
  return true;
}

// Recovery cannot trust an old approval or a baseline captured before a
// disconnected run. Keep the declarative plan visible for the next Agent turn,
// but force it to re-establish the target state and obtain a fresh decision.
void invalidate_plan_after_recovery(nlohmann::json& plan, nlohmann::json& artifacts,
                                    const std::string& now) {
  plan["approval"] = {{"status", "pending"},
                      {"note", "恢复后需要重新核对目标、基线和用户选择"}};
  if (plan.contains("observationWindow") && plan.at("observationWindow").is_object()) {
    auto& window = plan["observationWindow"];
    if (json_text_field(window, "status") == "running") {
      window["status"] = "cancelled";
      window["lastFailure"] = "运行中断，观察窗口必须在重新规划后重启";
    }
  }
  plan["updatedAt"] = now;
  const auto plan_id = json_text_field(plan, "id");
  if (!artifacts.is_array()) return;
  for (auto& artifact : artifacts) {
    if (!artifact.is_object()) continue;
    if (json_text_field(artifact, "kind") != "baseline") continue;
    const auto status = json_text_field(artifact, "status");
    if (status != "ready" && status != "succeeded") continue;
    if (json_text_field(artifact, "planId") != plan_id) continue;
    artifact["status"] = "superseded";
    artifact["summary"] = "恢复后旧基线失效，必须重新采集并核对目标状态";
    artifact["updatedAt"] = now;
  }
}

// The shared transfer handle-id contract: a trimmed 16..128 character opaque id.
bool valid_transfer_handle_id(const std::string& value) {
  const auto trimmed = trim_ascii(value);
  return trimmed.size() >= 16 && trimmed.size() <= 128;
}

// The shared remote transfer path contract: an absolute, bounded POSIX path.
bool valid_transfer_remote_path(const std::string& value) {
  return !value.empty() && value.size() <= 4096 && value.front() == '/';
}

nlohmann::json transfer_snapshot_array(const std::vector<TransferSnapshot>& snapshots) {
  nlohmann::json transfers = nlohmann::json::array();
  for (const auto& snapshot : snapshots) {
    transfers.push_back(transfer_snapshot_json(snapshot));
  }
  return transfers;
}

IpcReply transfer_snapshot_reply(TransferManager& manager, const std::string& transfer_id) {
  const auto snapshot = manager.get(transfer_id);
  if (!snapshot.has_value()) {
    return failure("NOT_FOUND", "The file transfer was not found");
  }
  return {true, {{"transfer", transfer_snapshot_json(*snapshot)}}};
}

IpcReply transfer_error_reply(const TransferError& error) {
  switch (error.code()) {
    case TransferErrorCode::InvalidInput:
      return failure("INVALID_PARAMS", error.what());
    case TransferErrorCode::Local:
      return failure("STORAGE_ERROR", error.what());
    case TransferErrorCode::Remote:
    case TransferErrorCode::AlreadyExists:
    case TransferErrorCode::Unsupported:
    case TransferErrorCode::SourceChanged:
    case TransferErrorCode::Verification:
    case TransferErrorCode::Cancelled:
      return failure("TRANSFER_FAILED", error.what());
  }
  return failure("TRANSFER_FAILED", error.what());
}

// True when every component of `root` is also a leading component of `candidate`.
// Used with canonical paths, so a staged file cannot escape its private stage.
bool path_is_within(const std::filesystem::path& candidate,
                    const std::filesystem::path& root) {
  auto candidate_it = candidate.begin();
  for (auto root_it = root.begin(); root_it != root.end(); ++root_it, ++candidate_it) {
    if (candidate_it == candidate.end() || *candidate_it != *root_it) return false;
  }
  return true;
}

// The single regular file a completed drag download must have produced. A
// directory or symbolic link in the stage is a contract violation, matching the
// retired Rust `collect_staged_regular_files`.
std::vector<std::filesystem::path> collect_staged_regular_files(
    const std::filesystem::path& directory) {
  std::vector<std::filesystem::path> files;
  std::error_code error;
  std::filesystem::directory_iterator iterator(
      directory, std::filesystem::directory_options::skip_permission_denied, error);
  if (error) throw std::runtime_error(error.message());
  for (const auto& entry : iterator) {
    std::error_code status_error;
    const auto status = entry.symlink_status(status_error);
    if (status_error) throw std::runtime_error(status_error.message());
    if (std::filesystem::is_symlink(status) || std::filesystem::is_directory(status)) {
      throw std::runtime_error("staged download contains a directory or symbolic link");
    }
    if (std::filesystem::is_regular_file(status)) files.push_back(entry.path());
  }
  return files;
}

// Waits until `transfer_id` reaches a terminal status or `timeout` elapses. A
// missed event is recovered by polling the manager's latest snapshot once, so a
// transfer that completed before the subscription was established is still seen.
std::optional<TransferSnapshot> wait_for_transfer_terminal(
    TransferManager& manager, const std::string& transfer_id,
    std::chrono::milliseconds timeout) {
  struct State {
    std::mutex mutex;
    std::condition_variable condition;
    std::optional<TransferSnapshot> terminal;
  };
  auto state = std::make_shared<State>();
  const auto token = manager.subscribe(
      [state, transfer_id](const TransferSnapshot& snapshot) {
        if (snapshot.transfer_id != transfer_id || !is_terminal(snapshot.status)) return;
        {
          std::lock_guard lock(state->mutex);
          if (state->terminal.has_value()) return;
          state->terminal = snapshot;
        }
        state->condition.notify_all();
      });
  struct Unsubscriber {
    TransferManager* manager;
    std::size_t token;
    ~Unsubscriber() { manager->unsubscribe(token); }
  } unsubscriber{&manager, token};

  {
    std::lock_guard lock(state->mutex);
    if (const auto current = manager.get(transfer_id);
        current.has_value() && is_terminal(current->status)) {
      state->terminal = *current;
    }
  }
  std::unique_lock lock(state->mutex);
  state->condition.wait_for(lock, timeout, [&] { return state->terminal.has_value(); });
  return state->terminal;
}

}  // namespace

IpcRouter::IpcRouter(DesktopDatabase* database, const CredentialStore* credentials)
    : database_(database),
      credentials_(credentials == nullptr ? &default_credentials_ : credentials) {
  if (database_ == nullptr) return;
  try {
    const auto pending = database_->credential_cleanup_references();
    std::string ignored_error;
    (void)clean_credential_references(database_, *credentials_, pending, ignored_error);
  } catch (...) {
  }
  terminals_ = std::make_unique<TerminalManager>(database_, credentials_);
  terminals_->set_event_sink([this](const std::string& event, const nlohmann::json& payload) {
    std::function<void(const std::string&, const nlohmann::json&)> sink;
    {
      std::lock_guard lock(mutex_);
      sink = event_sink_;
    }
    if (sink) sink(event, payload);
  });
  mcp_ = std::make_unique<McpSupervisor>(database_, credentials_);
}

IpcRouter::~IpcRouter() {
  std::unique_ptr<TransferManager> transfers;
  std::size_t subscription = 0;
  {
    std::lock_guard lock(transfer_mutex_);
    transfers = std::move(transfers_);
    subscription = transfers_subscription_;
    transfers_subscription_ = 0;
  }
  if (!transfers) return;
  if (subscription != 0) transfers->unsubscribe(subscription);
  // Blocks until every active transfer has stopped and its staging cleanup ran.
  transfers->shutdown();
}

void IpcRouter::set_transfer_manager(std::unique_ptr<TransferManager> manager) {
  std::lock_guard lock(transfer_mutex_);
  if (transfers_ && transfers_subscription_ != 0) {
    transfers_->unsubscribe(transfers_subscription_);
  }
  transfers_ = std::move(manager);
  transfers_subscription_ = 0;
  if (transfers_) subscribe_transfer_events_locked();
}

void IpcRouter::subscribe_transfer_events_locked() {
  transfers_subscription_ = transfers_->subscribe([this](const TransferSnapshot& snapshot) {
    std::function<void(const std::string&, const nlohmann::json&)> sink;
    {
      std::lock_guard lock(mutex_);
      sink = event_sink_;
    }
    if (sink) sink("file.transfer_updated", {{"transfer", transfer_snapshot_json(snapshot)}});
  });
}

TransferManager* IpcRouter::ensure_transfer_manager() {
  std::lock_guard lock(transfer_mutex_);
  if (transfers_) return transfers_.get();
  if (database_ == nullptr) return nullptr;
  auto ops = make_ssh_service_sftp_remote_ops(database_, credentials_);
  if (!ops) return nullptr;
  auto backend = std::make_shared<SftpTransferBackend>(std::move(ops));
  // `with_history` performs synchronous, fail-closed startup recovery before the
  // manager admits work; a failure propagates so the command reports it loudly.
  auto manager = std::make_unique<TransferManager>(TransferManager::with_history(
      std::move(backend), std::make_shared<DatabaseTransferHistoryStore>(database_)));
  transfers_ = std::move(manager);
  subscribe_transfer_events_locked();
  return transfers_.get();
}

void IpcRouter::set_remote_drag_probe(std::shared_ptr<RemoteDragProbe> probe) {
  std::lock_guard lock(drag_mutex_);
  drag_probe_ = std::move(probe);
}

void IpcRouter::set_remote_drag_store(std::unique_ptr<RemoteDragStore> store) {
  std::lock_guard lock(drag_mutex_);
  drag_store_ = std::move(store);
}

void IpcRouter::set_remote_drag_wait_bounds(std::chrono::milliseconds download_timeout,
                                            std::chrono::milliseconds cancel_grace) {
  std::lock_guard lock(drag_mutex_);
  remote_drag_timeout_ = download_timeout;
  remote_drag_cancel_grace_ = cancel_grace;
}

std::shared_ptr<RemoteDragProbe> IpcRouter::ensure_remote_drag_probe() {
  std::lock_guard lock(drag_mutex_);
  if (drag_probe_) return drag_probe_;
  if (database_ == nullptr) return nullptr;
  drag_probe_ = make_ssh_service_remote_drag_probe(database_, credentials_);
  return drag_probe_;
}

RemoteDragStore* IpcRouter::ensure_remote_drag_store() {
  std::lock_guard lock(drag_mutex_);
  if (drag_store_) return drag_store_.get();
  if (database_ == nullptr) return nullptr;
  drag_store_ = RemoteDragStore::open(database_->data_directory() /
                                      RemoteDragStore::kDirectoryName);
  return drag_store_.get();
}

IpcReply IpcRouter::invoke(const std::string& method, nlohmann::json params) {
  const bool internal_event_method =
      method == "__events.subscribe" || method == "__events.unsubscribe";
  if (!internal_event_method && !valid_method_name(method)) {
    return failure("INVALID_METHOD", "Desktop IPC command name is invalid");
  }
  if (!params.is_object()) {
    return failure("INVALID_PARAMS", "Desktop IPC parameters must be an object");
  }

  if (method == "server_add" || method == "server_update" || method == "server_delete") {
    IpcSecretParamScrubber scrubber(params);
    return server_command(method, params);
  }

  if (method == "network_proxy_get" || method == "network_proxy_save") {
    return network_proxy_command(method, params);
  }

  if (method == "server_host_key_status" || method == "server_host_key_forget") {
    return host_key_command(method, params);
  }

  if (method == "server_host_key_probe" || method == "server_host_key_trust" ||
      method == "server_logs" || method == "server_services" ||
      method == "server_snapshot" || method == "remote_file_list" ||
      method == "remote_file_read" || method == "remote_file_preview") {
    return server_read_command(method, params);
  }

  if (method == "server_connect" || method == "server_disconnect" ||
      method == "server_auth_respond" || method == "server_auth_cancel") {
    return connection_command(method, params);
  }

  if (method == "terminal_open" || method == "terminal_write" ||
      method == "terminal_resize" || method == "terminal_close") {
    return terminal_command(method, params);
  }

  if (method == "investigation_task_list") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"status", "limit"}, {})) {
      return failure("INVALID_PARAMS", "investigation_task_list received an unexpected parameter");
    }
    std::optional<std::string> status;
    if (params.contains("status")) {
      if (!params.at("status").is_string()) {
        return failure("INVALID_PARAMS", "investigation_task_list status must be a string");
      }
      const auto value = params.at("status").get<std::string>();
      if (!is_one_of(value, {"pending", "investigating", "waiting_user", "executing",
                            "verifying", "completed", "failed", "stopped", "expired"})) {
        return failure("INVALID_PARAMS", "investigation_task_list status is invalid");
      }
      status = value;
    }
    int limit = 50;
    if (params.contains("limit")) {
      if (!params.at("limit").is_number_integer()) {
        return failure("INVALID_PARAMS", "investigation_task_list limit must be an integer");
      }
      const auto requested = params.at("limit").get<std::int64_t>();
      if (requested < 1 || requested > 100) {
        return failure("INVALID_PARAMS", "investigation_task_list limit must be between 1 and 100");
      }
      limit = static_cast<int>(requested);
    }
    try {
      return {true, database_->list_investigation_tasks(status, limit)};
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not read its tasks");
    }
  }

  if (method == "investigation_task_get") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"taskId"}, {"taskId"}) || !params.at("taskId").is_string()) {
      return failure("INVALID_PARAMS", "investigation_task_get requires exactly taskId");
    }
    const auto task_id = params.at("taskId").get<std::string>();
    if (task_id.empty() || task_id.size() > 256) {
      return failure("INVALID_PARAMS", "investigation_task_get taskId is invalid");
    }
    try {
      auto detail = database_->get_investigation_task(task_id);
      if (!detail.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
      return {true, std::move(*detail)};
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not read the task");
    }
  }

  if (method == "investigation_task_create") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"input"}, {"input"}) || !params.at("input").is_object()) {
      return failure("INVALID_PARAMS", "investigation_task_create requires an input object");
    }
    std::string validation_error;
    auto task = build_investigation_task(params.at("input"), *database_, validation_error);
    if (!task.has_value()) {
      return failure("INVALID_PARAMS",
                     validation_error.empty() ? "investigation_task_create input is invalid"
                                              : validation_error);
    }
    try {
      auto created = database_->create_investigation_task(*task);
      if (!created.has_value()) {
        return failure("STORAGE_UNAVAILABLE",
                       "The local investigation database is unavailable");
      }
      return {true, {{"task", std::move(*created)}}};
    } catch (const std::invalid_argument& invalid) {
      return failure("INVALID_PARAMS", invalid.what());
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not create the task");
    }
  }

  if (method == "investigation_task_start") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"taskId"}, {"taskId"}) || !params.at("taskId").is_string()) {
      return failure("INVALID_PARAMS", "investigation_task_start requires exactly taskId");
    }
    auto task_id = normalized_opaque_id(params.at("taskId").get<std::string>());
    if (!task_id.has_value()) {
      return failure("INVALID_PARAMS", "investigation_task_start taskId is invalid");
    }
    std::optional<nlohmann::json> task;
    try {
      task = load_investigation_task(*database_, *task_id);
    } catch (...) {
      return failure("STORAGE_ERROR", "The local investigation database could not read the task");
    }
    if (!task.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
    if (is_one_of(json_text_field(*task, "status"),
                  {"completed", "failed", "stopped", "expired"})) {
      return failure("INVALID_STATE",
                     "terminal investigation tasks cannot start a new Agent run");
    }
    if (const auto window_error = task_time_window_error(*task); window_error.has_value()) {
      return failure("INVALID_STATE", *window_error);
    }
    std::optional<std::string> active_run_id;
    if (task->contains("activeRunId") && task->at("activeRunId").is_string()) {
      active_run_id = task->at("activeRunId").get<std::string>();
    }
    if (active_run_id.has_value()) {
      std::optional<nlohmann::json> active_run;
      try {
        active_run = database_->get_investigation_run(*active_run_id);
      } catch (...) {
        return failure("STORAGE_ERROR",
                       "The local investigation database could not inspect the active Agent run");
      }
      if (active_run.has_value() &&
          is_one_of(json_text_field(*active_run, "status"),
                    {"admitted", "running", "waiting_user"})) {
        return failure("INVALID_STATE", "investigation task already has an active Agent run");
      }
    }
    const auto run_id = next_id("run");
    const auto session_id = "session_task_" + *task_id;
    const auto message_id = "message_" + run_id;
    const auto prompt = autonomous_task_prompt(*task);
    if (utf8_character_count(prompt) > kMaxPromptTextChars) {
      return failure("INVALID_STATE", "the investigation prompt exceeds the Agent text limit");
    }
    bool require_checkpoint = false;
    try {
      auto existing = database_->get_investigation_run(run_id);
      if (!existing.has_value()) {
        if (active_run_id.has_value() && *active_run_id != run_id) {
          std::string interrupt_error;
          if (!interrupt_stale_investigation_run(*database_, *task_id, *active_run_id,
                                                 interrupt_error)) {
            return failure("STORAGE_ERROR", interrupt_error);
          }
        }
        const auto attempt = next_investigation_attempt(*database_, *task_id);
        const auto budget = task->value("budget", nlohmann::json::object());
        const auto max_attempts = budget.value("maxAttempts", std::int64_t{3});
        if (attempt > max_attempts) {
          return failure("INVALID_STATE",
                         "investigation attempt budget exhausted (" +
                             std::to_string(attempt - 1) + "/" + std::to_string(max_attempts) +
                             ")");
        }
        const auto now = iso8601_now();
        nlohmann::json run = {{"id", run_id},
                              {"taskId", *task_id},
                              {"sessionId", session_id},
                              {"messageId", message_id},
                              {"attempt", attempt},
                              {"phase", "investigating"},
                              {"status", "admitted"},
                              {"startedAt", now},
                              {"updatedAt", now}};
        database_->create_investigation_run(run);
      } else if (json_text_field(*existing, "taskId") == *task_id) {
        require_checkpoint = json_text_field(*existing, "status") == "waiting_user";
      } else {
        return failure("INVALID_STATE",
                       "run id is already bound to another investigation task");
      }
      const auto now = iso8601_now();
      auto started = database_->update_investigation_task_progress(
          *task_id, "investigating", "investigating", run_id, std::nullopt, now, std::nullopt);
      if (!started.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
    } catch (const std::invalid_argument& invalid) {
      return failure("INVALID_PARAMS", invalid.what());
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not start the task");
    }
    nlohmann::json run_params = {
        {"runId", run_id},
        {"sessionId", session_id},
        {"prompt", prompt},
        {"messageId", message_id},
        {"parts", nlohmann::json::array({{{"type", "text"}, {"text", prompt}}})},
        {"delivery", "async"},
        {"resume", true},
        {"providerId", nullptr},
        {"model", nullptr},
        {"taskId", *task_id},
    };
    const auto budget = task->value("budget", nlohmann::json::object());
    run_params["taskBudget"] = {{"maxSteps", budget.value("maxSteps", std::int64_t{25})},
                                {"maxRunMs", budget.value("maxRunMs", std::int64_t{900000})},
                                {"maxAttempts", budget.value("maxAttempts", std::int64_t{3})}};
    const auto workspace_id = task->contains("workspaceId") && task->at("workspaceId").is_string()
                                  ? std::optional<std::string>(
                                        task->at("workspaceId").get<std::string>())
                                  : std::nullopt;
    const auto server_id = task->contains("serverId") && task->at("serverId").is_string()
                               ? std::optional<std::string>(task->at("serverId").get<std::string>())
                               : std::nullopt;
    const auto scope = task->value("scope", nlohmann::json::object());
    const auto scope_host = json_text_field(scope, "host");
    if (scope_host == "remote" && server_id.has_value()) {
      // A remote action step still in flight bounds the sidecar's allowed tools
      // to the declarative plan; the host alone reads it back from the plan row.
      try {
        auto plan = database_->latest_investigation_plan(*task_id);
        if (plan.has_value() && json_text_field(*plan, "status") == "active" &&
            plan->contains("currentStepId") && plan->at("currentStepId").is_string()) {
          const auto current_step_id = plan->at("currentStepId").get<std::string>();
          if (plan->contains("steps") && plan->at("steps").is_array()) {
            for (const auto& step : plan->at("steps")) {
              if (json_text_field(step, "id") != current_step_id) continue;
              if (json_text_field(step, "kind") != "action") break;
              bool bound_tool = false;
              const auto tools = step.value("allowedTools", nlohmann::json::array());
              if (tools.is_array()) {
                for (const auto& tool : tools) {
                  if (!tool.is_string()) continue;
                  if (is_one_of(tool.get<std::string>(),
                                {"server.exec", "docker.restart", "systemd.restart",
                                 "package.install"})) {
                    bound_tool = true;
                    break;
                  }
                }
              }
              if (bound_tool) {
                run_params["planBinding"] = {
                    {"planId", json_text_field(*plan, "id")},
                    {"planStepId", current_step_id},
                    {"evidenceIds", step.value("evidenceIds", nlohmann::json::array())}};
              }
              break;
            }
          }
        }
      } catch (...) {
        return failure("STORAGE_ERROR",
                       "The local investigation database could not read the current plan");
      }
    }
    if (workspace_id.has_value()) run_params["workspaceId"] = *workspace_id;
    if (scope_host == "local") {
      nlohmann::json target = {{"host", "local"},
                               {"environment", json_text_field(scope, "environment")}};
      if (workspace_id.has_value()) target["workspaceId"] = *workspace_id;
      run_params["target"] = std::move(target);
    }
    if (server_id.has_value()) {
      std::optional<nlohmann::json> server_record;
      try {
        const auto servers = database_->list_servers();
        const auto list = servers.value("servers", nlohmann::json::array());
        if (list.is_array()) {
          for (const auto& candidate : list) {
            if (json_text_field(candidate, "id") == *server_id) {
              server_record = candidate;
              break;
            }
          }
        }
      } catch (...) {
        return failure("STORAGE_ERROR", "The local Yukinal database could not read the server");
      }
      if (!server_record.has_value()) {
        return failure("NOT_FOUND", "The investigation task server is not available");
      }
      const auto environment =
          server_record->value("metadata", nlohmann::json::object()).value("environment",
                                                                            std::string("unknown"));
      nlohmann::json target = {
          {"host", "remote"}, {"serverId", *server_id}, {"environment", environment}};
      if (workspace_id.has_value()) target["workspaceId"] = *workspace_id;
      run_params["focusServerId"] = *server_id;
      run_params["target"] = std::move(target);
    }
    run_params["permissionMode"] = json_text_field(*task, "permissionMode", "ask");
    run_params["mode"] = json_text_field(*task, "mode", "goal");
    if (require_checkpoint) run_params["requireCheckpoint"] = true;
    nlohmann::json response;
    try {
      auto& client = ensure_sidecar();
      response = client.request("agent.run.start", run_params, std::chrono::seconds(60));
    } catch (const SidecarRpcError& error) {
      fail_admitted_investigation_run(*database_, *task_id, run_id, error.what(), "transport");
      return failure(sidecar_error_code(error.code()), error.what());
    } catch (const SidecarError& error) {
      fail_admitted_investigation_run(*database_, *task_id, run_id, error.what(), "transport");
      return failure("AGENT_UNAVAILABLE", error.what());
    } catch (...) {
      fail_admitted_investigation_run(*database_, *task_id, run_id,
                                      "the Agent run could not be started", "internal");
      return failure("AGENT_UNAVAILABLE", "The Agent run could not be started");
    }
    const auto returned_run_id = json_text_field(response, "runId");
    if (returned_run_id.empty() || !response.contains("started") ||
        !response.at("started").is_boolean()) {
      fail_admitted_investigation_run(*database_, *task_id, run_id,
                                      "agent sidecar returned an invalid run.start response",
                                      "internal");
      return failure("AGENT_ERROR", "agent sidecar returned an invalid run.start response");
    }
    if (returned_run_id != run_id) {
      fail_admitted_investigation_run(*database_, *task_id, run_id,
                                      "agent sidecar returned a different run id", "internal");
      return failure("AGENT_ERROR", "agent sidecar returned a different run id");
    }
    return {true, {{"runId", run_id}, {"started", response.at("started").get<bool>()}}};
  }

  if (method == "investigation_task_recover") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"input"}, {"input"}) || !params.at("input").is_object()) {
      return failure("INVALID_PARAMS", "investigation_task_recover requires an input object");
    }
    const auto& input = params.at("input");
    if (!only_fields(input, {"taskId", "optionId"}, {"taskId"}) ||
        !input.at("taskId").is_string()) {
      return failure("INVALID_PARAMS", "investigation_task_recover input is invalid");
    }
    auto task_id = normalized_opaque_id(input.at("taskId").get<std::string>());
    if (!task_id.has_value()) {
      return failure("INVALID_PARAMS", "investigation_task_recover taskId is invalid");
    }
    std::optional<std::string> option_id;
    if (input.contains("optionId") && !input.at("optionId").is_null()) {
      if (!input.at("optionId").is_string()) {
        return failure("INVALID_PARAMS", "investigation_task_recover optionId is invalid");
      }
      option_id = normalized_opaque_id(input.at("optionId").get<std::string>());
      if (!option_id.has_value()) {
        return failure("INVALID_PARAMS", "investigation_task_recover optionId is invalid");
      }
    }
    std::optional<nlohmann::json> task;
    try {
      task = load_investigation_task(*database_, *task_id);
    } catch (...) {
      return failure("STORAGE_ERROR", "The local investigation database could not read the task");
    }
    if (!task.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
    if (is_one_of(json_text_field(*task, "status"), {"completed", "expired"})) {
      return failure("INVALID_STATE",
                     "completed or expired investigation tasks cannot be recovered");
    }
    std::optional<std::string> selected_action;
    if (option_id.has_value()) {
      const auto options = task->value("lastFailure", nlohmann::json::object())
                               .value("options", nlohmann::json::array());
      if (options.is_array()) {
        for (const auto& option : options) {
          if (json_text_field(option, "id") == *option_id) {
            selected_action = json_text_field(option, "action");
            break;
          }
        }
      }
      if (!selected_action.has_value()) {
        return failure("INVALID_STATE", "the selected failure option is no longer available");
      }
    }
    if (selected_action.has_value() && *selected_action == "inspect") {
      return {true, {{"task", *task}}};
    }
    const auto last_failure_code =
        task->contains("lastFailure") && task->at("lastFailure").is_object()
            ? json_text_field(task->at("lastFailure"), "code")
            : std::string();
    if (last_failure_code == "outcome_unknown" && !option_id.has_value()) {
      return failure("INVALID_STATE",
                     "the previous remote action may have taken effect; inspect the target and "
                     "choose an explicit recovery action");
    }
    const auto now = iso8601_now();
    nlohmann::json failure_record;
    if (task->contains("lastFailure") && task->at("lastFailure").is_object()) {
      failure_record = task->at("lastFailure");
    } else {
      failure_record = build_task_failure("unknown", "任务由用户请求恢复，上一轮运行被标记为中断", 1,
                                          now, std::nullopt);
    }
    const auto attempt = failure_record.value("attempt", std::int64_t{1});
    failure_record["attempt"] = attempt + 1;
    failure_record["at"] = now;
    nlohmann::json recovery_detail = {
        {"rollbackRequested",
         selected_action.has_value() && *selected_action == "rollback"},
        {"requiresFreshBaseline", true}};
    if (option_id.has_value()) recovery_detail["selectedOption"] = *option_id;
    failure_record["detail"] = std::move(recovery_detail);
    try {
      auto plan = database_->latest_investigation_plan(*task_id);
      if (plan.has_value()) {
        auto artifact_list = database_->list_investigation_artifacts(*task_id, 128);
        nlohmann::json artifacts =
            artifact_list.value("artifacts", nlohmann::json::array());
        invalidate_plan_after_recovery(*plan, artifacts, now);
        if (artifacts.is_array()) {
          for (const auto& artifact : artifacts) {
            if (json_text_field(artifact, "kind") != "baseline" ||
                json_text_field(artifact, "status") != "superseded" ||
                artifact.value("updatedAt", std::string()) != now) {
              continue;
            }
            database_->upsert_investigation_artifact(artifact);
          }
        }
        database_->save_investigation_plan(*plan);
      }
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not invalidate the recovered plan");
    }
    std::optional<nlohmann::json> recovered;
    try {
      recovered = database_->recover_investigation_task(*task_id, now, failure_record);
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not recover the task");
    }
    if (!recovered.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
    if (selected_action.has_value() &&
        (*selected_action == "stop" || *selected_action == "rollback")) {
      try {
        auto updated = database_->set_investigation_task_status(
            *task_id, *selected_action == "stop" ? "stopped" : "waiting_user",
            *selected_action == "stop" ? std::optional<std::string>(now) : std::nullopt, now);
        if (!updated.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
        recovered = std::move(updated);
      } catch (...) {
        return failure("STORAGE_ERROR",
                       "The local investigation database could not update the recovered task");
      }
    } else if (selected_action.has_value() && *selected_action == "wait_user") {
      try {
        auto updated = database_->set_investigation_task_status(*task_id, "waiting_user",
                                                                std::nullopt, now);
        if (!updated.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
        recovered = std::move(updated);
      } catch (...) {
        return failure("STORAGE_ERROR",
                       "The local investigation database could not update the recovered task");
      }
    }
    return {true, {{"task", std::move(*recovered)}}};
  }

  if (method == "investigation_task_status_update") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    const std::set<std::string> fields = {"taskId", "status", "completedAt"};
    if (!only_fields(params, fields, {"taskId", "status"}) ||
        !params.at("taskId").is_string() || !params.at("status").is_string()) {
      return failure("INVALID_PARAMS", "investigation_task_status_update parameters are invalid");
    }
    const auto task_id = params.at("taskId").get<std::string>();
    const auto status = params.at("status").get<std::string>();
    if (task_id.empty() || task_id.size() > 256) {
      return failure("INVALID_PARAMS", "investigation_task_status_update taskId is invalid");
    }
    if (!is_one_of(status, {"pending", "investigating", "waiting_user", "executing",
                            "verifying", "completed", "failed", "stopped", "expired"})) {
      return failure("INVALID_PARAMS", "investigation_task_status_update status is invalid");
    }
    std::optional<std::string> completed_at;
    if (params.contains("completedAt")) {
      if (!params.at("completedAt").is_string()) {
        return failure("INVALID_PARAMS", "investigation_task_status_update completedAt is invalid");
      }
      completed_at = params.at("completedAt").get<std::string>();
      if (completed_at->empty() || completed_at->size() > 80) {
        return failure("INVALID_PARAMS", "investigation_task_status_update completedAt is invalid");
      }
    }
    const auto now = iso8601_now();
    const bool terminal = is_one_of(status, {"completed", "failed", "stopped", "expired"});
    if (!terminal) {
      completed_at = std::nullopt;
    } else if (!completed_at.has_value()) {
      completed_at = now;
    }
    try {
      auto updated = database_->set_investigation_task_status(task_id, status, completed_at, now);
      if (!updated.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
      return {true, {{"task", std::move(*updated)}}};
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not update the task");
    }
  }

  if (method == "investigation_task_stop") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"taskId"}, {"taskId"}) || !params.at("taskId").is_string()) {
      return failure("INVALID_PARAMS", "investigation_task_stop requires exactly taskId");
    }
    const auto task_id = params.at("taskId").get<std::string>();
    if (task_id.empty() || task_id.size() > 256) {
      return failure("INVALID_PARAMS", "investigation_task_stop taskId is invalid");
    }
    std::optional<nlohmann::json> task;
    try {
      task = database_->get_investigation_task(task_id);
    } catch (...) {
      return failure("STORAGE_ERROR", "The local investigation database could not read the task");
    }
    if (!task.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
    const auto status = task->at("task").value("status", std::string());
    if (status == "stopped") return {true, {{"task", task->at("task")}}};
    if (is_one_of(status, {"completed", "failed", "expired"})) {
      return failure("INVALID_STATE",
                     "completed, failed, or expired investigation tasks cannot be stopped");
    }
    // Best-effort cancel of the durable Agent run; a missing or already-finished
    // run is not an error, and the task state below is the authoritative stop.
    try {
      if (const auto run_id = database_->active_investigation_run_id(task_id);
          run_id.has_value() && sidecar_ != nullptr && sidecar_->status().running) {
        (void)ensure_sidecar().request("agent.run.stop", {{"runId", *run_id}},
                                       std::chrono::seconds(15));
      }
    } catch (...) {
    }
    try {
      const auto now = iso8601_now();
      auto updated = database_->set_investigation_task_status(task_id, "stopped", now, now);
      if (!updated.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
      return {true, {{"task", std::move(*updated)}}};
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not stop the task");
    }
  }

  if (method == "investigation_brief_select") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"input"}, {"input"}) || !params.at("input").is_object()) {
      return failure("INVALID_PARAMS", "investigation_brief_select requires an input object");
    }
    const auto& input = params.at("input");
    if (!only_fields(input, {"taskId", "briefId", "optionId"},
                     {"taskId", "briefId", "optionId"}) ||
        !input.at("taskId").is_string() || !input.at("briefId").is_string() ||
        !input.at("optionId").is_string()) {
      return failure("INVALID_PARAMS", "investigation_brief_select input is invalid");
    }
    auto task_id = normalized_opaque_id(input.at("taskId").get<std::string>());
    auto brief_id = normalized_opaque_id(input.at("briefId").get<std::string>());
    auto option_id = normalized_opaque_id(input.at("optionId").get<std::string>());
    if (!task_id.has_value() || !brief_id.has_value() || !option_id.has_value()) {
      return failure("INVALID_PARAMS", "investigation_brief_select identifiers are invalid");
    }
    std::optional<nlohmann::json> task;
    try {
      task = load_investigation_task(*database_, *task_id);
    } catch (...) {
      return failure("STORAGE_ERROR", "The local investigation database could not read the task");
    }
    if (!task.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
    if (is_one_of(task->value("status", std::string()),
                  {"completed", "failed", "stopped", "expired"})) {
      return failure("INVALID_STATE",
                     "terminal investigation tasks cannot select a decision option");
    }
    std::optional<nlohmann::json> brief;
    try {
      brief = database_->select_investigation_brief_option(*task_id, *brief_id, *option_id);
    } catch (const std::invalid_argument& invalid) {
      return failure("INVALID_PARAMS", invalid.what());
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not select the decision option");
    }
    if (!brief.has_value()) return failure("NOT_FOUND", "The decision brief was not found");
    std::string continuation = "wait_user";
    const auto options = brief->value("options", nlohmann::json::array());
    if (options.is_array()) {
      for (const auto& option : options) {
        if (json_text_field(option, "id") == *option_id) {
          continuation = json_text_field(option, "continuation", "wait_user");
          break;
        }
      }
    }
    // Only an explicit `start_plan` continuation approves the linked dry-run plan;
    // read-only and wait-user continuations leave it pending.
    if (continuation == "start_plan") {
      const auto plan_id = brief->contains("planId") && brief->at("planId").is_string()
                               ? std::optional<std::string>(brief->at("planId").get<std::string>())
                               : std::nullopt;
      if (plan_id.has_value()) {
        try {
          auto plan = database_->latest_investigation_plan(*task_id);
          if (plan.has_value() && json_text_field(*plan, "id") == *plan_id) {
            const auto now = iso8601_now();
            (*plan)["approval"] = {
                {"status", "approved"},
                {"source", "user"},
                {"optionId", *option_id},
                {"approvedAt", now},
                {"note", "用户选择了该决策摘要方案；行动步骤仍受逐项权限审批约束"}};
            (*plan)["updatedAt"] = now;
            database_->save_investigation_plan(*plan);
          }
        } catch (...) {
          return failure("STORAGE_ERROR",
                         "The local investigation database could not approve the selected plan");
        }
      }
    } else if (continuation == "stop") {
      // Best-effort cancel of the durable Agent run, then the terminal stop; a
      // missing or already-finished run is not an error.
      try {
        if (const auto run_id = database_->active_investigation_run_id(*task_id);
            run_id.has_value() && sidecar_ != nullptr && sidecar_->status().running) {
          (void)ensure_sidecar().request("agent.run.stop", {{"runId", *run_id}},
                                         std::chrono::seconds(15));
        }
      } catch (...) {
      }
      const auto now = iso8601_now();
      try {
        auto updated = database_->set_investigation_task_status(*task_id, "stopped", now, now);
        if (!updated.has_value()) {
          return failure("NOT_FOUND", "The investigation task was not found");
        }
      } catch (...) {
        return failure("STORAGE_ERROR",
                       "The local investigation database could not stop the task");
      }
    }
    return {true, {{"brief", std::move(*brief)}, {"continuation", continuation}}};
  }

  if (method == "investigation_retention_preview") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"taskId", "cutoffAt", "limit"}, {"taskId"}) ||
        !params.at("taskId").is_string()) {
      return failure("INVALID_PARAMS", "investigation_retention_preview parameters are invalid");
    }
    auto task_id = normalized_opaque_id(params.at("taskId").get<std::string>());
    if (!task_id.has_value()) {
      return failure("INVALID_PARAMS", "investigation_retention_preview taskId is invalid");
    }
    std::string cutoff;
    if (params.contains("cutoffAt")) {
      if (!params.at("cutoffAt").is_string()) {
        return failure("INVALID_PARAMS", "investigation_retention_preview cutoffAt is invalid");
      }
      cutoff = trim_ascii(params.at("cutoffAt").get<std::string>());
      if (cutoff.empty() || cutoff.size() > 80 ||
          !parse_iso8601_utc_seconds(cutoff).has_value()) {
        return failure("INVALID_PARAMS", "retention cutoff must be a UTC ISO-8601 timestamp");
      }
    } else {
      cutoff = iso8601_from_epoch(now_epoch_seconds() - 30LL * 86400);
    }
    int limit = 64;
    if (params.contains("limit")) {
      if (!params.at("limit").is_number_integer()) {
        return failure("INVALID_PARAMS",
                       "investigation_retention_preview limit must be an integer");
      }
      const auto requested = params.at("limit").get<std::int64_t>();
      if (requested < 1 || requested > 128) {
        return failure("INVALID_PARAMS", "retention preview limit must be between 1 and 128");
      }
      limit = static_cast<int>(requested);
    }
    try {
      auto preview = database_->investigation_retention_preview(*task_id, cutoff, limit);
      if (!preview.has_value()) {
        return failure("NOT_FOUND", "The investigation task was not found");
      }
      return {true, {{"preview", std::move(*preview)}}};
    } catch (const std::invalid_argument& invalid) {
      return failure("INVALID_PARAMS", invalid.what());
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not build the retention preview");
    }
  }

  if (method == "investigation_retention_prune") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"input"}, {"input"}) || !params.at("input").is_object()) {
      return failure("INVALID_PARAMS", "investigation_retention_prune requires an input object");
    }
    const auto& input = params.at("input");
    if (!only_fields(input, {"taskId", "cutoffAt", "items", "confirmation"},
                     {"taskId", "cutoffAt", "items", "confirmation"}) ||
        !input.at("taskId").is_string() || !input.at("cutoffAt").is_string() ||
        !input.at("confirmation").is_string() || !input.at("items").is_array()) {
      return failure("INVALID_PARAMS", "investigation_retention_prune input is invalid");
    }
    auto task_id = normalized_opaque_id(input.at("taskId").get<std::string>());
    if (!task_id.has_value()) {
      return failure("INVALID_PARAMS", "investigation_retention_prune taskId is invalid");
    }
    const auto cutoff = trim_ascii(input.at("cutoffAt").get<std::string>());
    if (cutoff.empty() || cutoff.size() > 80 || !parse_iso8601_utc_seconds(cutoff).has_value()) {
      return failure("INVALID_PARAMS", "retention cutoff must be a UTC ISO-8601 timestamp");
    }
    if (input.at("confirmation").get<std::string>() != "delete_unreferenced") {
      return failure("INVALID_PARAMS",
                     "retention prune requires confirmation=delete_unreferenced");
    }
    const auto& items = input.at("items");
    if (items.empty() || items.size() > 128) {
      return failure("INVALID_PARAMS", "retention prune requires between 1 and 128 items");
    }
    nlohmann::json normalized_items = nlohmann::json::array();
    for (const auto& item : items) {
      if (!item.is_object() ||
          !only_fields(item, {"id", "kind"}, {"id", "kind"}) || !item.at("id").is_string() ||
          !item.at("kind").is_string()) {
        return failure("INVALID_PARAMS", "each retention prune item requires an id and kind");
      }
      auto item_id = normalized_opaque_id(item.at("id").get<std::string>());
      const auto kind = item.at("kind").get<std::string>();
      if (!item_id.has_value() || (kind != "evidence" && kind != "artifact")) {
        return failure("INVALID_PARAMS", "retention prune items are invalid");
      }
      normalized_items.push_back({{"id", *item_id}, {"kind", kind}});
    }
    try {
      auto result = database_->investigation_retention_prune(
          *task_id, cutoff, normalized_items, next_id("retention"), iso8601_now());
      if (!result.has_value()) {
        return failure("NOT_FOUND", "The investigation task was not found");
      }
      return {true, {{"result", std::move(*result)}}};
    } catch (const std::invalid_argument& invalid) {
      return failure("INVALID_PARAMS", invalid.what());
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not prune the requested history");
    }
  }

  if (method == "investigation_schedule_list") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!params.empty()) {
      return failure("INVALID_PARAMS", "investigation_schedule_list does not accept parameters");
    }
    try {
      return {true, database_->list_investigation_schedules(kScheduleListLimit)};
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not read schedules");
    }
  }

  if (method == "investigation_schedule_runs") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"scheduleId"}, {"scheduleId"}) ||
        !params.at("scheduleId").is_string()) {
      return failure("INVALID_PARAMS", "investigation_schedule_runs requires exactly scheduleId");
    }
    auto schedule_id = normalized_opaque_id(params.at("scheduleId").get<std::string>());
    if (!schedule_id.has_value()) {
      return failure("INVALID_PARAMS",
                     "investigation_schedule_runs scheduleId must be 1 to 256 visible characters");
    }
    try {
      return {true, database_->list_investigation_schedule_runs(*schedule_id, kScheduleListLimit)};
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not read schedule runs");
    }
  }

  if (method == "investigation_schedule_create") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"input"}, {"input"}) || !params.at("input").is_object()) {
      return failure("INVALID_PARAMS", "investigation_schedule_create requires an input object");
    }
    const auto& input = params.at("input");
    if (!only_fields(input,
                     {"id", "taskId", "intervalSeconds", "cooldownSeconds", "dedupeWindowSeconds",
                      "maxConcurrentRuns", "budget", "notificationPolicy", "nextRunAt"},
                     {"taskId", "intervalSeconds"}) ||
        !input.at("taskId").is_string() || !input.at("intervalSeconds").is_number_integer()) {
      return failure("INVALID_PARAMS", "investigation_schedule_create input is invalid");
    }
    if ((input.contains("id") && !input.at("id").is_string()) ||
        (input.contains("cooldownSeconds") && !input.at("cooldownSeconds").is_number_integer()) ||
        (input.contains("dedupeWindowSeconds") &&
         !input.at("dedupeWindowSeconds").is_number_integer()) ||
        (input.contains("maxConcurrentRuns") &&
         !input.at("maxConcurrentRuns").is_number_integer()) ||
        (input.contains("notificationPolicy") && !input.at("notificationPolicy").is_string()) ||
        (input.contains("nextRunAt") && !input.at("nextRunAt").is_string()) ||
        (input.contains("budget") && !input.at("budget").is_object())) {
      return failure("INVALID_PARAMS",
                     "investigation_schedule_create optional fields are the wrong type");
    }
    auto task_id = normalized_opaque_id(input.at("taskId").get<std::string>());
    if (!task_id.has_value()) {
      return failure("INVALID_PARAMS", "investigation_schedule_create taskId is invalid");
    }
    std::optional<nlohmann::json> task;
    try {
      task = load_investigation_task(*database_, *task_id);
    } catch (...) {
      return failure("STORAGE_ERROR", "The local investigation database could not read the task");
    }
    if (!task.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
    if (const auto task_error = scheduler_task_error(*task); task_error.has_value()) {
      return failure("INVALID_STATE", *task_error);
    }
    std::optional<std::string> schedule_id;
    if (input.contains("id")) {
      schedule_id = normalized_opaque_id(input.at("id").get<std::string>());
      if (!schedule_id.has_value()) {
        return failure("INVALID_PARAMS", "schedule id must be between 1 and 256 visible characters");
      }
    } else {
      schedule_id = next_id("schedule");
    }
    const auto now = iso8601_now();
    nlohmann::json schedule = {
        {"id", *schedule_id},
        {"taskId", *task_id},
        {"status", "active"},
        {"intervalSeconds", input.at("intervalSeconds").get<std::int64_t>()},
        {"cooldownSeconds", input.contains("cooldownSeconds")
                                ? input.at("cooldownSeconds").get<std::int64_t>()
                                : kDefaultScheduleCooldownSeconds},
        {"dedupeWindowSeconds", input.contains("dedupeWindowSeconds")
                                    ? input.at("dedupeWindowSeconds").get<std::int64_t>()
                                    : kDefaultScheduleDedupeSeconds},
        {"maxConcurrentRuns", input.contains("maxConcurrentRuns")
                                  ? input.at("maxConcurrentRuns").get<std::int64_t>()
                                  : kDefaultScheduleMaxConcurrentRuns},
        {"budget", input.contains("budget") ? input.at("budget") : task->at("budget")},
        {"notificationPolicy", input.contains("notificationPolicy")
                                   ? input.at("notificationPolicy").get<std::string>()
                                   : std::string("on_change")},
        {"nextRunAt", input.contains("nextRunAt") ? input.at("nextRunAt").get<std::string>() : now},
        {"createdAt", now},
        {"updatedAt", now},
    };
    if (const auto input_error = schedule_input_error(schedule); input_error.has_value()) {
      return failure("INVALID_PARAMS", *input_error);
    }
    try {
      auto created = database_->create_investigation_schedule(schedule);
      if (!created.has_value()) return failure("NOT_FOUND", "The investigation task was not found");
      return {true, {{"schedule", std::move(*created)}}};
    } catch (const std::invalid_argument& invalid) {
      return failure("INVALID_PARAMS", invalid.what());
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not create the schedule");
    }
  }

  if (method == "investigation_schedule_update") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"input"}, {"input"}) || !params.at("input").is_object()) {
      return failure("INVALID_PARAMS", "investigation_schedule_update requires an input object");
    }
    const auto& input = params.at("input");
    if (!only_fields(input,
                     {"scheduleId", "status", "intervalSeconds", "cooldownSeconds",
                      "dedupeWindowSeconds", "maxConcurrentRuns", "budget", "notificationPolicy",
                      "nextRunAt", "baselineRunId"},
                     {"scheduleId"}) ||
        !input.at("scheduleId").is_string()) {
      return failure("INVALID_PARAMS", "investigation_schedule_update input is invalid");
    }
    if ((input.contains("status") && !input.at("status").is_string()) ||
        (input.contains("intervalSeconds") && !input.at("intervalSeconds").is_number_integer()) ||
        (input.contains("cooldownSeconds") && !input.at("cooldownSeconds").is_number_integer()) ||
        (input.contains("dedupeWindowSeconds") &&
         !input.at("dedupeWindowSeconds").is_number_integer()) ||
        (input.contains("maxConcurrentRuns") &&
         !input.at("maxConcurrentRuns").is_number_integer()) ||
        (input.contains("budget") && !input.at("budget").is_object()) ||
        (input.contains("notificationPolicy") && !input.at("notificationPolicy").is_string()) ||
        (input.contains("nextRunAt") && !input.at("nextRunAt").is_string()) ||
        (input.contains("baselineRunId") && !input.at("baselineRunId").is_string() &&
         !input.at("baselineRunId").is_null())) {
      return failure("INVALID_PARAMS",
                     "investigation_schedule_update optional fields are the wrong type");
    }
    if (input.contains("status") &&
        !is_one_of(input.at("status").get<std::string>(), {"active", "paused", "revoked"})) {
      return failure("INVALID_PARAMS", "investigation_schedule_update status is invalid");
    }
    if (input.contains("notificationPolicy") &&
        !is_one_of(input.at("notificationPolicy").get<std::string>(),
                   {"silent", "on_change", "always", "failed_runs_only"})) {
      return failure("INVALID_PARAMS",
                     "investigation_schedule_update notificationPolicy is invalid");
    }
    auto schedule_id = normalized_opaque_id(input.at("scheduleId").get<std::string>());
    if (!schedule_id.has_value()) {
      return failure("INVALID_PARAMS",
                     "investigation_schedule_update scheduleId must be 1 to 256 visible characters");
    }
    std::optional<nlohmann::json> schedule;
    std::optional<nlohmann::json> task;
    try {
      schedule = database_->get_investigation_schedule(*schedule_id);
      if (schedule.has_value()) {
        task = load_investigation_task(*database_, schedule->value("taskId", std::string()));
      }
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not read the schedule");
    }
    if (!schedule.has_value()) {
      return failure("NOT_FOUND", "The investigation schedule was not found");
    }
    if (!task.has_value()) return failure("NOT_FOUND", "The investigation task was not found");

    const bool baseline_only_update =
        input.contains("baselineRunId") && !input.contains("status") &&
        !input.contains("intervalSeconds") && !input.contains("cooldownSeconds") &&
        !input.contains("dedupeWindowSeconds") && !input.contains("maxConcurrentRuns") &&
        !input.contains("budget") && !input.contains("notificationPolicy") &&
        !input.contains("nextRunAt");
    const bool requires_live_task =
        !(input.contains("status") && input.at("status").get<std::string>() == "revoked");
    if (!baseline_only_update && requires_live_task) {
      if (const auto task_error = scheduler_task_error(*task); task_error.has_value()) {
        return failure("INVALID_STATE", *task_error);
      }
    }
    if (schedule->value("status", std::string()) == "revoked" && input.contains("status") &&
        input.at("status").get<std::string>() != "revoked") {
      return failure("INVALID_STATE", "revoked schedules cannot be reactivated");
    }
    auto updated = *schedule;
    if (input.contains("status")) updated["status"] = input.at("status").get<std::string>();
    if (input.contains("intervalSeconds")) {
      updated["intervalSeconds"] = input.at("intervalSeconds").get<std::int64_t>();
    }
    if (input.contains("cooldownSeconds")) {
      updated["cooldownSeconds"] = input.at("cooldownSeconds").get<std::int64_t>();
    }
    if (input.contains("dedupeWindowSeconds")) {
      updated["dedupeWindowSeconds"] = input.at("dedupeWindowSeconds").get<std::int64_t>();
    }
    if (input.contains("maxConcurrentRuns")) {
      updated["maxConcurrentRuns"] = input.at("maxConcurrentRuns").get<std::int64_t>();
    }
    if (input.contains("budget")) updated["budget"] = input.at("budget");
    if (input.contains("notificationPolicy")) {
      updated["notificationPolicy"] = input.at("notificationPolicy").get<std::string>();
    }
    if (input.contains("nextRunAt")) updated["nextRunAt"] = input.at("nextRunAt").get<std::string>();
    if (input.contains("baselineRunId")) {
      if (input.at("baselineRunId").is_null()) {
        updated.erase("baselineRunId");
      } else {
        auto baseline_run_id =
            normalized_opaque_id(input.at("baselineRunId").get<std::string>());
        if (!baseline_run_id.has_value()) {
          return failure("INVALID_PARAMS",
                         "baseline run id must be between 1 and 256 visible characters");
        }
        std::optional<nlohmann::json> baseline_run;
        try {
          baseline_run = database_->get_investigation_schedule_run(*baseline_run_id);
        } catch (...) {
          return failure("STORAGE_ERROR",
                         "The local investigation database could not read the baseline run");
        }
        if (!baseline_run.has_value()) {
          return failure("NOT_FOUND", "The baseline schedule run was not found");
        }
        if (baseline_run->value("scheduleId", std::string()) != *schedule_id) {
          return failure("INVALID_PARAMS", "baseline run must belong to this schedule");
        }
        if (baseline_run->value("status", std::string()) != "succeeded") {
          return failure("INVALID_PARAMS", "baseline run must be a succeeded schedule run");
        }
        if (database_->investigation_evidence_count_for_run(*baseline_run_id) <= 0) {
          return failure("INVALID_PARAMS",
                         "baseline run must contain at least one evidence item");
        }
        updated["baselineRunId"] = *baseline_run_id;
      }
    }
    updated["updatedAt"] = iso8601_now();
    if (const auto input_error = schedule_input_error(updated); input_error.has_value()) {
      return failure("INVALID_PARAMS", *input_error);
    }
    try {
      auto result = database_->update_investigation_schedule(updated);
      if (!result.has_value()) {
        return failure("NOT_FOUND", "The investigation schedule was not found");
      }
      return {true, {{"schedule", std::move(*result)}}};
    } catch (const std::invalid_argument& invalid) {
      return failure("INVALID_PARAMS", invalid.what());
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not update the schedule");
    }
  }

  if (method == "investigation_schedule_tick") {
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
    }
    if (!only_fields(params, {"input"}, {"input"}) || !params.at("input").is_object()) {
      return failure("INVALID_PARAMS", "investigation_schedule_tick requires an input object");
    }
    const auto& input = params.at("input");
    if (!only_fields(input, {"now", "limit"}, {}) ||
        (input.contains("now") && !input.at("now").is_string()) ||
        (input.contains("limit") && !input.at("limit").is_number_integer())) {
      return failure("INVALID_PARAMS", "investigation_schedule_tick input is invalid");
    }
    std::string now = iso8601_now();
    if (input.contains("now")) {
      now = trim_ascii(input.at("now").get<std::string>());
      if (now.empty() || now.size() > 80 || !parse_iso8601_utc_seconds(now).has_value()) {
        return failure("INVALID_PARAMS",
                       "investigation_schedule_tick now must be a UTC ISO-8601 timestamp");
      }
    }
    int limit = kDefaultScheduleTickLimit;
    if (input.contains("limit")) {
      const auto requested = input.at("limit").get<std::int64_t>();
      if (requested < 1 || requested > 100) {
        return failure("INVALID_PARAMS", "schedule tick limit must be between 1 and 100");
      }
      limit = static_cast<int>(requested);
    }
    try {
      return {true, database_->claim_due_investigation_schedule_runs(now, limit)};
    } catch (const std::invalid_argument& invalid) {
      return failure("INVALID_PARAMS", invalid.what());
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local investigation database could not claim due schedule runs");
    }
  }

  if (method == "core_ping") {
    if (!params.empty()) {
      return failure("INVALID_PARAMS", "core_ping does not accept parameters");
    }
    return {true, {{"version", YUKINAL_DESKTOP_VERSION}, {"os", operating_system()}}};
  }

  if (method == "server_list") {
    if (!params.empty()) {
      return failure("INVALID_PARAMS", "server_list does not accept parameters");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local server database is unavailable");
    }
    try {
      return {true, database_->list_servers()};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local server database could not read servers");
    }
  }

  if (method == "workspace_list") {
    if (!params.empty()) {
      return failure("INVALID_PARAMS", "workspace_list does not accept parameters");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local workspace database is unavailable");
    }
    try {
      return {true, database_->list_workspaces()};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local workspace database could not read workspaces");
    }
  }

  if (method == "activity_list") {
    for (auto item = params.begin(); item != params.end(); ++item) {
      if (item.key() != "serverId" && item.key() != "limit") {
        return failure("INVALID_PARAMS", "activity_list received an unexpected parameter");
      }
    }
    std::optional<std::string> server_id;
    if (params.contains("serverId")) {
      if (!params.at("serverId").is_string()) {
        return failure("INVALID_PARAMS", "activity_list serverId must be a string");
      }
      const auto value = params.at("serverId").get<std::string>();
      if (!valid_server_id(value)) {
        return failure("INVALID_PARAMS", "activity_list serverId is invalid");
      }
      server_id = value;
    }
    int limit = 50;
    if (params.contains("limit")) {
      const auto& value = params.at("limit");
      if (value.is_number_unsigned()) {
        const auto requested_limit = value.get<std::uint64_t>();
        if (requested_limit < 1 || requested_limit > 100) {
          return failure("INVALID_PARAMS", "activity_list limit must be between 1 and 100");
        }
        limit = static_cast<int>(requested_limit);
      } else if (value.is_number_integer()) {
        const auto requested_limit = value.get<std::int64_t>();
        if (requested_limit < 1 || requested_limit > 100) {
          return failure("INVALID_PARAMS", "activity_list limit must be between 1 and 100");
        }
        limit = static_cast<int>(requested_limit);
      } else {
        return failure("INVALID_PARAMS", "activity_list limit must be an integer");
      }
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local activity database is unavailable");
    }
    try {
      return {true, database_->list_activities(server_id, limit)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local activity database could not read activities");
    }
  }

  if (method == "tool_execution_list") {
    for (auto item = params.begin(); item != params.end(); ++item) {
      if (item.key() != "traceId" && item.key() != "serverId" && item.key() != "limit") {
        return failure("INVALID_PARAMS", "tool_execution_list received an unexpected parameter");
      }
    }
    std::optional<std::string> trace_id;
    if (params.contains("traceId")) {
      if (!params.at("traceId").is_string()) {
        return failure("INVALID_PARAMS", "tool_execution_list traceId must be a string");
      }
      const auto value = params.at("traceId").get<std::string>();
      if (value.empty()) return failure("INVALID_PARAMS", "tool_execution_list traceId is empty");
      trace_id = value;
    }
    std::optional<std::string> server_id;
    if (params.contains("serverId")) {
      if (!params.at("serverId").is_string()) {
        return failure("INVALID_PARAMS", "tool_execution_list serverId must be a string");
      }
      const auto value = params.at("serverId").get<std::string>();
      if (!valid_server_id(value)) {
        return failure("INVALID_PARAMS", "tool_execution_list serverId is invalid");
      }
      server_id = value;
    }
    int limit = 50;
    if (params.contains("limit")) {
      const auto& value = params.at("limit");
      if (value.is_number_unsigned()) {
        const auto requested_limit = value.get<std::uint64_t>();
        if (requested_limit < 1 || requested_limit > 100) {
          return failure("INVALID_PARAMS", "tool_execution_list limit must be between 1 and 100");
        }
        limit = static_cast<int>(requested_limit);
      } else if (value.is_number_integer()) {
        const auto requested_limit = value.get<std::int64_t>();
        if (requested_limit < 1 || requested_limit > 100) {
          return failure("INVALID_PARAMS", "tool_execution_list limit must be between 1 and 100");
        }
        limit = static_cast<int>(requested_limit);
      } else {
        return failure("INVALID_PARAMS", "tool_execution_list limit must be an integer");
      }
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local execution database is unavailable");
    }
    try {
      return {true, database_->list_tool_executions(trace_id, server_id, limit)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local execution database could not read executions");
    }
  }

  if (method == "chat_session_list") {
    if (!only_fields(params, {"query", "archived", "offset", "limit"}, {})) {
      return failure("INVALID_PARAMS", "chat_session_list received unexpected parameters");
    }
    std::optional<std::string> query;
    if (params.contains("query")) {
      std::string value;
      if (!valid_request_text(params, "query", 200, false, value)) {
        return failure("INVALID_PARAMS", "chat_session_list query must be at most 200 characters");
      }
      query = std::move(value);
    }
    std::optional<bool> archived;
    if (params.contains("archived")) {
      if (!params.at("archived").is_boolean()) {
        return failure("INVALID_PARAMS", "chat_session_list archived must be a boolean");
      }
      archived = params.at("archived").get<bool>();
    }
    int limit = 50;
    if (params.contains("limit") && !valid_integer_range(params.at("limit"), 1, 100, limit)) {
      return failure("INVALID_PARAMS", "chat_session_list limit must be between 1 and 100");
    }
    int offset = 0;
    if (params.contains("offset") && !valid_integer_range(params.at("offset"), 0, 10'000, offset)) {
      return failure("INVALID_PARAMS", "chat_session_list offset must be between 0 and 10000");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->list_chat_sessions(query, archived, limit, offset)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not list sessions");
    }
  }

  if (method == "chat_session_get") {
    if (!only_fields(params, {"sessionId"}, {"sessionId"})) {
      return failure("INVALID_PARAMS", "chat_session_get requires a sessionId");
    }
    std::string session_id;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id)) {
      return failure("INVALID_PARAMS", "chat_session_get sessionId is invalid");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->get_chat_session(session_id)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not read the session");
    }
  }

  if (method == "chat_session_create") {
    if (!only_fields(params, {"sessionId", "workspaceId", "serverId", "title"}, {"title"})) {
      return failure("INVALID_PARAMS", "chat_session_create received invalid parameters");
    }
    std::string title;
    if (!valid_request_text(params, "title", 200, true, title)) {
      return failure("INVALID_PARAMS", "chat_session_create title must be 1 to 200 characters");
    }
    std::optional<std::string> session_id;
    if (params.contains("sessionId")) {
      std::string value;
      if (!valid_request_text(params, "sessionId", 256, true, value) || !valid_opaque_id(value)) {
        return failure("INVALID_PARAMS", "chat_session_create sessionId is invalid");
      }
      session_id = std::move(value);
    }
    std::optional<std::string> workspace_id;
    if (params.contains("workspaceId")) {
      std::string value;
      if (!valid_request_text(params, "workspaceId", 256, true, value)) {
        return failure("INVALID_PARAMS", "chat_session_create workspaceId is invalid");
      }
      workspace_id = std::move(value);
    }
    std::optional<std::string> server_id;
    if (params.contains("serverId")) {
      if (!params.at("serverId").is_string()) {
        return failure("INVALID_PARAMS", "chat_session_create serverId must be a string");
      }
      auto value = trim_ascii(params.at("serverId").get<std::string>());
      if (!valid_server_id(value)) {
        return failure("INVALID_PARAMS", "chat_session_create serverId is invalid");
      }
      server_id = std::move(value);
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->create_chat_session(
                        session_id.value_or(next_id("ses")), workspace_id, server_id,
                        title, iso8601_now())};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not create the session");
    }
  }

  if (method == "chat_message_append") {
    if (!only_fields(params,
                     {"sessionId", "messageId", "role", "content", "parts", "traceId", "createdAt"},
                     {"sessionId", "role", "content"})) {
      return failure("INVALID_PARAMS", "chat_message_append received invalid parameters");
    }
    std::string session_id;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id)) {
      return failure("INVALID_PARAMS", "chat_message_append sessionId is invalid");
    }
    std::string role;
    if (!valid_request_text(params, "role", 16, true, role) ||
        !is_one_of(role, {"user", "assistant", "tool", "system"})) {
      return failure("INVALID_PARAMS", "chat_message_append role is invalid");
    }
    if (!params.at("content").is_string()) {
      return failure("INVALID_PARAMS", "chat_message_append content must be a string");
    }
    auto content = trim_ascii(params.at("content").get<std::string>());
    if (utf8_character_count(content) > 100'000) {
      return failure("INVALID_PARAMS", "chat_message_append content is too long");
    }
    std::optional<nlohmann::json> parts;
    if (params.contains("parts")) {
      if (!valid_chat_prompt_parts(params.at("parts"))) {
        return failure("INVALID_PARAMS", "chat_message_append prompt parts are invalid");
      }
      parts = params.at("parts");
    }
    if (content.empty() && !parts.has_value()) {
      return failure("INVALID_PARAMS", "chat_message_append requires content or prompt parts");
    }
    std::optional<std::string> message_id;
    if (params.contains("messageId")) {
      std::string value;
      if (!valid_request_text(params, "messageId", 256, true, value) || !valid_opaque_id(value)) {
        return failure("INVALID_PARAMS", "chat_message_append messageId is invalid");
      }
      message_id = std::move(value);
    }
    std::optional<std::string> trace_id;
    if (params.contains("traceId")) {
      std::string value;
      if (!valid_request_text(params, "traceId", 256, true, value)) {
        return failure("INVALID_PARAMS", "chat_message_append traceId is invalid");
      }
      trace_id = std::move(value);
    }
    std::string timestamp = iso8601_now();
    if (params.contains("createdAt") &&
        !valid_request_text(params, "createdAt", 80, true, timestamp)) {
      return failure("INVALID_PARAMS", "chat_message_append createdAt is invalid");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->append_chat_message(
                        message_id.value_or(next_id("msg")), session_id, role, content,
                        parts, trace_id, timestamp)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not append the message");
    }
  }

  if (method == "chat_session_archive") {
    if (!only_fields(params, {"sessionId", "archived"}, {"sessionId", "archived"}) ||
        !params.at("archived").is_boolean()) {
      return failure("INVALID_PARAMS", "chat_session_archive requires sessionId and archived");
    }
    std::string session_id;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id)) {
      return failure("INVALID_PARAMS", "chat_session_archive sessionId is invalid");
    }
    const auto archived = params.at("archived").get<bool>();
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->set_chat_session_archived(
                        session_id, archived ? std::optional<std::string>(iso8601_now())
                                             : std::nullopt)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not update the session");
    }
  }

  if (method == "chat_session_rename") {
    if (!only_fields(params, {"sessionId", "title"}, {"sessionId", "title"})) {
      return failure("INVALID_PARAMS", "chat_session_rename requires sessionId and title");
    }
    std::string session_id;
    std::string title;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id) || !valid_request_text(params, "title", 200, true, title)) {
      return failure("INVALID_PARAMS", "chat_session_rename parameters are invalid");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->rename_chat_session(session_id, title)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not rename the session");
    }
  }

  if (method == "chat_session_delete") {
    if (!only_fields(params, {"sessionId"}, {"sessionId"})) {
      return failure("INVALID_PARAMS", "chat_session_delete requires sessionId");
    }
    std::string session_id;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id)) {
      return failure("INVALID_PARAMS", "chat_session_delete sessionId is invalid");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->delete_chat_session(session_id)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not delete the session");
    }
  }

  if (method == "agent_spawn" || method == "agent_status" || method == "agent_kill" ||
      method == "agent_logs") {
    return agent_command(method, params);
  }

  if (method == "provider_list" || method == "provider_save" ||
      method == "provider_activate" || method == "provider_delete" ||
      method == "provider_models" || method == "provider_test") {
    return provider_command(method, params);
  }

  if (method == "agent_run_start" || method == "agent_run_stop" ||
      method == "agent_approval_respond") {
    return agent_run_command(method, params);
  }

  if (method == "mcp_server_list" || method == "mcp_server_save" ||
      method == "mcp_server_delete" || method == "mcp_server_review" ||
      method == "mcp_server_start" || method == "mcp_server_stop" ||
      method == "mcp_oauth_connect" || method == "mcp_oauth_cancel") {
    return mcp_command(method, params);
  }

  if (method == "local_file_preview") {
    if (!exact_string_fields(params, {"handleId"})) {
      return failure("INVALID_PARAMS", "local_file_preview requires exactly handleId");
    }
    const auto handle_id = params.at("handleId").get<std::string>();
    if (handle_id.empty() || handle_id.size() > 256) {
      return failure("INVALID_PARAMS", "local_file_preview handleId is invalid");
    }
    try {
      auto preview = local_paths_.read_preview(handle_id, kMaxFilePreviewBytes);
      return {true, make_file_preview(preview.name, preview.size, std::move(preview.bytes),
                                      kMaxFilePreviewBytes)};
    } catch (const std::exception& error) {
      return failure("LOCAL_FILE_UNAVAILABLE", error.what());
    }
  }

  if (method == "local_path_pick") {
    if (!exact_string_fields(params, {"kind"})) {
      return failure("INVALID_PARAMS", "local_path_pick requires exactly kind");
    }
    const auto kind = params.at("kind").get<std::string>();
    if (!is_one_of(kind, {"uploadFiles", "uploadDirectory", "downloadDirectory"})) {
      return failure("INVALID_PARAMS", "local_path_pick kind is invalid");
    }
    std::vector<std::filesystem::path> paths;
    try {
      paths = pick_local_paths(kind);
    } catch (const std::exception& error) {
      return failure("LOCAL_PATH_PICK_UNAVAILABLE", error.what());
    }
    auto registration = local_paths_.register_paths(paths);
    if (!paths.empty() && registration.handles.empty()) {
      return failure("LOCAL_PATH_REJECTED",
                     "the selected paths could not be opened; select regular files or folders");
    }
    nlohmann::json handles = nlohmann::json::array();
    for (const auto& handle : registration.handles) {
      nlohmann::json entry = {{"handleId", handle.handle_id},
                              {"name", handle.name},
                              {"kind", handle.kind}};
      if (handle.size.has_value()) entry["size"] = *handle.size;
      handles.push_back(std::move(entry));
    }
    return {true, {{"handles", std::move(handles)}}};
  }

  if (method == "file_transfer_upload" || method == "file_transfer_download" ||
      method == "file_transfer_list" || method == "file_transfer_get" ||
      method == "file_transfer_cancel" || method == "file_transfer_resolve_conflict") {
    return transfer_command(method, params);
  }

  if (method == "file_prepare_remote_drag") {
    return remote_drag_prepare_command(params);
  }

  if (method == "__events.subscribe") return subscribe(params, true);
  if (method == "__events.unsubscribe") return subscribe(params, false);

  if (method == "shell.open_url") {
    if (!exact_string_fields(params, {"url"})) {
      return failure("INVALID_PARAMS", "shell.open_url requires exactly one URL");
    }
    const auto url = params.at("url").get<std::string>();
    if (!is_external_url(url)) {
      return failure("INVALID_URL", "Only http(s) and mailto links can be opened");
    }
    if (!open_external_url(url)) {
      return failure("OPEN_FAILED", "The system could not open the external URL");
    }
    return {true, {{"opened", true}}};
  }

  return failure("NOT_IMPLEMENTED",
                 "This desktop command has not yet migrated to the C++ host: " + method);
}

IpcReply IpcRouter::transfer_command(const std::string& method,
                                     const nlohmann::json& params) {
  if (method == "file_transfer_list") {
    if (!only_fields(params, {"serverId"}, {})) {
      return failure("INVALID_PARAMS", "file_transfer_list received an unexpected parameter");
    }
    std::optional<std::string> server_id;
    if (params.contains("serverId")) {
      if (!params.at("serverId").is_string()) {
        return failure("INVALID_PARAMS", "file_transfer_list serverId must be a string");
      }
      const auto value = params.at("serverId").get<std::string>();
      if (!valid_server_id(value)) {
        return failure("INVALID_PARAMS", "file_transfer_list serverId is invalid");
      }
      server_id = value;
    }
    TransferManager* manager = nullptr;
    try {
      manager = ensure_transfer_manager();
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
    if (manager == nullptr) {
      // This build has no SFTP backend, so no live manager exists. Fall back to
      // the durable history table so the UI can still show past transfers.
      if (database_ == nullptr || !database_->is_available()) {
        return failure("STORAGE_UNAVAILABLE", "The local file transfer database is unavailable");
      }
      try {
        return {true, database_->list_file_transfers(server_id, 100)};
      } catch (...) {
        return failure("STORAGE_ERROR",
                       "The local file transfer database could not read transfers");
      }
    }
    try {
      // Retry remote staging cleanup for servers that are now connected before
      // reporting the merged job+history view.
      manager->reconcile_remote_staging();
      return {true, {{"transfers", transfer_snapshot_array(manager->list(server_id, 100))}}};
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    } catch (const std::exception& error) {
      return failure("STORAGE_ERROR", error.what());
    }
  }

  if (method == "file_transfer_get") {
    if (!exact_string_fields(params, {"transferId"})) {
      return failure("INVALID_PARAMS", "file_transfer_get requires exactly transferId");
    }
    const auto transfer_id = params.at("transferId").get<std::string>();
    if (transfer_id.empty() || transfer_id.size() > 128 ||
        has_control_characters(transfer_id)) {
      return failure("INVALID_PARAMS", "file_transfer_get transferId is invalid");
    }
    TransferManager* manager = nullptr;
    try {
      manager = ensure_transfer_manager();
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
    if (manager != nullptr) return transfer_snapshot_reply(*manager, transfer_id);
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local file transfer database is unavailable");
    }
    try {
      auto transfer = database_->get_file_transfer(transfer_id);
      if (!transfer.has_value()) {
        return failure("NOT_FOUND", "The file transfer was not found");
      }
      return {true, {{"transfer", std::move(*transfer)}}};
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local file transfer database could not read the transfer");
    }
  }

  if (method == "file_transfer_upload") {
    if (!only_fields(params, {"serverId", "remoteDirectory", "sourceHandleIds"},
                     {"serverId", "remoteDirectory", "sourceHandleIds"})) {
      return failure("INVALID_PARAMS", "file_transfer_upload received an unexpected parameter");
    }
    if (!params.at("serverId").is_string() || !params.at("remoteDirectory").is_string() ||
        !params.at("sourceHandleIds").is_array()) {
      return failure("INVALID_PARAMS", "file_transfer_upload parameters are invalid");
    }
    const auto server_id = params.at("serverId").get<std::string>();
    const auto remote_directory = params.at("remoteDirectory").get<std::string>();
    if (!valid_server_id(server_id) || !valid_transfer_remote_path(remote_directory)) {
      return failure("INVALID_PARAMS", "file_transfer_upload parameters are invalid");
    }
    std::vector<std::string> handle_ids;
    for (const auto& entry : params.at("sourceHandleIds")) {
      if (!entry.is_string()) {
        return failure("INVALID_PARAMS", "file_transfer_upload sourceHandleIds are invalid");
      }
      handle_ids.push_back(entry.get<std::string>());
    }
    if (handle_ids.empty() || handle_ids.size() > 256 ||
        !std::all_of(handle_ids.begin(), handle_ids.end(), valid_transfer_handle_id)) {
      return failure("INVALID_PARAMS", "file_transfer_upload sourceHandleIds are invalid");
    }
    std::vector<std::filesystem::path> local_paths;
    try {
      local_paths = local_paths_.take_upload_sources(handle_ids);
    } catch (const std::exception& error) {
      return failure("LOCAL_PATH_UNAVAILABLE", error.what());
    }
    TransferManager* manager = nullptr;
    try {
      manager = ensure_transfer_manager();
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
    if (manager == nullptr) {
      return failure("TRANSFER_UNAVAILABLE",
                     "File transfer is unavailable because this build has no SSH core");
    }
    try {
      const auto transfer_id = manager->start_upload(
          server_id, remote_directory, std::move(local_paths), ConflictPolicy::Ask);
      return transfer_snapshot_reply(*manager, transfer_id);
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
  }

  if (method == "file_transfer_download") {
    if (!only_fields(params, {"serverId", "remotePaths", "destinationHandleId"},
                     {"serverId", "remotePaths", "destinationHandleId"})) {
      return failure("INVALID_PARAMS", "file_transfer_download received an unexpected parameter");
    }
    if (!params.at("serverId").is_string() || !params.at("remotePaths").is_array() ||
        !params.at("destinationHandleId").is_string()) {
      return failure("INVALID_PARAMS", "file_transfer_download parameters are invalid");
    }
    const auto server_id = params.at("serverId").get<std::string>();
    if (!valid_server_id(server_id)) {
      return failure("INVALID_PARAMS", "file_transfer_download serverId is invalid");
    }
    std::vector<std::string> remote_paths;
    for (const auto& entry : params.at("remotePaths")) {
      if (!entry.is_string()) {
        return failure("INVALID_PARAMS", "file_transfer_download remotePaths are invalid");
      }
      const auto path = entry.get<std::string>();
      if (!valid_transfer_remote_path(path)) {
        return failure("INVALID_PARAMS", "file_transfer_download remotePaths are invalid");
      }
      remote_paths.push_back(path);
    }
    if (remote_paths.empty() || remote_paths.size() > 256) {
      return failure("INVALID_PARAMS", "file_transfer_download remotePaths are invalid");
    }
    const auto destination_id = params.at("destinationHandleId").get<std::string>();
    if (!valid_transfer_handle_id(destination_id)) {
      return failure("INVALID_PARAMS", "file_transfer_download destinationHandleId is invalid");
    }
    std::filesystem::path local_directory;
    try {
      local_directory = local_paths_.take_download_directory(destination_id);
    } catch (const std::exception& error) {
      return failure("LOCAL_PATH_UNAVAILABLE", error.what());
    }
    TransferManager* manager = nullptr;
    try {
      manager = ensure_transfer_manager();
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
    if (manager == nullptr) {
      return failure("TRANSFER_UNAVAILABLE",
                     "File transfer is unavailable because this build has no SSH core");
    }
    try {
      const auto transfer_id = manager->start_download(
          server_id, std::move(remote_paths), local_directory, ConflictPolicy::Ask);
      return transfer_snapshot_reply(*manager, transfer_id);
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
  }

  if (method == "file_transfer_cancel") {
    if (!exact_string_fields(params, {"transferId"})) {
      return failure("INVALID_PARAMS", "file_transfer_cancel requires exactly transferId");
    }
    const auto transfer_id = params.at("transferId").get<std::string>();
    if (transfer_id.empty() || transfer_id.size() > 128 ||
        has_control_characters(transfer_id)) {
      return failure("INVALID_PARAMS", "file_transfer_cancel transferId is invalid");
    }
    TransferManager* manager = nullptr;
    try {
      manager = ensure_transfer_manager();
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
    if (manager == nullptr) {
      if (database_ == nullptr || !database_->is_available()) {
        return failure("STORAGE_UNAVAILABLE", "The local file transfer database is unavailable");
      }
      return failure("NOT_FOUND", "The file transfer was not found");
    }
    try {
      manager->cancel(transfer_id);
      return transfer_snapshot_reply(*manager, transfer_id);
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
  }

  if (method == "file_transfer_resolve_conflict") {
    if (!only_fields(params, {"transferId", "action"}, {"transferId", "action"})) {
      return failure("INVALID_PARAMS",
                     "file_transfer_resolve_conflict received an unexpected parameter");
    }
    if (!params.at("transferId").is_string() || !params.at("action").is_object()) {
      return failure("INVALID_PARAMS", "file_transfer_resolve_conflict parameters are invalid");
    }
    const auto transfer_id = params.at("transferId").get<std::string>();
    if (transfer_id.empty() || transfer_id.size() > 128 ||
        has_control_characters(transfer_id)) {
      return failure("INVALID_PARAMS", "file_transfer_resolve_conflict transferId is invalid");
    }
    const auto& action_json = params.at("action");
    // The shared schema is a strict discriminated union: `rename` carries exactly
    // one extra `name` field and the other actions carry none.
    const bool is_rename = action_json.value("action", std::string()) == "rename";
    if (action_json.size() != (is_rename ? 2u : 1u)) {
      return failure("INVALID_PARAMS", "file_transfer_resolve_conflict action is invalid");
    }
    ConflictAction action;
    try {
      action = conflict_action_from_json(action_json);
    } catch (const std::invalid_argument& error) {
      return failure("INVALID_PARAMS", error.what());
    }
    TransferManager* manager = nullptr;
    try {
      manager = ensure_transfer_manager();
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
    if (manager == nullptr) {
      if (database_ == nullptr || !database_->is_available()) {
        return failure("STORAGE_UNAVAILABLE", "The local file transfer database is unavailable");
      }
      return failure("NOT_FOUND", "The file transfer was not found");
    }
    try {
      const auto current = manager->get(transfer_id);
      if (!current.has_value()) {
        return failure("NOT_FOUND", "The file transfer was not found");
      }
      if (!current->active_conflict.has_value()) {
        return failure("INVALID_STATE", "The transfer is not waiting for a conflict decision");
      }
      manager->resolve_conflict(transfer_id, current->active_conflict->item_index, action);
      return transfer_snapshot_reply(*manager, transfer_id);
    } catch (const TransferError& error) {
      return transfer_error_reply(error);
    }
  }

  return failure("NOT_IMPLEMENTED",
                 "This desktop command has not yet migrated to the C++ host: " + method);
}

IpcReply IpcRouter::remote_drag_prepare_command(const nlohmann::json& params) {
  if (!only_fields(params, {"serverId", "path"}, {"serverId", "path"})) {
    return failure("INVALID_PARAMS",
                   "file_prepare_remote_drag received an unexpected parameter");
  }
  if (!params.at("serverId").is_string() || !params.at("path").is_string()) {
    return failure("INVALID_PARAMS", "file_prepare_remote_drag parameters are invalid");
  }
  const auto server_id = params.at("serverId").get<std::string>();
  const auto path = params.at("path").get<std::string>();
  if (!valid_server_id(server_id)) {
    return failure("INVALID_PARAMS", "file_prepare_remote_drag serverId is invalid");
  }
  if (!valid_remote_drag_path(path)) {
    return failure("INVALID_PARAMS", "remote drag path must be a safe absolute file path");
  }
  const auto name = remote_drag_file_name(path);
  if (!name.has_value()) {
    return failure("INVALID_PARAMS", "remote file path has no name");
  }

  // The host serializes preparation: IPC dispatch runs on the window thread, and a
  // single download-to-stage at a time keeps the private staging root bounded.
  std::lock_guard prepare_lock(prepare_mutex_);

  RemoteDragStore* store = nullptr;
  try {
    store = ensure_remote_drag_store();
  } catch (const std::exception& error) {
    return failure("STORAGE_ERROR", error.what());
  }
  if (store == nullptr) {
    return failure("STORAGE_UNAVAILABLE", "The native drag staging location is unavailable");
  }

  const auto probe = ensure_remote_drag_probe();
  if (!probe) {
    return failure("TRANSFER_UNAVAILABLE",
                   "the selected target is not supported by the C++ SSH service");
  }
  std::optional<RemoteStat> stat;
  try {
    stat = probe->lstat(server_id, path);
  } catch (const TransferError& error) {
    return transfer_error_reply(error);
  }
  if (!stat.has_value()) {
    return failure("NOT_FOUND", "remote file no longer exists");
  }
  if (stat->kind != RemoteEntryKind::File) {
    return failure("INVALID_PARAMS",
                   "only regular remote files can be dragged to the desktop");
  }
  if (stat->size > RemoteDragStore::kMaxFileBytes) {
    return failure("INVALID_PARAMS",
                   "remote files larger than 8 GiB cannot be prepared for native drag-out");
  }

  TransferManager* manager = nullptr;
  try {
    manager = ensure_transfer_manager();
  } catch (const TransferError& error) {
    return transfer_error_reply(error);
  }
  if (manager == nullptr) {
    return failure("TRANSFER_UNAVAILABLE",
                   "File transfer is unavailable because this build has no SSH core");
  }

  std::chrono::milliseconds download_timeout;
  std::chrono::milliseconds cancel_grace;
  {
    std::lock_guard lock(drag_mutex_);
    download_timeout = remote_drag_timeout_;
    cancel_grace = remote_drag_cancel_grace_;
  }

  std::filesystem::path directory;
  try {
    directory = store->create_stage();
  } catch (const std::exception& error) {
    return failure("STORAGE_ERROR", error.what());
  }
  // Owns the private stage until the ticket is registered (or `keep()` pins it for
  // the next startup sweep when cancellation cannot be observed).
  StagedRemoteDrag staging(directory, {});

  std::string transfer_id;
  try {
    transfer_id = manager->start_download(server_id, {path}, directory, ConflictPolicy::Ask);
  } catch (const TransferError& error) {
    return transfer_error_reply(error);
  }

  auto terminal = wait_for_transfer_terminal(*manager, transfer_id, download_timeout);
  if (!terminal.has_value()) {
    // A hung backend must not race the stage cleanup: cancel and observe the stop,
    // otherwise leave the directory for the next startup sweep.
    try {
      manager->cancel(transfer_id);
    } catch (const TransferError&) {
    }
    if (!wait_for_transfer_terminal(*manager, transfer_id, cancel_grace).has_value()) {
      staging.keep();
    }
    return failure("TRANSFER_FAILED",
                   "could not prepare remote file for native drag: the download did not finish");
  }
  if (terminal->status != TransferStatus::Completed || terminal->completed_files != 1) {
    std::string detail = "download did not complete successfully";
    if (!terminal->failures.empty() && !terminal->failures.front().message.empty()) {
      detail = terminal->failures.front().message;
    }
    return failure("TRANSFER_FAILED",
                   "could not prepare remote file for native drag: " + detail);
  }

  std::vector<std::filesystem::path> files;
  try {
    files = collect_staged_regular_files(directory);
  } catch (const std::exception& error) {
    return failure("TRANSFER_FAILED", error.what());
  }
  if (files.size() != 1) {
    return failure("TRANSFER_FAILED",
                   "remote download did not produce exactly one regular file");
  }
  const auto& staged_file = files.front();
  std::error_code status_error;
  const auto metadata = std::filesystem::symlink_status(staged_file, status_error);
  if (status_error || std::filesystem::is_symlink(metadata) ||
      !std::filesystem::is_regular_file(metadata)) {
    return failure("TRANSFER_FAILED", "staged remote file failed its local type or size check");
  }
  std::error_code size_error;
  const auto staged_size = std::filesystem::file_size(staged_file, size_error);
  if (size_error || staged_size != stat->size) {
    return failure("TRANSFER_FAILED", "staged remote file failed its local type or size check");
  }
  std::error_code canonical_error;
  const auto canonical_root = std::filesystem::canonical(directory, canonical_error);
  const auto canonical_file = std::filesystem::canonical(staged_file, canonical_error);
  if (canonical_error || !path_is_within(canonical_file, canonical_root)) {
    return failure("TRANSFER_FAILED", "staged file escaped the private drag directory");
  }

  PreparedRemoteDrag prepared;
  try {
    prepared = store->insert(directory, canonical_file, *name, staged_size);
  } catch (const std::exception& error) {
    return failure("STORAGE_ERROR", error.what());
  }
  staging.release();
  return {true,
          {{"dragId", std::move(prepared.drag_id)},
           {"name", std::move(prepared.name)},
           {"size", prepared.size}}};
}

IpcReply IpcRouter::server_command(const std::string& method,
                                   const nlohmann::json& params) {
  if (database_ == nullptr) {
    return failure("STORAGE_UNAVAILABLE", "The local server database is unavailable");
  }
  if (!database_->is_available()) {
    return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
  }

  if (method == "server_delete") {
    if (!exact_string_fields(params, {"serverId"})) {
      return failure("INVALID_PARAMS", "server_delete requires exactly serverId");
    }
    const auto server_id = params.at("serverId").get<std::string>();
    if (!valid_server_id(server_id)) {
      return failure("INVALID_PARAMS", "server_delete serverId is invalid");
    }
    std::optional<std::string> identity_id;
    try {
      identity_id = database_->delete_server(server_id, next_id("act"), iso8601_now());
    } catch (const std::exception& error) {
      if (std::string_view(error.what()) == "Server was not found") {
        return failure("NOT_FOUND", "The server was not found");
      }
      return failure("STORAGE_ERROR", "The local server database could not delete the server");
    }
    if (identity_id.has_value() && !identity_id->empty()) {
      try {
        const auto references = database_->delete_identity_if_unshared(*identity_id, server_id);
        std::string cleanup_error;
        if (!clean_credential_references(database_, *credentials_, references, cleanup_error)) {
          return failure("STORAGE_ERROR", cleanup_error);
        }
      } catch (...) {
        return failure("STORAGE_ERROR", "The server was deleted but its identity could not be reclaimed");
      }
    }
    return {true, {{"deleted", true}}};
  }

  nlohmann::json normalized;
  IpcSecretParamScrubber normalized_scrubber(normalized);
  if (!normalize_server_input(method, params, normalized)) {
    return failure("INVALID_PARAMS", method + " parameters do not match the server contract");
  }

  const bool add = method == "server_add";
  const auto timestamp = iso8601_now();
  const auto server_id = add ? next_id("srv") : normalized.at("serverId").get<std::string>();
  nlohmann::json existing_server;
  if (!add) {
    try {
      const auto listed = database_->list_servers();
      const auto& servers = listed.at("servers");
      const auto found = std::find_if(servers.begin(), servers.end(), [&server_id](const auto& server) {
        return server.is_object() && server.value("id", std::string()) == server_id;
      });
      if (found == servers.end()) return failure("NOT_FOUND", "The server was not found");
      existing_server = *found;
    } catch (...) {
      return failure("STORAGE_ERROR", "The local server database could not read the server");
    }
  }

  std::vector<std::string> staged_references;
  std::optional<nlohmann::json> new_identity;
  std::string selected_identity_id;
  const bool authentication_present = normalized.contains("authentication");
  if (!add && !authentication_present) {
    selected_identity_id = existing_server.at("connection").value("identityId", std::string());
  } else {
    const auto& authentication = normalized.at("authentication");
    const auto auth_method = authentication.at("method").get<std::string>();
    if (auth_method == "identity") {
      selected_identity_id = authentication.at("identityId").get<std::string>();
      try {
        if (!database_->get_identity(selected_identity_id).has_value()) {
          return failure("NOT_FOUND", "The selected SSH identity was not found");
        }
      } catch (...) {
        return failure("STORAGE_ERROR", "The local identity database could not read the identity");
      }
    } else {
      const auto credential_account = add ? server_id : server_id + "-" + next_id("cred");
      const auto identity_id = next_id("idn");
      std::string credential_ref;
      std::optional<std::string> passphrase_ref;
      try {
        auto store_secret = [this, &staged_references](const std::string& account,
                                                      const std::string& value) {
          Secret secret{std::string_view(value)};
          auto reference = credentials_->set("ssh", account, secret).to_string();
          staged_references.push_back(reference);
          return reference;
        };

        if (auth_method == "password") {
          credential_ref = store_secret(
              credential_account,
              authentication.at("password").get_ref<const std::string&>());
        } else if (auth_method == "privateKey" || auth_method == "certificate") {
          credential_ref = store_secret(
              credential_account,
              authentication.at("privateKeyPem").get_ref<const std::string&>());
          if (authentication.contains("passphrase")) {
            const auto& passphrase =
                authentication.at("passphrase").get_ref<const std::string&>();
            const bool meaningful = std::any_of(
                passphrase.begin(), passphrase.end(), [](unsigned char character) {
                  return !std::isspace(character);
                });
            if (meaningful) {
              passphrase_ref = store_secret(credential_account + "-passphrase", passphrase);
            }
          }
        }

        nlohmann::json identity = {
            {"id", identity_id},
            {"label", normalized.at("name").get<std::string>() + " (" + server_id + ")"},
            {"method", auth_method},
            {"credentialRef", credential_ref},
            {"createdAt", timestamp},
        };
        if (passphrase_ref.has_value()) identity["passphraseRef"] = *passphrase_ref;
        if (auth_method == "certificate") {
          identity["certificatePath"] = trim_ascii(
              authentication.at("certificatePath").get<std::string>());
          if (authentication.contains("privateKeyPath")) {
            identity["privateKeyPath"] = trim_ascii(
                authentication.at("privateKeyPath").get<std::string>());
          }
        }
        new_identity = std::move(identity);
        selected_identity_id = identity_id;
      } catch (const std::exception&) {
        std::string cleanup_error;
        const bool cleaned = clean_credential_references(
            database_, *credentials_, staged_references, cleanup_error);
        return failure("CREDENTIAL_ERROR", cleaned
            ? "The operating system credential store could not save the SSH identity"
            : "The SSH identity could not be saved or queued for cleanup");
      }
    }
  }

  const auto old_identity_id = add
      ? std::string()
      : existing_server.at("connection").value("identityId", std::string());
  nlohmann::json server;
  if (add) {
    server = {
        {"id", server_id},
        {"name", normalized.at("name")},
        {"connection", {{"host", normalized.at("host")},
                         {"port", normalized.at("port")},
                         {"username", normalized.at("username")}}},
        {"capabilities", nlohmann::json::object()},
        {"status", "disconnected"},
        {"metadata", {{"environment", normalized.at("environment")}}},
        {"createdAt", timestamp},
        {"updatedAt", timestamp},
    };
  } else {
    server = std::move(existing_server);
    server["name"] = normalized.at("name");
    server["connection"]["host"] = normalized.at("host");
    server["connection"]["port"] = normalized.at("port");
    server["connection"]["username"] = normalized.at("username");
    server["metadata"]["environment"] = normalized.at("environment");
    server["status"] = "disconnected";
    server["updatedAt"] = timestamp;
    if (!normalized.contains("groupId")) server.erase("groupId");
  }
  if (normalized.contains("groupId")) server["groupId"] = normalized.at("groupId");
  auto& connection = server["connection"];
  if (selected_identity_id.empty()) connection.erase("identityId");
  else connection["identityId"] = selected_identity_id;
  if (normalized.contains("hostCertificateAuthority")) {
    connection["hostCertificateAuthority"] = normalized.at("hostCertificateAuthority");
  } else if (normalized.value("clearHostCertificateAuthority", false)) {
    connection.erase("hostCertificateAuthority");
  }

  try {
    if (add) database_->add_server(server, new_identity, next_id("act"));
    else (void)database_->update_server(server, new_identity, next_id("act"));
  } catch (const std::exception&) {
    std::string cleanup_error;
    const bool cleaned = clean_credential_references(
        database_, *credentials_, staged_references, cleanup_error);
    return failure("STORAGE_ERROR", cleaned
        ? "The local database could not save the server"
        : "The server was not saved and staged credentials could not be queued for cleanup");
  }

  if (!add && authentication_present) {
    if (!old_identity_id.empty() && old_identity_id != selected_identity_id) {
      try {
        const auto references = database_->delete_identity_if_unshared(old_identity_id, server_id);
        std::string cleanup_error;
        if (!clean_credential_references(database_, *credentials_, references, cleanup_error)) {
          return failure("STORAGE_ERROR", cleanup_error);
        }
      } catch (...) {
        return failure("STORAGE_ERROR", "The server was updated but the previous identity could not be reclaimed");
      }
    }
  }
  return {true, {{"server", std::move(server)}}};
}

IpcReply IpcRouter::network_proxy_command(const std::string& method,
                                        nlohmann::json& params) {
  if (database_ == nullptr || !database_->is_available()) {
    return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
  }

  if (method == "network_proxy_get") {
    if (!params.empty()) {
      return failure("INVALID_PARAMS", "network_proxy_get does not accept parameters");
    }
    try {
      return {true, describe_network_proxy(database_->network_proxy_config(), *credentials_)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local network proxy setting could not be read");
    }
  }

  if (!only_fields(params, {"input"}, {"input"})) {
    return failure("INVALID_PARAMS", "network_proxy_save requires exactly one input object");
  }
  NetworkProxyCredentialScrubber scrubber(params);
  auto& input = params.at("input");
  if (!only_fields(input, {"mode", "credential", "clearCredential"}, {"mode"})) {
    return failure("INVALID_PARAMS", "network_proxy_save input does not match the contract");
  }
  if (!input.at("mode").is_string() ||
      !is_one_of(input.at("mode").get<std::string>(), {"direct", "system"})) {
    return failure("INVALID_PARAMS", "network_proxy_save mode must be direct or system");
  }
  const auto mode = input.at("mode").get<std::string>();

  bool clear_credential = false;
  if (input.contains("clearCredential")) {
    if (!input.at("clearCredential").is_boolean()) {
      return failure("INVALID_PARAMS", "network_proxy_save clearCredential must be a boolean");
    }
    clear_credential = input.at("clearCredential").get<bool>();
  }
  std::optional<std::string> credential;
  if (input.contains("credential")) {
    if (!input.at("credential").is_string()) {
      return failure("INVALID_PARAMS", "network_proxy_save credential must be a string");
    }
    credential = input.at("credential").get<std::string>();
  }
  if (credential.has_value() && clear_credential) {
    return failure("INVALID_PARAMS", "代理凭据与清除操作不能同时提交");
  }
  if (mode == "direct" && credential.has_value() && !credential->empty()) {
    return failure("INVALID_PARAMS", "直连模式不能提交新的代理凭据，请先选择系统代理");
  }

  DesktopNetworkProxyConfig existing;
  try {
    existing = database_->network_proxy_config();
  } catch (...) {
    return failure("STORAGE_ERROR", "The local network proxy setting could not be read");
  }

  std::optional<std::string> staged_reference;
  std::optional<std::string> credential_ref;
  if (clear_credential) {
    credential_ref = std::nullopt;
  } else if (credential.has_value() && !credential->empty()) {
    std::string reason;
    if (!valid_proxy_credential(*credential, reason)) {
      return failure("INVALID_PARAMS", "代理凭据无法使用：" + reason);
    }
    try {
      const auto account = "network-proxy-" + next_id("credential");
      const auto reference = credentials_->set(
          "mcp", account, Secret{std::string_view(*credential)}).to_string();
      staged_reference = reference;
      credential_ref = reference;
    } catch (const std::exception&) {
      return failure("CREDENTIAL_ERROR",
                     "The operating system credential store could not save the proxy credential");
    }
  } else {
    credential_ref = existing.credential_ref;
  }

  DesktopNetworkProxyConfig config{mode, credential_ref};
  try {
    database_->save_network_proxy_config(config);
  } catch (const std::exception&) {
    if (staged_reference) {
      std::string ignored_error;
      (void)clean_credential_references(database_, *credentials_, {*staged_reference}, ignored_error);
    }
    return failure("STORAGE_ERROR", "The local network proxy setting could not be saved");
  }

  // A previous credential is only reclaimed once nothing points at it any more.
  if (existing.credential_ref && credential_ref != existing.credential_ref) {
    try {
      const auto previous = CredentialReference::parse(*existing.credential_ref);
      std::string ignored_error;
      (void)clean_credential_references(database_, *credentials_, {previous.to_string()}, ignored_error);
    } catch (const std::exception&) {
      // Leave a malformed reference for startup reconciliation; the save already succeeded.
    }
  }

  try {
    return {true, describe_network_proxy(database_->network_proxy_config(), *credentials_)};
  } catch (...) {
    return failure("STORAGE_ERROR", "The local network proxy setting could not be read back");
  }
}

IpcReply IpcRouter::host_key_command(const std::string& method,
                                     const nlohmann::json& params) {
  if (database_ == nullptr || !database_->is_available()) {
    return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
  }
  if (!only_fields(params, {"serverId"}, {"serverId"})) {
    return failure("INVALID_PARAMS", method + " requires exactly serverId");
  }
  std::string server_id;
  if (!valid_request_text(params, "serverId", 256, true, server_id) ||
      !valid_server_id(server_id)) {
    return failure("INVALID_PARAMS", method + " serverId is invalid");
  }

  std::optional<nlohmann::json> endpoint;
  try {
    endpoint = database_->get_server_endpoint(server_id);
  } catch (...) {
    return failure("STORAGE_ERROR", "The local server database could not read the server");
  }
  if (!endpoint.has_value()) return failure("NOT_FOUND", "The server was not found");
  const auto host = endpoint->at("host").get<std::string>();
  const auto port_value = endpoint->at("port").get<int>();
  if (host.empty() || port_value < 1 || port_value > 65535) {
    return failure("STORAGE_ERROR", "The stored server endpoint is invalid");
  }
  const auto port = static_cast<std::uint16_t>(port_value);

  try {
    auto store = HostTrustStore::load(database_->data_directory() / "known_hosts");
    if (method == "server_host_key_status") {
      const auto pinned = store.pinned_fingerprint(host, port);
      nlohmann::json response = {{"host", host}, {"port", port}, {"pinned", pinned.has_value()}};
      if (pinned.has_value()) response["pinnedFingerprint"] = *pinned;
      return {true, std::move(response)};
    }
    const auto outcome = store.forget(host, port);
    return {true, {{"host", host},
                   {"port", port},
                   {"forgotten", outcome == HostKeyForgetOutcome::Removed}}};
  } catch (const std::exception&) {
    return failure("STORAGE_ERROR",
                   "The local known-hosts store could not be read or written");
  }
}

std::string IpcRouter::issue_probe_ticket(const std::string& server_id, const std::string& host,
                                          int port, const std::string& fingerprint) {
  const auto ticket = new_probe_ticket();
  const auto now = std::chrono::steady_clock::now();
  std::lock_guard lock(mutex_);
  for (auto item = probe_tickets_.begin(); item != probe_tickets_.end();) {
    if (item->second.expires_at <= now) {
      item = probe_tickets_.erase(item);
    } else {
      ++item;
    }
  }
  probe_tickets_[ticket] = ProbeTicket{server_id, host, port, fingerprint,
                                       now + std::chrono::minutes(5)};
  return ticket;
}

std::optional<std::string> IpcRouter::claim_probe_ticket(
    const std::string& ticket, const std::string& server_id, const std::string& host, int port,
    const std::string& fingerprint) {
  std::lock_guard lock(mutex_);
  const auto found = probe_tickets_.find(ticket);
  if (found == probe_tickets_.end()) {
    return "host-key probe ticket is missing or was already used; probe again before trusting";
  }
  const auto entry = found->second;
  probe_tickets_.erase(found);
  if (entry.expires_at <= std::chrono::steady_clock::now()) {
    return "host-key probe ticket expired; probe the server again before trusting";
  }
  if (entry.server_id != server_id || entry.host != host || entry.port != port) {
    return "the server endpoint changed after the probe; probe the current endpoint again";
  }
  if (entry.fingerprint != fingerprint) {
    return "the fingerprint does not match the server-issued probe ticket; probe again";
  }
  return std::nullopt;
}

IpcReply IpcRouter::server_read_command(const std::string& method,
                                        const nlohmann::json& params) {
  if (database_ == nullptr || !database_->is_available()) {
    return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
  }
  if (method.rfind("remote_file_", 0) == 0) {
    const std::set<std::string> fields = {"serverId", "path"};
    if (!only_fields(params, fields, fields) || !params.at("serverId").is_string() ||
        !params.at("path").is_string()) {
      return failure("INVALID_PARAMS", method + " requires serverId and path");
    }
    const auto server_id = params.at("serverId").get<std::string>();
    const auto path = params.at("path").get<std::string>();
    if (!valid_server_id(server_id)) {
      return failure("INVALID_PARAMS", method + " serverId is invalid");
    }
    try {
      if (method == "remote_file_list") {
        return {true, run_remote_file_list(*database_, *credentials_, server_id, path, 20)};
      }
      if (method == "remote_file_read") {
        return {true, run_remote_file_read(*database_, *credentials_, server_id, path, 20)};
      }
      return {true, run_remote_file_preview(*database_, *credentials_, server_id, path, 20)};
    } catch (const std::exception& error) {
      return failure("SSH_ERROR", error.what());
    }
  }
  if (method == "server_host_key_probe" || method == "server_host_key_trust") {
    const bool trust = method == "server_host_key_trust";
    const std::set<std::string> fields =
        trust ? std::set<std::string>{"serverId", "probeTicket", "fingerprint"}
              : std::set<std::string>{"serverId"};
    if (!only_fields(params, fields, fields)) {
      return failure("INVALID_PARAMS", method + " parameters are invalid");
    }
    if (!params.at("serverId").is_string()) {
      return failure("INVALID_PARAMS", method + " serverId must be a string");
    }
    const auto server_id = params.at("serverId").get<std::string>();
    if (!valid_server_id(server_id)) {
      return failure("INVALID_PARAMS", method + " serverId is invalid");
    }
    std::optional<nlohmann::json> endpoint;
    try {
      endpoint = database_->get_server_endpoint(server_id);
    } catch (...) {
      return failure("STORAGE_ERROR", "The local server database could not read the server");
    }
    if (!endpoint.has_value()) return failure("NOT_FOUND", "The server was not found");
    const auto host = endpoint->at("host").get<std::string>();
    const auto port = endpoint->at("port").get<int>();
    if (host.empty() || port < 1 || port > 65535) {
      return failure("STORAGE_ERROR", "The stored server endpoint is invalid");
    }

    if (method == "server_host_key_probe") {
      SshCommandResult probe;
      try {
        probe = run_host_key_probe(*database_, *credentials_, server_id, 15);
      } catch (const std::exception& error) {
        return failure("SSH_UNAVAILABLE", error.what());
      }
      if (!probe.error_code.empty()) {
        return failure("SSH_ERROR",
                       probe.error_message.empty() ? probe.error_code : probe.error_message);
      }
      const auto fingerprint = probe.stdout_text;
      if (!valid_host_key_fingerprint(fingerprint)) {
        return failure("SSH_ERROR",
                       "The SSH service returned an invalid host-key fingerprint");
      }
      HostKeyCheck check;
      try {
        auto store = HostTrustStore::load(database_->data_directory() / "known_hosts");
        check = store.check(host, static_cast<std::uint16_t>(port), fingerprint);
      } catch (...) {
        return failure("STORAGE_ERROR", "The local known-hosts store could not be read");
      }
      const auto ticket = issue_probe_ticket(server_id, host, port, fingerprint);
      nlohmann::json response = {{"host", host},
                                 {"port", port},
                                 {"probeTicket", ticket},
                                 {"presentedFingerprint", fingerprint},
                                 {"comparison", comparison_word(check.comparison)}};
      if (check.pinned.has_value()) response["pinnedFingerprint"] = *check.pinned;
      return {true, std::move(response)};
    }

    // server_host_key_trust
    if (!params.at("probeTicket").is_string() || !params.at("fingerprint").is_string()) {
      return failure("INVALID_PARAMS", "server_host_key_trust parameters are invalid");
    }
    const auto ticket = params.at("probeTicket").get<std::string>();
    const auto fingerprint = params.at("fingerprint").get<std::string>();
    if (ticket.empty() || ticket.size() > 256 || !valid_host_key_fingerprint(fingerprint)) {
      return failure("INVALID_PARAMS", "server_host_key_trust parameters are invalid");
    }
    const auto claim_error = claim_probe_ticket(ticket, server_id, host, port, fingerprint);
    if (claim_error.has_value()) return failure("INVALID_PARAMS", *claim_error);
    try {
      auto store = HostTrustStore::load(database_->data_directory() / "known_hosts");
      const auto existing = store.pinned_fingerprint(host, static_cast<std::uint16_t>(port));
      if (existing.has_value() && *existing != fingerprint) {
        return failure("HOST_KEY_MISMATCH",
                       "This host is already pinned to a different fingerprint; forget it first, "
                       "then probe and confirm again");
      }
      const bool already_pinned = existing.has_value();
      if (!already_pinned) store.pin(host, static_cast<std::uint16_t>(port), fingerprint);
      return {true, {{"host", host},
                     {"port", port},
                     {"fingerprint", fingerprint},
                     {"alreadyPinned", already_pinned}}};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local known-hosts store could not be written");
    }
  }

  if (!only_fields(params, {"serverId"}, {"serverId"}) || !params.at("serverId").is_string()) {
    return failure("INVALID_PARAMS", method + " requires exactly serverId");
  }
  const auto server_id = params.at("serverId").get<std::string>();
  if (!valid_server_id(server_id)) {
    return failure("INVALID_PARAMS", method + " serverId is invalid");
  }

  if (method == "server_logs" || method == "server_services") {
    const bool logs = method == "server_logs";
    SshCommandResult raw;
    try {
      raw = logs ? run_server_logs(*database_, *credentials_, server_id, std::nullopt, "", 20)
                 : run_service_inventory(*database_, *credentials_, server_id, 20);
    } catch (const std::exception& error) {
      return failure("SSH_UNAVAILABLE", error.what());
    }
    if (!raw.error_code.empty()) {
      return failure("SSH_ERROR", raw.error_message.empty() ? raw.error_code : raw.error_message);
    }
    try {
      auto& client = ensure_sidecar();
      client.ensure_started(std::chrono::milliseconds(60000));
      const auto normalized = client.request(
          logs ? "server.logs.normalize" : "server.services.normalize",
          {{"stdout", raw.stdout_text}, {"arguments", nlohmann::json::object()}},
          std::chrono::milliseconds(15000));
      if (!normalized.is_object() || normalized.value("ok", false) != true) {
        return failure(
            "NORMALIZE_ERROR",
            normalized.value("message",
                             std::string("The Agent could not parse the remote output")));
      }
      if (!normalized.contains("output")) {
        return failure("NORMALIZE_ERROR", "The Agent returned no normalized output");
      }
      return {true, normalized.at("output")};
    } catch (const SidecarRpcError& error) {
      return failure(sidecar_error_code(error.code()), error.what());
    } catch (const SidecarError& error) {
      return failure("AGENT_UNAVAILABLE", error.what());
    } catch (const std::exception& error) {
      return failure("NORMALIZE_ERROR", error.what());
    }
  }

  // server_snapshot: run the fixed collector batch, normalize it through Python,
  // then persist and return the UI contract.
  nlohmann::json outputs;
  try {
    outputs = run_system_snapshot(*database_, *credentials_, server_id, 25);
  } catch (const std::exception& error) {
    return failure("SSH_UNAVAILABLE", error.what());
  }
  nlohmann::json snapshot_data;
  try {
    auto& client = ensure_sidecar();
    client.ensure_started(std::chrono::milliseconds(60000));
    const auto normalized = client.request("server.snapshot.normalize", {{"outputs", outputs}},
                                           std::chrono::milliseconds(15000));
    if (!normalized.is_object() || !normalized.contains("snapshotData") ||
        !normalized.at("snapshotData").is_object()) {
      return failure("NORMALIZE_ERROR", "The Agent returned invalid snapshot data");
    }
    snapshot_data = normalized.at("snapshotData");
  } catch (const SidecarRpcError& error) {
    return failure(sidecar_error_code(error.code()), error.what());
  } catch (const SidecarError& error) {
    return failure("AGENT_UNAVAILABLE", error.what());
  } catch (const std::exception& error) {
    return failure("NORMALIZE_ERROR", error.what());
  }
  const auto health = snapshot_data.value("health", std::string());
  if (!is_one_of(health, {"healthy", "warning", "critical", "unknown"}) ||
      !snapshot_data.contains("capabilities") || !snapshot_data.contains("collectors")) {
    return failure("NORMALIZE_ERROR", "The Agent returned invalid snapshot data");
  }
  nlohmann::json snapshot = snapshot_data;
  const auto epoch_seconds = std::chrono::duration_cast<std::chrono::seconds>(
                                 std::chrono::system_clock::now().time_since_epoch())
                                 .count();
  snapshot["id"] = "snap_" + server_id + "_" + std::to_string(epoch_seconds);
  snapshot["serverId"] = server_id;
  snapshot["collectedAt"] = iso8601_now();
  try {
    database_->insert_snapshot(snapshot);
  } catch (...) {
    return failure("STORAGE_ERROR", "The local snapshot database could not save the snapshot");
  }
  return {true, {{"snapshot", std::move(snapshot)}}};
}

IpcReply IpcRouter::connection_command(const std::string& method,
                                       const nlohmann::json& params) {
  if (database_ == nullptr || !database_->is_available()) {
    return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
  }

  if (method == "server_auth_respond" || method == "server_auth_cancel") {
    // Keyboard-interactive prompts are owned by the C++ PTY worker. Until a PTY
    // registers a challenge there is nothing to accept, which is the correct
    // answer for an idle desktop.
    const bool respond = method == "server_auth_respond";
    const std::set<std::string> fields =
        respond ? std::set<std::string>{"authId", "responses"}
                : std::set<std::string>{"authId"};
    if (!only_fields(params, fields, fields) || !params.at("authId").is_string()) {
      return failure("INVALID_PARAMS", method + " parameters are invalid");
    }
    const auto auth_id = trim_ascii(params.at("authId").get<std::string>());
    if (auth_id.empty() || utf8_character_count(auth_id) > 256) {
      return failure("INVALID_PARAMS", "authentication challenge id is invalid");
    }
    if (respond) {
      const auto& responses = params.at("responses");
      if (!responses.is_array() || responses.size() > 16) {
        return failure("INVALID_PARAMS", "authentication responses are invalid");
      }
      for (const auto& response : responses) {
        if (!response.is_string() || response.get<std::string>().size() > 4096) {
          return failure("INVALID_PARAMS", "authentication responses are invalid");
        }
      }
    }
    return {true, {{"accepted", false}}};
  }

  if (!only_fields(params, {"serverId"}, {"serverId"}) || !params.at("serverId").is_string()) {
    return failure("INVALID_PARAMS", method + " requires exactly serverId");
  }
  const auto server_id = params.at("serverId").get<std::string>();
  if (!valid_server_id(server_id)) {
    return failure("INVALID_PARAMS", method + " serverId is invalid");
  }

  if (method == "server_disconnect") {
    // Idempotent: a stale UI may ask to disconnect twice.
    try {
      const auto now = iso8601_now();
      database_->set_server_status(server_id, "disconnected", now);
      database_->record_connection_activity(next_id("act"), server_id, "已断开服务器",
                                            std::nullopt, "success", now);
    } catch (...) {
      return failure("STORAGE_ERROR",
                     "The local server database could not update the connection state");
    }
    return {true, nlohmann::json::object()};
  }

  SshCommandResult result;
  try {
    result = run_connection_check(*database_, *credentials_, server_id, 20);
  } catch (const std::exception& error) {
    return failure("SSH_UNAVAILABLE", error.what());
  }
  const auto now = iso8601_now();
  if (!result.error_code.empty()) {
    const auto message = result.error_message.empty() ? result.error_code : result.error_message;
    try {
      database_->set_server_status(server_id, "error", now);
      database_->record_connection_activity(next_id("act"), server_id, "连接服务器失败",
                                            message, "failure", now);
    } catch (...) {
      // The connection error is the answer the user needs; a failed status write
      // must not replace it.
    }
    return failure("SSH_ERROR", message);
  }
  try {
    database_->set_server_status(server_id, "connected", now);
    database_->record_connection_activity(next_id("act"), server_id, "已连接服务器", std::nullopt,
                                          "success", now);
  } catch (...) {
    return failure("STORAGE_ERROR",
                   "The local server database could not update the connection state");
  }
  return {true, {{"status", "connected"}}};
}

IpcReply IpcRouter::terminal_command(const std::string& method,
                                     const nlohmann::json& params) {
  if (database_ == nullptr || !database_->is_available()) {
    return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
  }
  if (terminals_ == nullptr) {
    return failure("TERMINAL_UNAVAILABLE", "The SSH terminal manager is unavailable");
  }
  if (method == "terminal_open") {
    if (!only_fields(params, {"serverId", "cols", "rows"}, {"serverId", "cols", "rows"}) ||
        !params.at("serverId").is_string() || !params.at("cols").is_number_integer() ||
        !params.at("rows").is_number_integer()) {
      return failure("INVALID_PARAMS", "terminal_open parameters are invalid");
    }
    const auto server_id = params.at("serverId").get<std::string>();
    const auto cols = params.at("cols").get<int>();
    const auto rows = params.at("rows").get<int>();
    if (!valid_server_id(server_id) || cols < 1 || cols > 1000 || rows < 1 || rows > 1000) {
      return failure("INVALID_PARAMS", "terminal_open parameters are invalid");
    }
    try {
      return {true, {{"terminalSessionId", terminals_->open(server_id, cols, rows)}}};
    } catch (const std::exception& error) {
      return failure("TERMINAL_ERROR", error.what());
    }
  }

  const bool write = method == "terminal_write";
  const bool resize = method == "terminal_resize";
  const std::set<std::string> fields =
      write ? std::set<std::string>{"terminalSessionId", "data"}
            : resize ? std::set<std::string>{"terminalSessionId", "cols", "rows"}
                     : std::set<std::string>{"terminalSessionId"};
  if (!only_fields(params, fields, fields) || !params.at("terminalSessionId").is_string()) {
    return failure("INVALID_PARAMS", method + " parameters are invalid");
  }
  const auto session_id = params.at("terminalSessionId").get<std::string>();
  if (session_id.empty() || session_id.size() > 256) {
    return failure("INVALID_PARAMS", method + " session id is invalid");
  }

  if (write) {
    if (!params.at("data").is_string()) {
      return failure("INVALID_PARAMS", "terminal_write data is invalid");
    }
    const auto data = params.at("data").get<std::string>();
    if (data.size() > 64 * 1024) {
      return failure("INVALID_PARAMS", "terminal_write data is too large");
    }
    if (!terminals_->write(session_id, data)) {
      return failure("TERMINAL_NOT_FOUND", "The terminal session is not available");
    }
    return {true, nlohmann::json::object()};
  }
  if (resize) {
    if (!params.at("cols").is_number_integer() || !params.at("rows").is_number_integer()) {
      return failure("INVALID_PARAMS", "terminal_resize dimensions are invalid");
    }
    const auto cols = params.at("cols").get<int>();
    const auto rows = params.at("rows").get<int>();
    if (cols < 1 || cols > 1000 || rows < 1 || rows > 1000) {
      return failure("INVALID_PARAMS", "terminal_resize dimensions are invalid");
    }
    if (!terminals_->resize(session_id, cols, rows)) {
      return failure("TERMINAL_NOT_FOUND", "The terminal session is not available");
    }
    return {true, nlohmann::json::object()};
  }
  (void)terminals_->close(session_id);
  return {true, nlohmann::json::object()};
}

void IpcRouter::set_event_sink(
    std::function<void(const std::string&, const nlohmann::json&)> sink) {
  std::lock_guard lock(mutex_);
  event_sink_ = std::move(sink);
}

void IpcRouter::configure_sidecar() {
  SidecarLaunch launch;
  if (const auto entry = read_environment("YUKINAL_AGENT_ENTRY");
      entry.has_value() && !entry->empty()) {
    launch.program = *entry;
  } else {
    auto python = read_environment("YUKINAL_AGENT_PYTHON");
    if (!python.has_value() || python->empty()) python = read_environment("YUKINAL_PYTHON");
    if (!python.has_value() || python->empty()) python = std::string("python");
    launch.program = *python;
    launch.args = {"-m", "yukinal_agent.sidecar"};
  }
  if (const auto source = read_environment("YUKINAL_AGENT_SOURCE");
      source.has_value() && !source->empty()) {
    launch.environment.push_back({"PYTHONPATH", *source});
  }
  for (const auto* name : {"YUKINAL_SSH_API_TOKEN", "YUKINAL_SSH_API_URL",
                           "YUKINAL_SSH_SERVICE_AUTOCONFIG", "YUKINAL_SSH_SERVICE_BINARY",
                           "YUKINAL_SSH_SERVICE_CONFIG"}) {
    if (const auto value = read_environment(name); value.has_value() && !value->empty()) {
      launch.environment.push_back({name, *value});
    }
  }
  if (database_ != nullptr) {
    launch.data_directory = database_->data_directory().string();
  } else if (const auto data = read_environment("YUKINAL_DATA_DIR"); data.has_value()) {
    launch.data_directory = *data;
  }
  launch.client_version = YUKINAL_DESKTOP_VERSION;
  sidecar_->configure(std::move(launch));
  sidecar_->set_host_handler([this](const std::string& method, const nlohmann::json& params,
                                    const std::shared_ptr<std::atomic<bool>>& cancel) {
    return handle_host_request(method, params, cancel);
  });
  sidecar_->set_notification_sink([this](const std::string& method,
                                         const nlohmann::json& params) {
    // A host action approval ticket is minted here, from the raw notification,
    // before the `hostInput` it fingerprints is stripped from the projected event.
    if (method == "agent.stream" && params.is_object() &&
        params.value("type", std::string()) == "agent.waiting_approval" &&
        params.contains("approval") && params.at("approval").is_object() &&
        is_host_approval_tool_name(
            params.at("approval").value("toolName", std::string()))) {
      try {
        approvals_.record_waiting_event(params, now_epoch_seconds());
      } catch (const std::exception&) {
        // A malformed or unsupported approval simply mints no ticket; the
        // effectful action that uses it later fails closed.
      }
    }
    // `hostInput` crosses the local sidecar pipe only so the host can fingerprint
    // the exact executor input. Keep it out of UI events, like the retired host.
    nlohmann::json projected = params;
    if (projected.is_object() && projected.contains("approval") &&
        projected.at("approval").is_object()) {
      projected["approval"].erase("hostInput");
    }
    std::function<void(const std::string&, const nlohmann::json&)> sink;
    {
      std::lock_guard lock(mutex_);
      sink = event_sink_;
    }
    if (!sink) return;
    if (method == "agent.stream" && projected.is_object() && projected.contains("type") &&
        projected.at("type").is_string()) {
      sink(projected.at("type").get<std::string>(), projected);
    } else {
      sink(method, projected);
    }
  });
}

SidecarClient& IpcRouter::ensure_sidecar() {
  std::lock_guard lock(sidecar_mutex_);
  if (!sidecar_) {
    sidecar_ = std::make_unique<SidecarClient>();
    configure_sidecar();
  }
  return *sidecar_;
}

IpcReply IpcRouter::agent_command(const std::string& method, const nlohmann::json& params) {
  if (!params.empty()) {
    return failure("INVALID_PARAMS", method + " does not accept parameters");
  }
  if (method == "agent_status") {
    const SidecarStatus status = sidecar_ ? sidecar_->status() : SidecarStatus{};
    nlohmann::json response = {
        {"running", status.running},
        {"pid", status.pid.has_value() ? nlohmann::json(*status.pid) : nlohmann::json(nullptr)},
        {"protocolVersion", status.protocol_version.has_value()
                                ? nlohmann::json(*status.protocol_version)
                                : nlohmann::json(nullptr)},
        {"agentVersion", status.agent_version.has_value() ? nlohmann::json(*status.agent_version)
                                                          : nlohmann::json(nullptr)},
        {"toolCount", (status.running && status.tool_count.has_value())
                          ? nlohmann::json(*status.tool_count)
                          : nlohmann::json(nullptr)},
        {"entry", status.entry.empty() ? nlohmann::json(nullptr) : nlohmann::json(status.entry)},
        {"startedAt", status.started_at.has_value() ? nlohmann::json(*status.started_at)
                                                     : nlohmann::json(nullptr)},
        {"lastExit", nullptr},
    };
    if (status.last_exit.has_value()) {
      response["lastExit"] = {
          {"code", status.last_exit->code.has_value() ? nlohmann::json(*status.last_exit->code)
                                                       : nlohmann::json(nullptr)},
          {"signal", status.last_exit->signal.empty() ? nlohmann::json(nullptr)
                                                       : nlohmann::json(status.last_exit->signal)},
          {"at", status.last_exit->at}};
    }
    return {true, std::move(response)};
  }
  if (method == "agent_logs") {
    std::vector<std::string> lines;
    std::size_t capacity = 200;
    if (sidecar_) {
      lines = sidecar_->log_lines();
      capacity = sidecar_->log_capacity();
    }
    return {true, {{"lines", std::move(lines)}, {"capacity", capacity}}};
  }
  if (method == "agent_kill") {
    bool killed = false;
    if (sidecar_ && sidecar_->status().running) {
      sidecar_->stop();
      killed = true;
    }
    return {true, {{"killed", killed}}};
  }
  if (method == "agent_spawn") {
    try {
      auto& client = ensure_sidecar();
      const auto started = client.ensure_started(std::chrono::milliseconds(60000));
      return {true,
              {{"pid", started.pid},
               {"protocolVersion", started.protocol_version},
               {"agentVersion", started.agent_version},
               {"entry", started.entry},
               {"toolCount", started.tool_count},
               {"alreadyRunning", started.already_running}}};
    } catch (const SidecarRpcError& error) {
      return failure(sidecar_error_code(error.code()), error.what());
    } catch (const SidecarError& error) {
      return failure("AGENT_UNAVAILABLE", error.what());
    }
  }
  return failure("NOT_IMPLEMENTED", "This Agent command has not migrated yet: " + method);
}

IpcReply IpcRouter::agent_run_command(const std::string& method,
                                     const nlohmann::json& params) {
  static const std::map<std::string, std::string> kMethods = {
      {"agent_run_start", "agent.run.start"},
      {"agent_run_stop", "agent.run.stop"},
      {"agent_approval_respond", "agent.approval.respond"},
  };
  const auto found = kMethods.find(method);
  if (found == kMethods.end()) {
    return failure("NOT_IMPLEMENTED", "This Agent run command has not migrated yet: " + method);
  }
  if (!params.is_object()) {
    return failure("INVALID_PARAMS", method + " requires an object payload");
  }
  // The Python Agent Node validates every run field and owns the durable
  // admission/approval state, so the host forwards the payload unchanged. A
  // synchronous start waits for the run's bounded model/tool budget to finish,
  // while stop and approval responses answer immediately.
  const auto timeout = method == "agent_run_start"
                           ? std::chrono::minutes(15)
                           : std::chrono::seconds(30);
  if (method != "agent_approval_respond") {
    try {
      auto& client = ensure_sidecar();
      return {true, client.request(found->second, params, timeout)};
    } catch (const SidecarRpcError& error) {
      return failure(sidecar_error_code(error.code()), error.what());
    } catch (const SidecarError& error) {
      return failure("AGENT_UNAVAILABLE", error.what());
    }
  }

  // Approval responses are the one Agent command the host also mirrors into its
  // own ticket ledger: a host action ticket only becomes executable when the
  // sidecar accepted an approve-once decision. The sidecar still owns the actual
  // admission decision, so a decision the host does not track still forwards.
  std::string approval_id;
  std::string approval_run_id;
  std::string decision;
  if (!params.contains("approvalId") || !params.contains("runId") ||
      !params.contains("decision") ||
      !valid_request_text(params, "approvalId", 256, true, approval_id) ||
      !valid_request_text(params, "runId", 256, true, approval_run_id) ||
      !valid_request_text(params, "decision", 32, true, decision)) {
    return failure("INVALID_PARAMS",
                   "agent_approval_respond requires approvalId, runId and decision");
  }
  std::optional<HostApprovalLedger::ResponseToken> token;
  try {
    token = approvals_.begin_response(approval_id, approval_run_id, decision,
                                      now_epoch_seconds());
  } catch (const std::exception& error) {
    return failure("INVALID_PARAMS", error.what());
  }
  const auto finish = [this, &token](bool accepted) {
    if (!token.has_value()) return;
    try {
      approvals_.finish_response(*token, accepted, now_epoch_seconds());
    } catch (const std::exception&) {
      // The ticket left the responding state out from under us; nothing to do.
    }
  };
  try {
    auto& client = ensure_sidecar();
    const auto response = client.request(found->second, params, timeout);
    const auto accepted =
        response.is_object() ? response.value("accepted", nlohmann::json()) : nlohmann::json();
    if (!accepted.is_boolean()) {
      finish(false);
      return failure("AGENT_PROTOCOL_ERROR",
                     "the Agent returned an invalid approval response");
    }
    finish(accepted.get<bool>());
    return {true, {{"accepted", accepted.get<bool>()}}};
  } catch (const SidecarRpcError& error) {
    finish(false);
    return failure(sidecar_error_code(error.code()), error.what());
  } catch (const SidecarError& error) {
    finish(false);
    return failure("AGENT_UNAVAILABLE", error.what());
  }
}

IpcReply IpcRouter::mcp_command(const std::string& method, const nlohmann::json& params) {
  if (database_ == nullptr || !database_->is_available()) {
    return failure("STORAGE_UNAVAILABLE", "The local Yukinal database has not been initialized");
  }

  const auto view_for = [](const nlohmann::json& config) {
    nlohmann::json view = {
        {"config", config},
        {"status",
         {{"serverId", config.at("id")},
          {"running", false},
          {"toolCount", 0},
          {"stderrTail", nlohmann::json::array()},
          {"diagnostics", nlohmann::json::array()}}},
        {"tools", nlohmann::json::array()},
    };
    if (!config.value("enabled", false)) {
      view["unavailable"] = {{"code", "disabled"},
                             {"message", "This MCP server is disabled."}};
    } else if (const auto reason = oauth_config_error(config); reason.has_value()) {
      view["unavailable"] = *reason;
    }
    return view;
  };

  if (method == "mcp_server_list") {
    if (!params.empty()) {
      return failure("INVALID_PARAMS", "mcp_server_list does not accept parameters");
    }
    try {
      const auto servers = database_->list_mcp_servers().at("servers");
      nlohmann::json views = nlohmann::json::array();
      for (const auto& config : servers) views.push_back(view_for(config));
      return {true, {{"servers", std::move(views)}}};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local MCP database could not read its servers");
    }
  }

  if (method == "mcp_server_save") {
    if (!only_fields(params, {"input"}, {"input"}) || !params.at("input").is_object()) {
      return failure("INVALID_PARAMS", "mcp_server_save requires exactly one input object");
    }
    const auto& input = params.at("input");
    static const std::set<std::string> kAllowed = {
        "id", "label", "transport", "command", "args", "url", "httpAuthHeaders", "oauth",
        "enabled", "annotationTrust"};
    if (!only_fields(input, kAllowed, {"id", "label", "transport", "enabled"}) ||
        !input.at("id").is_string() || !input.at("label").is_string() ||
        !input.at("transport").is_string() || !input.at("enabled").is_boolean()) {
      return failure("INVALID_PARAMS", "mcp_server_save input does not match the contract");
    }
    const auto server_id = trim_ascii(input.at("id").get<std::string>());
    const auto label = input.at("label").get<std::string>();
    const auto transport = input.at("transport").get<std::string>();
    if (server_id.empty() || server_id.size() > 160 || has_control_characters(server_id)) {
      return failure("INVALID_PARAMS", "mcp_server_save id is invalid");
    }
    if (label.size() > 200 || has_control_characters(label)) {
      return failure("INVALID_PARAMS", "mcp_server_save label is invalid");
    }
    if (transport != "stdio" && transport != "http") {
      return failure("INVALID_PARAMS", "mcp_server_save transport must be stdio or http");
    }

    std::optional<nlohmann::json> existing;
    try {
      existing = database_->get_mcp_server(server_id);
    } catch (...) {
      return failure("STORAGE_ERROR", "The local MCP database could not read the server");
    }

    nlohmann::json config = {
        {"id", server_id},
        {"label", label},
        {"transport", transport},
        {"enabled", input.at("enabled").get<bool>()},
        {"allowedTools", existing.has_value()
                             ? existing->value("allowedTools", nlohmann::json::array())
                             : nlohmann::json::array()},
        {"trustLevel", existing.has_value()
                           ? existing->value("trustLevel", std::string("unreviewed"))
                           : std::string("unreviewed")},
    };
    if (input.contains("annotationTrust")) {
      if (!input.at("annotationTrust").is_string() ||
          !is_one_of(input.at("annotationTrust").get<std::string>(), {"none", "trusted"})) {
        return failure("INVALID_PARAMS", "mcp_server_save annotationTrust is invalid");
      }
      config["annotationTrust"] = input.at("annotationTrust");
    } else {
      config["annotationTrust"] = existing.has_value()
                                       ? existing->value("annotationTrust", std::string("none"))
                                       : std::string("none");
    }

    if (transport == "stdio") {
      if (!input.contains("command") || !input.at("command").is_string()) {
        return failure("INVALID_PARAMS", "a stdio MCP server requires command");
      }
      const auto command = input.at("command").get<std::string>();
      if (command.empty() || command.size() > 1024 || has_control_characters(command)) {
        return failure("INVALID_PARAMS", "mcp_server_save command is invalid");
      }
      config["command"] = command;
      if (input.contains("args")) {
        if (!input.at("args").is_array() || input.at("args").size() > 64) {
          return failure("INVALID_PARAMS", "mcp_server_save args are invalid");
        }
        for (const auto& argument : input.at("args")) {
          if (!argument.is_string() || argument.get<std::string>().size() > 1024) {
            return failure("INVALID_PARAMS", "mcp_server_save args are invalid");
          }
        }
        config["args"] = input.at("args");
      }
    } else {
      if (!input.contains("url") || !input.at("url").is_string()) {
        return failure("INVALID_PARAMS", "an http MCP server requires url");
      }
      const auto url = input.at("url").get<std::string>();
      const bool secure =
          url.size() > 8 && url.rfind("https://", 0) == 0 && url.size() <= 2048 &&
          !has_control_characters(url) && url.find('?') == std::string::npos &&
          url.find('#') == std::string::npos;
      const bool loopback =
          url.size() > 7 && url.rfind("http://", 0) == 0 && url.size() <= 2048 &&
          !has_control_characters(url) && url.find('?') == std::string::npos &&
          url.find('#') == std::string::npos &&
          (url.substr(7).rfind("127.0.0.1", 0) == 0 ||
           url.substr(7).rfind("localhost", 0) == 0 || url.substr(7).rfind("[::1]", 0) == 0);
      if (!secure && !loopback) {
        return failure("INVALID_PARAMS", "an http MCP url must use HTTPS or loopback HTTP");
      }
      config["url"] = url;
    }

    // Static HTTP auth headers: a new secret enters the OS credential store; a
    // missing one preserves the stored header with the same name.
    std::vector<std::string> staged_references;
    nlohmann::json headers = nlohmann::json::array();
    if (input.contains("httpAuthHeaders")) {
      if (!input.at("httpAuthHeaders").is_array() || input.at("httpAuthHeaders").size() > 32) {
        return failure("INVALID_PARAMS", "mcp_server_save httpAuthHeaders are invalid");
      }
      std::set<std::string> names;
      for (const auto& entry : input.at("httpAuthHeaders")) {
        if (!entry.is_object() || !entry.contains("name") || !entry.at("name").is_string()) {
          return failure("INVALID_PARAMS", "mcp_server_save httpAuthHeaders are invalid");
        }
        const auto name = entry.at("name").get<std::string>();
        if (name.size() > 128 || has_control_characters(name)) {
          return failure("INVALID_PARAMS", "mcp_server_save header name is invalid");
        }
        std::string lower = name;
        std::transform(lower.begin(), lower.end(), lower.begin(), [](unsigned char byte) {
          return static_cast<char>(std::tolower(byte));
        });
        if (!names.insert(lower).second) {
          return failure("INVALID_PARAMS", "mcp_server_save header names must be unique");
        }
        std::string reference;
        if (entry.contains("secret") && entry.at("secret").is_string() &&
            !entry.at("secret").get<std::string>().empty()) {
          const auto secret = entry.at("secret").get<std::string>();
          if (secret.size() > 8192) {
            return failure("INVALID_PARAMS", "mcp_server_save header secret is too long");
          }
          try {
            reference = credentials_
                            ->set("mcp", "mcp-header-" + next_id("credential"),
                                  Secret{std::string_view(secret)})
                            .to_string();
          } catch (...) {
            return failure("CREDENTIAL_ERROR",
                           "The operating system credential store could not save the header secret");
          }
          staged_references.push_back(reference);
        } else if (existing.has_value()) {
          for (const auto& stored : existing->value("httpAuthHeaders", nlohmann::json::array())) {
            if (!stored.is_object() || !stored.value("name", std::string()).size()) continue;
            std::string stored_lower = stored.value("name", std::string());
            std::transform(stored_lower.begin(), stored_lower.end(), stored_lower.begin(),
                           [](unsigned char byte) { return static_cast<char>(std::tolower(byte)); });
            if (stored_lower == lower) reference = stored.value("credentialRef", std::string());
          }
        }
        headers.push_back({{"name", name}, {"credentialRef", reference}});
      }
    } else if (existing.has_value()) {
      headers = existing->value("httpAuthHeaders", nlohmann::json::array());
    }
    config["httpAuthHeaders"] = std::move(headers);

    // OAuth: a write-only client secret is staged in the credential store; the
    // token, secret and DPoP key of a previous identity are reclaimed below.
    std::optional<nlohmann::json> existing_oauth;
    if (existing.has_value() && existing->contains("oauth") && existing->at("oauth").is_object()) {
      existing_oauth = existing->at("oauth");
    }
    std::optional<nlohmann::json> resolved_oauth;
    {
      std::string oauth_reason;
      const auto requested_oauth =
          input.contains("oauth") ? input.at("oauth") : nlohmann::json(nullptr);
      if (!resolve_oauth_config(server_id, transport == "http", requested_oauth, existing_oauth,
                                config.at("httpAuthHeaders"), *credentials_, resolved_oauth,
                                staged_references, oauth_reason)) {
        std::string ignored;
        (void)clean_credential_references(database_, *credentials_, staged_references, ignored);
        return failure("INVALID_PARAMS", oauth_reason);
      }
    }
    if (resolved_oauth.has_value()) config["oauth"] = *resolved_oauth;

    try {
      database_->upsert_mcp_server(config);
    } catch (...) {
      std::string ignored;
      (void)clean_credential_references(database_, *credentials_, staged_references, ignored);
      return failure("STORAGE_ERROR", "The local MCP database could not save the server");
    }
    // A credential reference the saved row no longer points at (a replaced header
    // secret, or the token/secret/key of an OAuth identity that changed) is removed.
    if (existing.has_value()) {
      std::set<std::string> live;
      for (const auto& header : config.value("httpAuthHeaders", nlohmann::json::array())) {
        if (header.is_object() && header.contains("credentialRef") &&
            header.at("credentialRef").is_string()) {
          live.insert(header.at("credentialRef").get<std::string>());
        }
      }
      if (config.contains("oauth") && config.at("oauth").is_object()) {
        for (const auto* key : {"credentialRef", "clientSecretRef", "dpopKeyRef"}) {
          if (config.at("oauth").contains(key) && config.at("oauth").at(key).is_string()) {
            live.insert(config.at("oauth").at(key).get<std::string>());
          }
        }
      }
      std::vector<std::string> retired;
      for (const auto& header : existing->value("httpAuthHeaders", nlohmann::json::array())) {
        if (header.is_object() && header.contains("credentialRef") &&
            header.at("credentialRef").is_string()) {
          const auto reference = header.at("credentialRef").get<std::string>();
          if (!live.count(reference)) retired.push_back(reference);
        }
      }
      if (existing->contains("oauth") && existing->at("oauth").is_object()) {
        for (const auto* key : {"credentialRef", "clientSecretRef", "dpopKeyRef"}) {
          if (existing->at("oauth").contains(key) && existing->at("oauth").at(key).is_string()) {
            const auto reference = existing->at("oauth").at(key).get<std::string>();
            if (!live.count(reference)) retired.push_back(reference);
          }
        }
      }
      std::string ignored;
      (void)clean_credential_references(database_, *credentials_, retired, ignored);
    }
    try {
      return {true, view_for(database_->get_mcp_server(server_id).value())};
    } catch (...) {
      return failure("STORAGE_ERROR", "The saved MCP server could not be read back");
    }
  }

  if (method == "mcp_server_start" || method == "mcp_server_stop") {
    if (!only_fields(params, {"serverId"}, {"serverId"}) || !params.at("serverId").is_string()) {
      return failure("INVALID_PARAMS", method + " requires exactly serverId");
    }
    const auto server_id = trim_ascii(params.at("serverId").get<std::string>());
    if (server_id.empty() || server_id.size() > 160 || has_control_characters(server_id)) {
      return failure("INVALID_PARAMS", method + " serverId is invalid");
    }
    if (mcp_ == nullptr) {
      return failure("MCP_UNAVAILABLE", "The MCP supervisor is unavailable");
    }
    try {
      if (method == "mcp_server_start") {
        const auto row = database_->get_mcp_server(server_id);
        if (!row.has_value()) return failure("NOT_FOUND", "The MCP server was not found");
        // An unconnected OAuth server (or any other stored-config problem) is
        // reported in the view instead of being launched into a certain 401.
        if (const auto reason = oauth_config_error(*row); reason.has_value()) {
          return {true, view_for(*row)};
        }
        return {true, mcp_->start(server_id)};
      }
      return {true, mcp_->stop(server_id)};
    } catch (const std::exception& error) {
      return failure("MCP_ERROR", error.what());
    }
  }

  if (method == "mcp_server_review") {
    const std::set<std::string> allowed = {"serverId", "allowedTools", "trustLevel",
                                           "annotationTrust"};
    if (!only_fields(params, allowed, {"serverId", "allowedTools", "trustLevel"}) ||
        !params.at("serverId").is_string() || !params.at("allowedTools").is_array() ||
        !params.at("trustLevel").is_string()) {
      return failure("INVALID_PARAMS", "mcp_server_review input does not match the contract");
    }
    const auto server_id = trim_ascii(params.at("serverId").get<std::string>());
    if (server_id.empty() || server_id.size() > 160 || has_control_characters(server_id)) {
      return failure("INVALID_PARAMS", "mcp_server_review serverId is invalid");
    }
    const auto trust = params.at("trustLevel").get<std::string>();
    if (!is_one_of(trust, {"reviewed", "unreviewed"})) {
      return failure("INVALID_PARAMS", "mcp_server_review trustLevel is invalid");
    }
    const auto& tools = params.at("allowedTools");
    if (tools.size() > 512) {
      return failure("INVALID_PARAMS", "mcp_server_review allows at most 512 tools");
    }
    std::set<std::string> seen;
    nlohmann::json allowed_tools = nlohmann::json::array();
    for (const auto& tool : tools) {
      if (!tool.is_string()) {
        return failure("INVALID_PARAMS", "mcp_server_review allowedTools are invalid");
      }
      auto name = trim_ascii(tool.get<std::string>());
      if (name.empty() || name.size() > 256 || !seen.insert(name).second) {
        return failure("INVALID_PARAMS", "mcp_server_review allowedTools are invalid");
      }
      allowed_tools.push_back(std::move(name));
    }
    std::optional<std::string> annotation_trust;
    if (params.contains("annotationTrust")) {
      if (!params.at("annotationTrust").is_string() ||
          !is_one_of(params.at("annotationTrust").get<std::string>(), {"none", "trusted"})) {
        return failure("INVALID_PARAMS", "mcp_server_review annotationTrust is invalid");
      }
      annotation_trust = params.at("annotationTrust").get<std::string>();
    }
    std::optional<nlohmann::json> existing;
    try {
      existing = database_->get_mcp_server(server_id);
    } catch (...) {
      return failure("STORAGE_ERROR", "The local MCP database could not read the server");
    }
    if (!existing.has_value()) return failure("NOT_FOUND", "The MCP server was not found");
    (*existing)["allowedTools"] = std::move(allowed_tools);
    (*existing)["trustLevel"] = trust;
    if (annotation_trust.has_value()) (*existing)["annotationTrust"] = *annotation_trust;
    try {
      database_->upsert_mcp_server(*existing);
    } catch (...) {
      return failure("STORAGE_ERROR", "The local MCP database could not save the review");
    }
    try {
      return {true, view_for(database_->get_mcp_server(server_id).value())};
    } catch (...) {
      return failure("STORAGE_ERROR", "The reviewed MCP server could not be read back");
    }
  }

  if (method == "mcp_oauth_connect") {
    if (!only_fields(params, {"serverId"}, {"serverId"}) || !params.at("serverId").is_string()) {
      return failure("INVALID_PARAMS", "mcp_oauth_connect requires exactly serverId");
    }
    const auto server_id = trim_ascii(params.at("serverId").get<std::string>());
    if (server_id.empty() || server_id.size() > 160 || has_control_characters(server_id)) {
      return failure("INVALID_PARAMS", "mcp_oauth_connect serverId is invalid");
    }
    auto cancel = std::make_shared<std::atomic<bool>>(false);
    {
      std::lock_guard lock(oauth_flows_mutex_);
      if (oauth_flows_.count(server_id) != 0) {
        return failure("MCP_OAUTH_IN_PROGRESS",
                       "An OAuth authorization is already waiting for this MCP server");
      }
      oauth_flows_[server_id] = cancel;
    }
    // Removes the registry entry however the flow ends: success, failure, or a
    // cancellation that unwound an exception.
    struct FlowGuard {
      IpcRouter* router;
      std::string server_id;
      ~FlowGuard() {
        std::lock_guard lock(router->oauth_flows_mutex_);
        router->oauth_flows_.erase(server_id);
      }
    } guard{this, server_id};

    OAuthConnectCallbacks callbacks;
    callbacks.cancel = cancel;
    callbacks.open_browser = [](const std::string& url) { return open_external_url(url); };
    callbacks.device_code_prompt = [this](const nlohmann::json& prompt) {
      // The prompt is an event, not a return value: this call is still blocked
      // waiting for the user, and the UI needs the code while it waits. No token
      // is in this payload.
      std::function<void(const std::string&, const nlohmann::json&)> sink;
      {
        std::lock_guard lock(mutex_);
        sink = event_sink_;
      }
      if (sink) sink("mcp.oauth_device_code", prompt);
    };
    try {
      const auto result = oauth_connect(*database_, *credentials_, server_id, callbacks);
      // A running session still holds the old token. Stop it after the new token
      // is durably stored; the next start creates a fresh authenticated session.
      if (mcp_ != nullptr) mcp_->stop(server_id);
      return {true, result};
    } catch (const std::exception& error) {
      return failure("MCP_OAUTH_ERROR", error.what());
    }
  }

  if (method == "mcp_oauth_cancel") {
    if (!only_fields(params, {"serverId"}, {"serverId"}) || !params.at("serverId").is_string()) {
      return failure("INVALID_PARAMS", "mcp_oauth_cancel requires exactly serverId");
    }
    const auto server_id = trim_ascii(params.at("serverId").get<std::string>());
    if (server_id.empty() || server_id.size() > 160 || has_control_characters(server_id)) {
      return failure("INVALID_PARAMS", "mcp_oauth_cancel serverId is invalid");
    }
    std::shared_ptr<std::atomic<bool>> cancel;
    {
      std::lock_guard lock(oauth_flows_mutex_);
      const auto found = oauth_flows_.find(server_id);
      if (found != oauth_flows_.end()) cancel = found->second;
    }
    if (cancel != nullptr) cancel->store(true);
    // `false` is a real answer, not an error: the flow may have already finished,
    // timed out, or never started.
    return {true, {{"accepted", cancel != nullptr}}};
  }

  // mcp_server_delete
  if (!only_fields(params, {"serverId"}, {"serverId"}) || !params.at("serverId").is_string()) {
    return failure("INVALID_PARAMS", "mcp_server_delete requires exactly serverId");
  }
  const auto server_id = trim_ascii(params.at("serverId").get<std::string>());
  if (server_id.empty() || server_id.size() > 160 || has_control_characters(server_id)) {
    return failure("INVALID_PARAMS", "mcp_server_delete serverId is invalid");
  }
  try {
    auto removed = database_->delete_mcp_server(server_id);
    if (!removed.has_value()) return failure("NOT_FOUND", "The MCP server was not found");
    std::vector<std::string> references;
    for (const auto& header : removed->value("httpAuthHeaders", nlohmann::json::array())) {
      if (header.is_object() && header.contains("credentialRef") &&
          header.at("credentialRef").is_string()) {
        references.push_back(header.at("credentialRef").get<std::string>());
      }
    }
    if (removed->contains("oauth") && removed->at("oauth").is_object()) {
      for (const auto* key : {"clientSecretRef", "credentialRef", "dpopKeyRef"}) {
        const auto& oauth = removed->at("oauth");
        if (oauth.contains(key) && oauth.at(key).is_string()) {
          references.push_back(oauth.at(key).get<std::string>());
        }
      }
    }
    std::string cleanup_error;
    (void)clean_credential_references(database_, *credentials_, references, cleanup_error);
    return {true, {{"deleted", true}, {"stopped", false}}};
  } catch (...) {
    return failure("STORAGE_ERROR", "The local MCP database could not delete the server");
  }
}

IpcReply IpcRouter::provider_command(const std::string& method, const nlohmann::json& params) {
  static const std::map<std::string, std::string> kMethods = {
      {"provider_list", "provider.list"},
      {"provider_save", "provider.save"},
      {"provider_activate", "provider.activate"},
      {"provider_delete", "provider.delete"},
      {"provider_models", "provider.models"},
      {"provider_test", "provider.test"},
  };
  const auto found = kMethods.find(method);
  if (found == kMethods.end()) {
    return failure("NOT_IMPLEMENTED", "This provider command has not migrated yet: " + method);
  }
  try {
    auto& client = ensure_sidecar();
    return {true, client.request(found->second, params, std::chrono::milliseconds(60000))};
  } catch (const SidecarRpcError& error) {
    return failure(sidecar_error_code(error.code()), error.what());
  } catch (const SidecarError& error) {
    return failure("AGENT_UNAVAILABLE", error.what());
  }
}

nlohmann::json IpcRouter::handle_host_request(
    const std::string& method, const nlohmann::json& params,
    const std::shared_ptr<std::atomic<bool>>& cancel) {
  if (method == "host.mcp.catalog") {
    if (mcp_ == nullptr) {
      return nlohmann::json{{"servers", nlohmann::json::array()},
                            {"failures", nlohmann::json::array()}};
    }
    try {
      return mcp_->catalog();
    } catch (const std::exception&) {
      return nlohmann::json{
          {"servers", nlohmann::json::array()},
          {"failures",
           nlohmann::json::array({nlohmann::json{{"serverId", "mcp"},
                                                 {"code", "request_failed"},
                                                 {"message", "The MCP catalog could not be built"}}})}};
    }
  }
  if (method == "host.context.fetch") {
    if (database_ == nullptr || !database_->is_available()) {
      return nlohmann::json{
          {"status", "failed"},
          {"error", investigation_error("internal", "the local server database is unavailable",
                                        false)}};
    }
    return handle_context_request(*database_, params);
  }
  if (method == "host.investigation.filesystem_backup.register" ||
      method == "host.investigation.filesystem_backup.restored" ||
      method == "host.investigation.filesystem_backup.deleted") {
    if (database_ == nullptr || !database_->is_available()) {
      throw SidecarRpcError(-32603, "the local server database is unavailable");
    }
    try {
      if (method == "host.investigation.filesystem_backup.register") {
        return handle_filesystem_backup_register(*database_, params);
      }
      if (method == "host.investigation.filesystem_backup.restored") {
        return handle_filesystem_backup_restored(*database_, params);
      }
      return handle_filesystem_backup_deleted(*database_, params);
    } catch (const SidecarRpcError&) {
      throw;
    } catch (const std::exception& error) {
      throw SidecarRpcError(-32000, error.what());
    }
  }
  if (method == "host.filesystem.backup.list_query" ||
      method == "host.filesystem.backup.retention_scan") {
    if (database_ == nullptr || !database_->is_available()) {
      throw SidecarRpcError(-32603, "the local server database is unavailable");
    }
    try {
      if (method == "host.filesystem.backup.list_query") {
        return handle_filesystem_backup_list_query(*database_, params, host_tool_replay_mutex_,
                                                   host_tool_replays_);
      }
      return handle_filesystem_backup_retention_scan(*database_, params, host_tool_replay_mutex_,
                                                     host_tool_replays_);
    } catch (const SidecarRpcError&) {
      throw;
    } catch (const std::exception& error) {
      throw SidecarRpcError(-32000, error.what());
    }
  }
  if (method == "host.investigation.plan.record" ||
      method == "host.investigation.plan.check" ||
      method == "host.investigation.plan.step_result" ||
      method == "host.investigation.evidence.record" ||
      method == "host.investigation.evidence.fetch" ||
      method == "host.investigation.evidence.search" ||
      method == "host.investigation.evidence.compare" ||
      method == "host.investigation.evidence.correlate" ||
      method == "host.investigation.retention.preview" ||
      method == "host.investigation.finding.record" ||
      method == "host.investigation.brief.record" ||
      method == "host.investigation.artifact.record") {
    if (database_ == nullptr || !database_->is_available()) {
      return record_failure_json("internal", "the local server database is unavailable", false);
    }
    try {
      if (method == "host.investigation.plan.record") {
        return handle_plan_record(*database_, params);
      }
      if (method == "host.investigation.plan.check") {
        return handle_plan_check(*database_, params);
      }
      if (method == "host.investigation.plan.step_result") {
        return handle_plan_step_result(*database_, params);
      }
      if (method == "host.investigation.evidence.record") {
        return handle_evidence_record(*database_, params);
      }
      if (method == "host.investigation.evidence.fetch") {
        return handle_evidence_fetch(*database_, params);
      }
      if (method == "host.investigation.evidence.search") {
        return handle_evidence_search(*database_, params);
      }
      if (method == "host.investigation.evidence.compare") {
        return handle_evidence_compare(*database_, params);
      }
      if (method == "host.investigation.evidence.correlate") {
        return handle_evidence_correlation(*database_, params);
      }
      if (method == "host.investigation.retention.preview") {
        return handle_retention_preview(*database_, params);
      }
      if (method == "host.investigation.finding.record") {
        return handle_finding_record(*database_, params);
      }
      if (method == "host.investigation.brief.record") {
        return handle_brief_record(*database_, params);
      }
      return handle_artifact_record(*database_, params);
    } catch (const std::exception& error) {
      return record_failure_json("internal", error.what(), false);
    }
  }
  if (method == "host.tool.execute") {
    const auto tool_name =
        params.is_object() ? params.value("toolName", std::string()) : std::string();
    const auto failed = [](const std::string& code, const std::string& message) {
      return nlohmann::json{{"status", "failed"},
                            {"error", nlohmann::json{{"code", code}, {"message", message}}}};
    };
    if (tool_name.rfind("mcp.", 0) != 0) {
      return failed("unsupported_tool", "this desktop host does not execute that tool");
    }
    if (mcp_ == nullptr) return failed("mcp_unavailable", "The MCP supervisor is unavailable");
    const auto input = params.contains("input") ? params.at("input") : nlohmann::json::object();
    if (!input.is_object()) {
      return failed("invalid_arguments", "MCP tool arguments must be an object");
    }
    try {
      return nlohmann::json{{"status", "success"},
                            {"output", mcp_->call(tool_name, input, cancel)}};
    } catch (const std::exception& error) {
      if (cancel != nullptr && cancel->load()) {
        return failed("cancelled", "Host operation cancelled");
      }
      return failed("mcp_call_failed", error.what());
    }
  }
  if (method == "host.credentials.set") {
    if (!params.is_object() || !params.contains("service") || !params.contains("account") ||
        !params.contains("secret") || !params.at("service").is_string() ||
        !params.at("account").is_string() || !params.at("secret").is_string()) {
      throw SidecarRpcError(-32602,
                            "host.credentials.set requires service, account, and secret");
    }
    const auto reference = credentials_->set(
        params.at("service").get<std::string>(), params.at("account").get<std::string>(),
        Secret{std::string_view(params.at("secret").get<std::string>())});
    return {{"credentialRef", reference.to_string()}};
  }
  if (method == "host.credentials.get") {
    if (!params.is_object() || !params.contains("reference") ||
        !params.at("reference").is_string()) {
      throw SidecarRpcError(-32602, "host.credentials.get requires a reference");
    }
    const auto secret =
        credentials_->get(CredentialReference::parse(params.at("reference").get<std::string>()));
    return {{"secret", std::string(reinterpret_cast<const char*>(secret.data()), secret.size())}};
  }
  if (method == "host.credentials.reclaim") {
    if (!params.is_object() || !params.contains("reference") ||
        !params.at("reference").is_string()) {
      throw SidecarRpcError(-32602, "host.credentials.reclaim requires a reference");
    }
    credentials_->erase(CredentialReference::parse(params.at("reference").get<std::string>()));
    return {{"reclaimed", true}};
  }
  if (method == "host.ssh.effectful_action_target" ||
      method == "host.ssh.effectful_action_finished") {
    if (database_ == nullptr || !database_->is_available()) {
      throw SidecarRpcError(-32603, "the local server database is unavailable");
    }
    try {
      if (method == "host.ssh.effectful_action_target") {
        return handle_ssh_effectful_action_target(*database_, *credentials_, approvals_, params,
                                                  host_tool_replay_mutex_, host_tool_replays_);
      }
      return handle_ssh_effectful_action_finished(*database_, params, host_tool_replay_mutex_,
                                                  host_tool_replays_);
    } catch (const SidecarRpcError&) {
      throw;
    } catch (const std::exception& error) {
      throw SidecarRpcError(-32000, error.what());
    }
  }
  if (method == "host.ssh.service_target" || method == "host.ssh.filesystem_read_target" ||
      method == "host.ssh.filesystem_write_target" ||
      method == "host.ssh.filesystem_edit_target" ||
      method == "host.ssh.filesystem_backup_target" ||
      method == "host.ssh.filesystem_restore_target" ||
      method == "host.ssh.filesystem_cleanup_target") {
    if (database_ == nullptr || !database_->is_available()) {
      throw SidecarRpcError(-32603, "the local server database is unavailable");
    }
    try {
      if (method == "host.ssh.service_target") {
        const auto server_id =
            params.is_object() ? params.value("serverId", std::string()) : std::string();
        if (server_id.empty()) {
          throw SidecarRpcError(-32602, "host.ssh.service_target requires serverId");
        }
        return resolve_ssh_service_target(*database_, *credentials_, server_id);
      }
      if (!params.is_object()) {
        throw SidecarRpcError(-32602, method + " requires an object payload");
      }
      if (method == "host.ssh.filesystem_read_target") {
        return resolve_ssh_service_filesystem_read_target(*database_, *credentials_, params);
      }
      if (method == "host.ssh.filesystem_write_target") {
        return resolve_ssh_service_filesystem_write_target(*database_, *credentials_, params);
      }
      if (method == "host.ssh.filesystem_backup_target") {
        return handle_ssh_filesystem_backup_target(*database_, *credentials_, params);
      }
      if (method == "host.ssh.filesystem_restore_target") {
        return handle_ssh_filesystem_restore_target(*database_, *credentials_, params);
      }
      if (method == "host.ssh.filesystem_cleanup_target") {
        return handle_ssh_filesystem_cleanup_target(*database_, *credentials_, params);
      }
      return resolve_ssh_service_filesystem_edit_target(*database_, *credentials_, params);
    } catch (const SidecarRpcError&) {
      throw;
    } catch (const std::exception& error) {
      throw SidecarRpcError(-32603, error.what());
    }
  }
  if (method == "host.tool.cancel") {
    return {{"cancelled", false}};
  }
  throw SidecarRpcError(-32601, "The desktop host does not implement " + method);
}

IpcReply IpcRouter::subscribe(const nlohmann::json& params, bool add) {
  if (!exact_string_fields(params, {"event", "token"})) {
    return failure("INVALID_PARAMS", "Event subscriptions require an event and token");
  }
  const auto event = params.at("event").get<std::string>();
  const auto token = params.at("token").get<std::string>();
  if (!valid_event_name(event) || !valid_subscription_token(token)) {
    return failure("INVALID_PARAMS", "The event subscription is invalid");
  }

  std::lock_guard lock(mutex_);
  if (add) {
    subscriptions_[event].insert(token);
    return {true, {{"subscribed", true}}};
  }
  if (auto found = subscriptions_.find(event); found != subscriptions_.end()) {
    found->second.erase(token);
    if (found->second.empty()) subscriptions_.erase(found);
  }
  return {true, {{"subscribed", false}}};
}

std::set<std::string> IpcRouter::subscribers(const std::string& event) const {
  std::lock_guard lock(mutex_);
  const auto found = subscriptions_.find(event);
  return found == subscriptions_.end() ? std::set<std::string>{} : found->second;
}

bool is_external_url(const std::string& url) {
  if (url.empty() || url.size() > 4096) return false;
  if (std::any_of(url.begin(), url.end(), [](unsigned char c) {
        return c <= 0x20 || c == 0x7f;
      })) {
    return false;
  }
  auto prefix = url.substr(0, std::min<size_t>(8, url.size()));
  std::transform(prefix.begin(), prefix.end(), prefix.begin(), [](unsigned char c) {
    return static_cast<char>(std::tolower(c));
  });
  if (prefix == "https://") return url.size() > 8 && url[8] != '/' && url[8] != '?' && url[8] != '#';
  if (url.size() >= 7) {
    auto http = url.substr(0, 7);
    std::transform(http.begin(), http.end(), http.begin(), [](unsigned char c) {
      return static_cast<char>(std::tolower(c));
    });
    if (http == "http://") return url.size() > 7 && url[7] != '/' && url[7] != '?' && url[7] != '#';
  }
  auto mailto = url.substr(0, std::min<size_t>(7, url.size()));
  std::transform(mailto.begin(), mailto.end(), mailto.begin(), [](unsigned char c) {
    return static_cast<char>(std::tolower(c));
  });
  return mailto == "mailto:" && url.size() > 7;
}

bool open_external_url(const std::string& url) {
  if (!is_external_url(url)) return false;
#ifdef _WIN32
  const auto wide_url = utf8_to_wide(url);
  if (wide_url.empty()) return false;
  const auto result = ShellExecuteW(nullptr, L"open", wide_url.c_str(), nullptr, nullptr,
                                    SW_SHOWNORMAL);
  return reinterpret_cast<INT_PTR>(result) > 32;
#else
  const char* executable =
#ifdef __APPLE__
      "open";
#else
      "xdg-open";
#endif
  char* const arguments[] = {const_cast<char*>(executable), const_cast<char*>(url.c_str()),
                             nullptr};
  pid_t process = 0;
  if (posix_spawnp(&process, executable, nullptr, nullptr, arguments, environ) != 0) return false;
  std::thread([process]() {
    int status = 0;
    while (waitpid(process, &status, 0) < 0 && errno == EINTR) {}
  }).detach();
  return true;
#endif
}

std::string operating_system() {
#ifdef _WIN32
  return "windows";
#elif defined(__APPLE__)
  return "macos";
#else
  return "linux";
#endif
}

}  // namespace yukinal::desktop_host
