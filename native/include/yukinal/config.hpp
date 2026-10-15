#pragma once

#include <map>
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
};

struct ServiceConfig {
  int listen_port = 8765;
  std::string api_token_env;
  std::map<std::string, TargetConfig> targets;
};

ServiceConfig parse_config(const nlohmann::json& value, const std::string& base_directory = {});
ServiceConfig load_config(const std::string& path);
bool proxy_bypasses_target(const TargetConfig& target);

}  // namespace yukinal::ssh_service
