#pragma once

#include <map>
#include <string>

#include <nlohmann/json.hpp>

namespace yukinal::ssh_service {

struct TargetConfig {
  std::string target_id;
  std::string host;
  int port = 22;
  std::string username;
  std::string known_hosts_file;
  std::string host_key_fingerprint;
  std::string private_key_file;
  std::string private_key_env;
  std::string password_env;
  std::string passphrase_env;
  bool agent_auth = false;
};

struct ServiceConfig {
  int listen_port = 8765;
  std::string api_token_env;
  std::map<std::string, TargetConfig> targets;
};

ServiceConfig parse_config(const nlohmann::json& value, const std::string& base_directory = {});
ServiceConfig load_config(const std::string& path);

}  // namespace yukinal::ssh_service
