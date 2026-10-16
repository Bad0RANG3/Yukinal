#include "yukinal/desktop_host/ipc_router.hpp"

#include "yukinal/desktop_host/desktop_ssh.hpp"
#include "yukinal/desktop_host/desktop_target.hpp"
#include "yukinal/desktop_host/host_trust.hpp"
#include "yukinal/desktop_host/network_proxy.hpp"

#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
#include "host_key.hpp"
#endif

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <ctime>
#include <filesystem>
#include <iomanip>
#include <initializer_list>
#include <map>
#include <random>
#include <regex>
#include <sstream>
#include <string_view>
#include <utility>

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
      method == "server_snapshot") {
    return server_read_command(method, params);
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
      method == "mcp_server_delete") {
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
    if (database_ == nullptr || !database_->is_available()) {
      return failure("STORAGE_UNAVAILABLE", "The local file transfer database is unavailable");
    }
    try {
      return {true, database_->list_file_transfers(server_id, 100)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local file transfer database could not read transfers");
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
  sidecar_->set_host_handler([this](const std::string& method, const nlohmann::json& params) {
    return handle_host_request(method, params);
  });
  sidecar_->set_notification_sink([this](const std::string& method,
                                         const nlohmann::json& params) {
    std::function<void(const std::string&, const nlohmann::json&)> sink;
    {
      std::lock_guard lock(mutex_);
      sink = event_sink_;
    }
    if (!sink) return;
    if (method == "agent.stream" && params.is_object() && params.contains("type") &&
        params.at("type").is_string()) {
      sink(params.at("type").get<std::string>(), params);
    } else {
      sink(method, params);
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
  try {
    auto& client = ensure_sidecar();
    return {true, client.request(found->second, params, timeout)};
  } catch (const SidecarRpcError& error) {
    return failure(sidecar_error_code(error.code()), error.what());
  } catch (const SidecarError& error) {
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
    if (input.contains("oauth")) {
      return failure("NOT_IMPLEMENTED",
                     "OAuth MCP servers are not supported by this host yet");
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

    try {
      database_->upsert_mcp_server(config);
    } catch (...) {
      std::string ignored;
      (void)clean_credential_references(database_, *credentials_, staged_references, ignored);
      return failure("STORAGE_ERROR", "The local MCP database could not save the server");
    }
    try {
      return {true, view_for(database_->get_mcp_server(server_id).value())};
    } catch (...) {
      return failure("STORAGE_ERROR", "The saved MCP server could not be read back");
    }
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

nlohmann::json IpcRouter::handle_host_request(const std::string& method,
                                              const nlohmann::json& params) {
  if (method == "host.mcp.catalog") {
    return {{"servers", nlohmann::json::array()}, {"failures", nlohmann::json::array()}};
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
  if (method == "host.ssh.service_target" || method == "host.ssh.filesystem_read_target" ||
      method == "host.ssh.filesystem_write_target" ||
      method == "host.ssh.filesystem_edit_target") {
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
