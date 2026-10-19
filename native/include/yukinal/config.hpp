#pragma once

#include <cstddef>
#include <map>
#include <memory>
#include <regex>
#include <string>
#include <vector>

#include <nlohmann/json.hpp>

namespace yukinal::ssh_service {

enum class ProxyProtocol {
  http_connect,
  https_connect,
  socks5_local_dns,
  socks5_remote_dns,
};

// One `server.exec` command-blacklist entry, used by both the service-level and
// the per-target `server_exec_denylist` arrays.
struct ServerExecDenyEntry {
  std::string pattern;                            // trimmed + whitespace-normalized literal or regex source
  bool is_regex = false;
  bool case_insensitive = false;
  std::shared_ptr<const std::regex> expression;   // non-null iff is_regex
};

struct ServerExecDenyList {
  std::vector<ServerExecDenyEntry> entries;
};

inline constexpr std::size_t kMaxServerExecDenyEntries = 256;
inline constexpr std::size_t kMaxServerExecDenyPatternBytes = 1024;

struct TargetConfig {
  std::string target_id;
  std::string host;
  int port = 22;
  std::string username;
  std::string known_hosts_file;
  std::string host_key_fingerprint;
  std::string host_ca_public_key;
  std::vector<std::string> host_principals;
  std::string host_ca_revocation_list_path;
  std::string host_ca_revocation_list_url;
  std::vector<std::string> host_ca_revocation_list_signers;
  std::string private_key_file;
  std::string public_key_file;
  std::string private_key_env;
  std::string password_env;
  std::string passphrase_env;
  std::string proxy_host;
  int proxy_port = 0;
  ProxyProtocol proxy_protocol = ProxyProtocol::http_connect;
  std::string proxy_auth_env;
  std::vector<std::string> proxy_no_proxy;
  bool agent_auth = false;
  bool probe_only = false;
  // Defaults to false. The desktop sets this only on a one-shot target config
  // after host policy authorizes the exact server.exec call.
  bool allow_server_exec = false;
  // The service-level entries merged in front of this target's own entries.
  ServerExecDenyList server_exec_denylist;
};

struct ServiceConfig {
  int listen_port = 8765;
  std::string api_token_env;
  std::map<std::string, TargetConfig> targets;
  // The unmerged service-level list; each target carries the merged list.
  ServerExecDenyList server_exec_denylist;
};

// Parses a `server_exec_denylist` array (a string literal or an object with
// `pattern`, `regex`, and `case_insensitive`). Throws std::runtime_error on any
// malformed entry; every message starts with `label`.
ServerExecDenyList parse_server_exec_denylist(const nlohmann::json& value, const std::string& label);
bool server_exec_command_is_denied(const ServerExecDenyList& list, const std::string& command,
                                   std::string* matched_pattern = nullptr);
nlohmann::json server_exec_denylist_to_json(const ServerExecDenyList& list);

ServiceConfig parse_config(const nlohmann::json& value, const std::string& base_directory = {});
ServiceConfig load_config(const std::string& path);
bool proxy_bypasses_target(const TargetConfig& target);

}  // namespace yukinal::ssh_service
