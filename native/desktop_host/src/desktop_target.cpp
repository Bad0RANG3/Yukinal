#include "yukinal/desktop_host/desktop_target.hpp"

#include "yukinal/desktop_host/content_revision.hpp"
#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/host_trust.hpp"
#include "yukinal/desktop_host/network_proxy.hpp"

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstdlib>
#include <filesystem>
#include <random>
#include <stdexcept>
#include <string>
#include <string_view>

namespace yukinal::desktop_host {
namespace {

bool valid_server_id(const std::string& value) {
  if (value.size() <= 4 || value.size() > 160 || value.rfind("srv_", 0) != 0) return false;
  return std::all_of(value.begin() + 4, value.end(), [](unsigned char byte) {
    return (byte >= 'a' && byte <= 'z') || (byte >= '0' && byte <= '9');
  });
}

bool valid_text(const std::string& value, std::size_t max_bytes) {
  if (value.empty() || value.size() > max_bytes) return false;
  return std::none_of(value.begin(), value.end(), [](unsigned char byte) {
    return byte <= 0x20 || byte == 0x7f;
  });
}

std::string credential_secret(const CredentialStore& credentials, const std::string& reference,
                              const char* label) {
  CredentialReference parsed;
  try {
    parsed = CredentialReference::parse(reference);
  } catch (const std::exception&) {
    throw std::runtime_error(std::string("the selected server ") + label +
                             " reference is invalid");
  }
  Secret secret = credentials.get(parsed);
  std::string value(reinterpret_cast<const char*>(secret.data()), secret.size());
  if (value.empty() || value.size() > 1024 * 1024 ||
      value.find('\0') != std::string::npos) {
    throw std::runtime_error(std::string("the selected server ") + label +
                             " is unavailable");
  }
  return value;
}

std::optional<std::string> optional_text(const nlohmann::json& object, const char* key) {
  const auto found = object.find(key);
  if (found == object.end() || !found->is_string()) return std::nullopt;
  auto value = found->get<std::string>();
  if (value.empty()) return std::nullopt;
  return value;
}

nlohmann::json parse_string_array(const std::optional<std::string>& encoded,
                                  const char* label) {
  nlohmann::json result = nlohmann::json::array();
  if (!encoded.has_value()) return result;
  nlohmann::json value;
  try {
    value = nlohmann::json::parse(*encoded);
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error(std::string("the stored server ") + label + " is invalid");
  }
  if (!value.is_array() || value.size() > 256) {
    throw std::runtime_error(std::string("the stored server ") + label + " is invalid");
  }
  for (const auto& entry : value) {
    if (!entry.is_string()) {
      throw std::runtime_error(std::string("the stored server ") + label + " is invalid");
    }
    result.push_back(entry);
  }
  return result;
}

// Mirrors the Python Agent's certificate-path rule: an explicit certificate file,
// or the private key path with a `-cert.pub` suffix, resolved against the working
// directory when relative.
std::string certificate_path(const nlohmann::json& identity) {
  std::optional<std::string> candidate = optional_text(identity, "certificatePath");
  if (!candidate.has_value()) {
    if (const auto private_key = optional_text(identity, "privateKeyPath");
        private_key.has_value()) {
      candidate = *private_key + "-cert.pub";
    }
  }
  if (!candidate.has_value() || candidate->size() > 4096 ||
      candidate->find('\0') != std::string::npos ||
      std::any_of(candidate->begin(), candidate->end(),
                  [](unsigned char byte) { return byte < 0x20 || byte == 0x7f; })) {
    throw std::runtime_error(
        "the selected SSH certificate identity has no valid public certificate file");
  }
  std::filesystem::path path(*candidate);
  if (!path.is_absolute()) {
    std::error_code error;
    const auto current = std::filesystem::current_path(error);
    if (error) throw std::runtime_error("the desktop working directory is unavailable");
    path = current / path;
  }
  const auto resolved = path.string();
  if (resolved.size() > 4096) {
    throw std::runtime_error("the SSH public certificate path is too long");
  }
  return resolved;
}

struct ProxyRoute {
  bool configured = false;
  nlohmann::json proxy;
  std::optional<std::string> credential;
};

ProxyRoute resolve_proxy_route(const DesktopNetworkProxyConfig& config,
                               const CredentialStore& credentials) {
  if (config.mode != "system") return ProxyRoute{};
  const auto resolution = resolve_network_proxy(true);
  const auto kind = resolution.value("kind", std::string());
  if (kind == "unusable") {
    throw std::runtime_error(
        "the configured proxy route is not supported by the C++ SSH service");
  }
  if (kind != "proxy") return ProxyRoute{};
  ProxyRoute route;
  route.configured = true;
  route.proxy = {{"url", resolution.at("url")}};
  if (config.credential_ref.has_value()) {
    route.credential = credential_secret(credentials, *config.credential_ref,
                                         "proxy credential");
  }
  return route;
}

nlohmann::json build_trust_source(const nlohmann::json& server, const std::string& host,
                                  int port, const std::string& data_directory) {
  nlohmann::json target = nlohmann::json::object();
  const auto authority_key = optional_text(server, "hostCertificateAuthorityPublicKey");
  if (authority_key.has_value()) {
    const auto principals =
        parse_string_array(optional_text(server, "hostPrincipals"), "host principals");
    if (principals.empty() || principals.size() > 256) {
      throw std::runtime_error("the stored server host principals are invalid");
    }
    nlohmann::json authority = {
        {"ca_public_key", *authority_key},
        {"principals", principals},
    };
    const auto revocation_path = optional_text(server, "hostRevocationListPath");
    const auto revocation_url = optional_text(server, "hostRevocationListUrl");
    if (revocation_path.has_value()) authority["revocation_list_path"] = *revocation_path;
    if (revocation_url.has_value()) authority["revocation_list_url"] = *revocation_url;
    const auto signers =
        parse_string_array(optional_text(server, "hostRevocationListSigners"),
                           "host revocation-list signers");
    if (!signers.empty()) authority["revocation_list_signers"] = signers;
    target["host_certificate_authority"] = std::move(authority);
    return target;
  }
  // No configured CA: the desktop pin is the trust source.
  const auto pin =
      HostTrustStore::load(std::filesystem::path(data_directory) / "known_hosts")
          .pinned_fingerprint(host, static_cast<std::uint16_t>(port));
  if (!pin.has_value()) {
    throw std::runtime_error("the selected server has no confirmed host-key pin");
  }
  target["host_key_fingerprint"] = *pin;
  return target;
}

// Path policy for the Agent file tools. This mirrors the retired Rust
// `yukinal_filesystem::policy`; the rules live here so the host tools have one
// place to change and the credentials/process-secret blocklist is enforced
// before any SFTP session is opened.
constexpr std::size_t kMaxRemotePathChars = 4096;
constexpr std::size_t kDefaultAgentReadBytes = 128 * 1024;
constexpr std::size_t kMaxAgentReadBytes = 1024 * 1024;
constexpr std::size_t kMaxAgentWriteBytes = 512 * 1024;
constexpr std::size_t kMaxAgentEditBytes = 512 * 1024;

std::size_t utf8_length(const std::string& value) {
  return static_cast<std::size_t>(std::count_if(value.begin(), value.end(), [](unsigned char byte) {
    return (byte & 0xC0) != 0x80;
  }));
}

std::string lower_ascii(const std::string& value) {
  std::string result = value;
  std::transform(result.begin(), result.end(), result.begin(), [](unsigned char byte) {
    return static_cast<char>(std::tolower(byte));
  });
  return result;
}

bool validate_remote_path(const std::string& value) {
  if (value.empty() || utf8_length(value) > kMaxRemotePathChars) return false;
  if (value.front() != '/') return false;
  return value.find('\0') == std::string::npos && value.find('\r') == std::string::npos &&
         value.find('\n') == std::string::npos;
}

bool is_agent_blocked_path(const std::string& value) {
  const auto normalized = lower_ascii(value);
  for (const auto* fragment : {"/.aws/", "/.azure/", "/.config/gcloud/", "/.kube/",
                               "/.ssh/", "/proc/", "/run/secrets/", "/var/run/secrets/"}) {
    if (normalized.find(fragment) != std::string::npos) return true;
  }
  const auto slash = normalized.rfind('/');
  const auto name = slash == std::string::npos ? normalized : normalized.substr(slash + 1);
  for (const auto* blocked : {"shadow", "gshadow", "sudoers", "id_rsa", "id_dsa", "id_ecdsa",
                              "id_ed25519", "credentials", "credentials.json", "secrets",
                              "secrets.json"}) {
    if (name == blocked) return true;
  }
  for (const auto* extension : {".pem", ".key", ".p12", ".pfx", ".jks"}) {
    const auto length = std::strlen(extension);
    if (name.size() >= length && name.compare(name.size() - length, length, extension) == 0) {
      return true;
    }
  }
  if (name == ".env" || name.rfind(".env.", 0) == 0) {
    return name != ".env.example" && name != ".env.sample" && name != ".env.template";
  }
  return false;
}

bool is_content_revision(const std::string& value) {
  return value.size() == 64 &&
         std::all_of(value.begin(), value.end(), [](unsigned char byte) {
           return std::isxdigit(byte) != 0;
         });
}

std::string required_param(const nlohmann::json& request, const char* key) {
  const auto found = request.find(key);
  if (found == request.end() || !found->is_string()) {
    throw std::runtime_error("the SSH filesystem target request is invalid");
  }
  return found->get<std::string>();
}

void require_checked_remote_path(const std::string& path) {
  if (!validate_remote_path(path)) {
    throw std::runtime_error(
        "remote path must be 1-4096 characters, absolute, and free of control characters");
  }
  if (is_agent_blocked_path(path)) {
    throw std::runtime_error(
        "Agent file tools cannot access paths that commonly contain credentials or process "
        "secrets");
  }
}

}  // namespace

nlohmann::json resolve_ssh_service_target(const DesktopDatabase& database,
                                          const CredentialStore& credentials,
                                          const std::string& server_id) {
  if (!valid_server_id(server_id)) {
    throw std::runtime_error("the SSH service target ID is invalid");
  }
  const auto record = database.get_server_connection(server_id);
  if (!record.has_value()) {
    throw std::runtime_error("the selected SSH server is not available");
  }
  const auto host = record->value("host", std::string());
  const auto port = record->value("port", 0);
  const auto username = record->value("username", std::string());
  if (!valid_text(host, 253) || port < 1 || port > 65535) {
    throw std::runtime_error("the stored server endpoint is invalid");
  }
  if (username.empty() || username.size() > 64 ||
      !(std::isalpha(static_cast<unsigned char>(username.front())) || username.front() == '_')) {
    throw std::runtime_error("the stored server username is invalid");
  }
  for (const auto byte : username) {
    const auto character = static_cast<unsigned char>(byte);
    if (!(std::isalnum(character) || byte == '_' || byte == '.' || byte == '-')) {
      throw std::runtime_error("the stored server username is invalid");
    }
  }

  const auto identity_id = optional_text(*record, "identityId");
  if (!identity_id.has_value()) {
    throw std::runtime_error("the selected server has no SSH identity");
  }
  const auto identity = database.get_identity(*identity_id);
  if (!identity.has_value()) {
    throw std::runtime_error("the selected server SSH identity is unavailable");
  }
  const auto method = identity->value("method", std::string());
  const auto credential_ref = optional_text(*identity, "credentialRef");
  if (!credential_ref.has_value()) {
    throw std::runtime_error("the selected server SSH identity has no credential");
  }

  nlohmann::json credentials_out = nlohmann::json::object();
  nlohmann::json auth;
  if (method == "password") {
    credentials_out["YK_SSH_PASSWORD"] =
        credential_secret(credentials, *credential_ref, "password");
    auth = {{"type", "password"}, {"password_env", "YK_SSH_PASSWORD"}};
  } else if (method == "privateKey" || method == "certificate") {
    credentials_out["YK_SSH_PRIVATE_KEY"] =
        credential_secret(credentials, *credential_ref, "private key");
    auth = {{"type", "private_key"}, {"private_key_env", "YK_SSH_PRIVATE_KEY"}};
    if (const auto passphrase_ref = optional_text(*identity, "passphraseRef");
        passphrase_ref.has_value()) {
      credentials_out["YK_SSH_PASSPHRASE"] =
          credential_secret(credentials, *passphrase_ref, "private-key passphrase");
      auth["passphrase_env"] = "YK_SSH_PASSPHRASE";
    }
    if (method == "certificate") {
      auth["public_key_file"] = certificate_path(*identity);
    }
  } else if (method == "agent") {
    if (const auto* socket = std::getenv("SSH_AUTH_SOCK"); socket != nullptr) {
      const std::string value(socket);
      if (value.empty() || value.size() > 4096 || value.find('\0') != std::string::npos) {
        throw std::runtime_error("the SSH agent socket path is invalid");
      }
      credentials_out["SSH_AUTH_SOCK"] = value;
    }
    auth = {{"type", "agent"}};
  } else {
    throw std::runtime_error(
        "the selected SSH identity method is not supported by the C++ service");
  }

  nlohmann::json target = {
      {"target_id", server_id},
      {"host", host},
      {"port", port},
      {"username", username},
      {"auth", std::move(auth)},
  };
  const auto trust_source =
      build_trust_source(*record, host, port, database.data_directory().string());
  for (auto item = trust_source.begin(); item != trust_source.end(); ++item) {
    target[item.key()] = item.value();
  }

  const auto proxy = resolve_proxy_route(database.network_proxy_config(), credentials);
  if (proxy.configured) {
    nlohmann::json proxy_json = proxy.proxy;
    if (proxy.credential.has_value()) {
      credentials_out["YK_SSH_PROXY_CREDENTIAL"] = *proxy.credential;
      proxy_json["auth_env"] = "YK_SSH_PROXY_CREDENTIAL";
    }
    target["proxy"] = std::move(proxy_json);
  }

  return {{"target", std::move(target)}, {"credentials", std::move(credentials_out)}};
}

nlohmann::json resolve_ssh_service_probe_target(const DesktopDatabase& database,
                                                const CredentialStore& credentials,
                                                const std::string& server_id) {
  if (!valid_server_id(server_id)) {
    throw std::runtime_error("the SSH host-key probe target ID is invalid");
  }
  const auto record = database.get_server_connection(server_id);
  if (!record.has_value()) {
    throw std::runtime_error("the selected SSH server is not available");
  }
  const auto host = record->value("host", std::string());
  const auto port = record->value("port", 0);
  if (!valid_text(host, 253) || port < 1 || port > 65535) {
    throw std::runtime_error("the stored server endpoint is invalid");
  }
  nlohmann::json target = {
      {"target_id", server_id},
      {"host", host},
      {"port", port},
      {"username", "ssh_probe"},
      {"probe_only", true},
      {"auth", {{"type", "probe"}}},
  };
  nlohmann::json credentials_out = nlohmann::json::object();
  const auto proxy = resolve_proxy_route(database.network_proxy_config(), credentials);
  if (proxy.configured) {
    nlohmann::json proxy_json = proxy.proxy;
    if (proxy.credential.has_value()) {
      credentials_out["YK_SSH_PROXY_CREDENTIAL"] = *proxy.credential;
      proxy_json["auth_env"] = "YK_SSH_PROXY_CREDENTIAL";
    }
    target["proxy"] = std::move(proxy_json);
  }
  return {{"target", std::move(target)}, {"credentials", std::move(credentials_out)}};
}

nlohmann::json resolve_ssh_service_filesystem_read_target(const DesktopDatabase& database,
                                                          const CredentialStore& credentials,
                                                          const nlohmann::json& request) {
  const auto path = required_param(request, "path");
  require_checked_remote_path(path);
  std::size_t max_bytes = kDefaultAgentReadBytes;
  if (const auto found = request.find("maxBytes"); found != request.end() && !found->is_null()) {
    if (!found->is_number_integer()) {
      throw std::runtime_error("the SSH filesystem target request is invalid");
    }
    const auto requested = found->get<std::int64_t>();
    if (requested < 1 || requested > static_cast<std::int64_t>(kMaxAgentReadBytes)) {
      throw std::runtime_error("maxBytes must be between 1 and 1048576");
    }
    max_bytes = static_cast<std::size_t>(requested);
  }
  auto payload =
      resolve_ssh_service_target(database, credentials, required_param(request, "serverId"));
  payload["filesystemRead"] = {{"path", path}, {"maxBytes", max_bytes}};
  return payload;
}

nlohmann::json resolve_ssh_service_filesystem_write_target(const DesktopDatabase& database,
                                                           const CredentialStore& credentials,
                                                           const nlohmann::json& request) {
  const auto path = required_param(request, "path");
  require_checked_remote_path(path);
  const auto content = required_param(request, "content");
  if (content.size() > kMaxAgentWriteBytes) {
    throw std::runtime_error("content must be at most 524288 bytes");
  }
  auto payload =
      resolve_ssh_service_target(database, credentials, required_param(request, "serverId"));
  payload["filesystemWrite"] = {{"path", path}, {"bytesWritten", content.size()}};
  return payload;
}

nlohmann::json resolve_ssh_service_filesystem_edit_target(const DesktopDatabase& database,
                                                          const CredentialStore& credentials,
                                                          const nlohmann::json& request) {
  const auto path = required_param(request, "path");
  require_checked_remote_path(path);
  const auto expected_revision = required_param(request, "expectedRevision");
  if (!is_content_revision(expected_revision)) {
    throw std::runtime_error(
        "expectedRevision must be the 64-character hex revision returned by filesystem.read");
  }
  const auto old_string = required_param(request, "oldString");
  const auto new_string = required_param(request, "newString");
  if (old_string.empty()) {
    throw std::runtime_error("oldString must not be empty: it must occur exactly once");
  }
  if (old_string.size() > kMaxAgentEditBytes || new_string.size() > kMaxAgentEditBytes) {
    throw std::runtime_error("oldString and newString must be at most 524288 bytes");
  }
  auto payload =
      resolve_ssh_service_target(database, credentials, required_param(request, "serverId"));
  payload["filesystemEdit"] = {{"path", path}, {"expectedRevision", expected_revision}};
  return payload;
}

bool is_safe_backup_token(const std::string& token) {
  if (token.size() != kBackupTokenChars) return false;
  return std::all_of(token.begin(), token.end(), [](unsigned char byte) {
    return (byte >= '0' && byte <= '9') || (byte >= 'a' && byte <= 'f');
  });
}

std::string backup_path_for(const std::string& path, const std::string& token) {
  if (!validate_remote_path(path)) {
    throw std::runtime_error(
        "remote path must be 1-4096 characters, absolute, and free of control characters");
  }
  if (!is_safe_backup_token(token)) {
    throw std::runtime_error("backup token must be 32 lowercase hexadecimal characters");
  }
  const auto slash = path.rfind('/');
  if (slash == std::string::npos) {
    throw std::runtime_error("backup source must name a file below the remote root");
  }
  const auto name = path.substr(slash + 1);
  if (name.empty()) {
    throw std::runtime_error("backup source must name a file, not a directory");
  }
  const auto parent_raw = path.substr(0, slash);
  const auto parent = parent_raw.empty() ? std::string("/") : parent_raw;
  const auto fingerprint = content_revision_hex(path).substr(0, 16);
  const auto backup_name = std::string(kBackupMarker) + fingerprint + "-" + token;
  const auto backup_path = parent == "/" ? "/" + backup_name : parent + "/" + backup_name;
  if (utf8_length(backup_path) > kMaxRemotePathChars) {
    throw std::runtime_error("derived backup path must be at most 4096 characters");
  }
  return backup_path;
}

bool is_backup_path_for(const std::string& path, const std::string& backup_path) {
  std::string expected;
  try {
    expected = backup_path_for(path, std::string(kBackupTokenChars, '0'));
  } catch (const std::exception&) {
    return false;
  }
  if (!validate_remote_path(backup_path)) return false;
  const auto expected_slash = expected.rfind('/');
  const auto actual_slash = backup_path.rfind('/');
  if (expected_slash == std::string::npos || actual_slash == std::string::npos) return false;
  const auto expected_parent = expected.substr(0, expected_slash);
  const auto expected_name = expected.substr(expected_slash + 1);
  const auto actual_parent = backup_path.substr(0, actual_slash);
  const auto actual_name = backup_path.substr(actual_slash + 1);
  const auto zeros = std::string(kBackupTokenChars, '0');
  auto prefix = expected_name;
  if (prefix.size() >= zeros.size() &&
      prefix.compare(prefix.size() - zeros.size(), zeros.size(), zeros) == 0) {
    prefix.resize(prefix.size() - zeros.size());
  }
  if (actual_parent != expected_parent) return false;
  if (actual_name.size() != prefix.size() + kBackupTokenChars) return false;
  if (actual_name.compare(0, prefix.size(), prefix) != 0) return false;
  return is_safe_backup_token(actual_name.substr(prefix.size()));
}

std::string backup_token() {
  std::random_device device;
  static constexpr char kHex[] = "0123456789abcdef";
  std::string token;
  token.reserve(kBackupTokenChars);
  for (std::size_t index = 0; index < kBackupTokenChars / 2; ++index) {
    const auto byte = static_cast<unsigned>(device() & 0xFFu);
    token.push_back(kHex[byte >> 4]);
    token.push_back(kHex[byte & 0x0Fu]);
  }
  return token;
}

std::string backup_record_id(const std::string& server_id, const std::string& backup_path) {
  return "backup_" + content_revision_hex(server_id + '\0' + backup_path);
}

void require_agent_backup_request(const std::string& path, const std::string& token) {
  require_checked_remote_path(path);
  if (!is_safe_backup_token(token)) {
    throw std::runtime_error("backup token must be 32 lowercase hexadecimal characters");
  }
  (void)backup_path_for(path, token);
}

void require_agent_restore_request(const std::string& path, const std::string& backup_path,
                                   const std::string& expected_revision) {
  require_checked_remote_path(path);
  if (!validate_remote_path(backup_path)) {
    throw std::runtime_error("backupPath must be an absolute remote path");
  }
  if (!is_backup_path_for(path, backup_path)) {
    throw std::runtime_error(
        "backupPath must be a host-generated backup for the same target path");
  }
  if (!is_content_revision(expected_revision)) {
    throw std::runtime_error(
        "expectedRevision must be the 64-character hex revision returned by filesystem.read or "
        "filesystem.edit");
  }
}

void require_agent_cleanup_request(const std::string& path, const std::string& backup_path,
                                   const std::string& expected_revision) {
  require_checked_remote_path(path);
  if (!validate_remote_path(backup_path)) {
    throw std::runtime_error("backupPath must be an absolute remote path");
  }
  if (!is_backup_path_for(path, backup_path)) {
    throw std::runtime_error(
        "backupPath must be a host-generated backup for the same target path");
  }
  if (!is_content_revision(expected_revision)) {
    throw std::runtime_error(
        "expectedRevision must be the 64-character hex revision returned by filesystem.backup");
  }
}

}  // namespace yukinal::desktop_host
