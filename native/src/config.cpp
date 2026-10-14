#include "yukinal/config.hpp"

#include <filesystem>
#include <fstream>
#include <cstdint>
#include <regex>
#include <set>
#include <stdexcept>

#include "yukinal/policy.hpp"

namespace yukinal::ssh_service {
namespace {

using Json = nlohmann::json;

void require_object(const Json& value, const std::string& label) {
  if (!value.is_object()) throw std::runtime_error(label + " must be an object");
}

void require_only_keys(const Json& value, const std::set<std::string>& allowed,
                       const std::string& label) {
  for (auto it = value.begin(); it != value.end(); ++it) {
    if (!allowed.contains(it.key())) throw std::runtime_error(label + " contains an unknown field");
  }
}

std::string required_string(const Json& value, const char* key, const std::string& label) {
  const auto it = value.find(key);
  if (it == value.end() || !it->is_string() || it->get_ref<const std::string&>().empty()) {
    throw std::runtime_error(label + "." + key + " must be a non-empty string");
  }
  return it->get<std::string>();
}

int required_integer(const Json& value, const char* key, int minimum, int maximum,
                     const std::string& label) {
  const auto it = value.find(key);
  if (it == value.end() || !(it->is_number_integer() || it->is_number_unsigned())) {
    throw std::runtime_error(label + "." + key + " must be an integer");
  }
  if (it->is_number_unsigned()) {
    const auto number = it->get<std::uint64_t>();
    if (number < static_cast<std::uint64_t>(minimum) || number > static_cast<std::uint64_t>(maximum)) {
      throw std::runtime_error(label + "." + key + " is out of range");
    }
    return static_cast<int>(number);
  }
  const auto number = it->get<std::int64_t>();
  if (number < minimum || number > maximum) throw std::runtime_error(label + "." + key + " is out of range");
  return static_cast<int>(number);
}

bool valid_env_name(const std::string& name) {
  static const std::regex pattern("^[A-Za-z_][A-Za-z0-9_]{0,127}$");
  return std::regex_match(name, pattern);
}

bool valid_host(const std::string& host) {
  if (host.empty() || host.size() > 253) return false;
  for (const unsigned char ch : host) {
    if (ch <= 0x20 || ch == 0x7f || ch == '/' || ch == '\\' || ch == '@' || ch == '?' || ch == '#') {
      return false;
    }
  }
  return true;
}

bool valid_username(const std::string& username) {
  static const std::regex pattern("^[A-Za-z_][A-Za-z0-9_.-]{0,63}$");
  return std::regex_match(username, pattern);
}

std::string resolve_path(const std::string& path, const std::string& base_directory) {
  std::filesystem::path result(path);
  if (result.is_relative() && !base_directory.empty()) {
    result = std::filesystem::path(base_directory) / result;
  }
  return result.lexically_normal().string();
}

}  // namespace

ServiceConfig parse_config(const nlohmann::json& value, const std::string& base_directory) {
  require_object(value, "config");
  require_only_keys(value, {"listen_port", "api_token_env", "targets"}, "config");

  ServiceConfig config;
  config.listen_port = required_integer(value, "listen_port", 1, 65535, "config");
  config.api_token_env = required_string(value, "api_token_env", "config");
  if (!valid_env_name(config.api_token_env)) {
    throw std::runtime_error("config.api_token_env must be an environment variable name");
  }

  const auto targets_it = value.find("targets");
  if (targets_it == value.end() || !targets_it->is_array() || targets_it->empty() || targets_it->size() > 256) {
    throw std::runtime_error("config.targets must contain between 1 and 256 targets");
  }

  for (std::size_t i = 0; i < targets_it->size(); ++i) {
    const Json& entry = (*targets_it)[i];
    const std::string label = "config.targets[" + std::to_string(i) + "]";
    require_object(entry, label);
    require_only_keys(entry, {"target_id", "host", "port", "username", "known_hosts_file", "auth"}, label);

    TargetConfig target;
    target.target_id = required_string(entry, "target_id", label);
    target.host = required_string(entry, "host", label);
    target.port = required_integer(entry, "port", 1, 65535, label);
    target.username = required_string(entry, "username", label);
    target.known_hosts_file = resolve_path(required_string(entry, "known_hosts_file", label), base_directory);
    if (!valid_target_id(target.target_id)) throw std::runtime_error(label + ".target_id is invalid");
    if (!valid_host(target.host)) throw std::runtime_error(label + ".host is invalid");
    if (!valid_username(target.username)) throw std::runtime_error(label + ".username is invalid");

    const auto auth_it = entry.find("auth");
    if (auth_it == entry.end()) throw std::runtime_error(label + ".auth is required");
    require_object(*auth_it, label + ".auth");
    require_only_keys(*auth_it, {"type", "private_key_file", "password_env", "passphrase_env"}, label + ".auth");
    const std::string auth_type = required_string(*auth_it, "type", label + ".auth");
    if (auth_type == "private_key") {
      if (auth_it->contains("password_env")) throw std::runtime_error(label + ".auth has conflicting credential fields");
      target.private_key_file = resolve_path(required_string(*auth_it, "private_key_file", label + ".auth"), base_directory);
      if (auth_it->contains("passphrase_env")) {
        target.passphrase_env = required_string(*auth_it, "passphrase_env", label + ".auth");
        if (!valid_env_name(target.passphrase_env)) throw std::runtime_error(label + ".auth.passphrase_env is invalid");
      }
    } else if (auth_type == "password") {
      if (auth_it->contains("private_key_file") || auth_it->contains("passphrase_env")) {
        throw std::runtime_error(label + ".auth has conflicting credential fields");
      }
      target.password_env = required_string(*auth_it, "password_env", label + ".auth");
      if (!valid_env_name(target.password_env)) throw std::runtime_error(label + ".auth.password_env is invalid");
    } else {
      throw std::runtime_error(label + ".auth.type must be private_key or password");
    }

    const std::string target_id = target.target_id;
    if (!config.targets.emplace(target_id, std::move(target)).second) {
      throw std::runtime_error("config contains a duplicate target_id");
    }
  }
  return config;
}

ServiceConfig load_config(const std::string& path) {
  std::ifstream input(path, std::ios::binary);
  if (!input) throw std::runtime_error("could not open the SSH service configuration file");
  Json value;
  try {
    input >> value;
  } catch (const Json::exception&) {
    throw std::runtime_error("SSH service configuration is not valid JSON");
  }
  const auto config_path = std::filesystem::absolute(path).lexically_normal();
  return parse_config(value, config_path.parent_path().string());
}

}  // namespace yukinal::ssh_service
