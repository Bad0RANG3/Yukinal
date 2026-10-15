#include "yukinal/config.hpp"

#include <algorithm>
#include <array>
#include <charconv>
#include <cctype>
#include <filesystem>
#include <fstream>
#include <cstdint>
#include <regex>
#include <set>
#include <stdexcept>
#include <string_view>

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

bool valid_proxy_credential_env_name(const std::string& name) {
  if (!valid_env_name(name)) return false;
  std::string normalized = name;
  std::transform(normalized.begin(), normalized.end(), normalized.begin(), [](unsigned char ch) {
    return static_cast<char>(std::toupper(ch));
  });
  if (normalized.rfind("YUKINAL_", 0) == 0) return false;
  static const std::set<std::string> reserved = {
      "PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "HOME", "USERPROFILE",
      "TMPDIR", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"};
  return !reserved.contains(normalized);
}

bool parse_ipv4(std::string_view value, std::array<unsigned char, 4>* address) {
  if (!address) return false;
  std::size_t start = 0;
  for (std::size_t index = 0; index < address->size(); ++index) {
    const auto end = value.find('.', start);
    if ((index < address->size() - 1) == (end == std::string_view::npos)) return false;
    const auto octet = value.substr(start, end == std::string_view::npos ? value.size() - start : end - start);
    if (octet.empty() || octet.size() > 3 || (octet.size() > 1 && octet.front() == '0')) return false;
    unsigned int parsed = 0;
    const auto result = std::from_chars(octet.data(), octet.data() + octet.size(), parsed);
    if (result.ec != std::errc{} || result.ptr != octet.data() + octet.size() || parsed > 255) return false;
    (*address)[index] = static_cast<unsigned char>(parsed);
    start = end == std::string_view::npos ? value.size() : end + 1;
  }
  return start == value.size();
}

bool parse_ipv4_cidr(std::string_view value, std::array<unsigned char, 4>* address,
                     unsigned int* prefix) {
  const auto slash = value.find('/');
  if (slash == std::string_view::npos || value.find('/', slash + 1) != std::string_view::npos || !prefix) {
    return false;
  }
  if (!parse_ipv4(value.substr(0, slash), address)) return false;
  const auto prefix_text = value.substr(slash + 1);
  if (prefix_text.empty()) return false;
  unsigned int parsed = 0;
  const auto result = std::from_chars(prefix_text.data(), prefix_text.data() + prefix_text.size(), parsed);
  if (result.ec != std::errc{} || result.ptr != prefix_text.data() + prefix_text.size() || parsed > 32) {
    return false;
  }
  *prefix = parsed;
  return true;
}

bool ipv4_matches_cidr(std::string_view host, std::string_view cidr) {
  std::array<unsigned char, 4> address{};
  std::array<unsigned char, 4> network{};
  unsigned int prefix = 0;
  if (!parse_ipv4(host, &address) || !parse_ipv4_cidr(cidr, &network, &prefix)) return false;
  for (std::size_t index = 0; index < address.size(); ++index) {
    if (prefix >= 8) {
      if (address[index] != network[index]) return false;
      prefix -= 8;
    } else if (prefix > 0) {
      const auto mask = static_cast<unsigned char>(0xffU << (8 - prefix));
      if ((address[index] & mask) != (network[index] & mask)) return false;
      prefix = 0;
    }
  }
  return true;
}

bool valid_proxy_bypass_entry(const std::string& entry) {
  if (entry.empty() || entry.size() > 253) return false;
  if (entry == "*") return true;
  if (entry.find('/') != std::string::npos) {
    std::array<unsigned char, 4> network{};
    unsigned int prefix = 0;
    return parse_ipv4_cidr(entry, &network, &prefix);
  }

  std::string_view host = entry;
  if (host.rfind("*.", 0) == 0) host.remove_prefix(2);
  else if (host.front() == '.') host.remove_prefix(1);
  else if (host.size() > 2 && host.substr(host.size() - 2) == ".*") {
    const auto prefix = host.substr(0, host.size() - 2);
    std::size_t octets = 0;
    std::size_t start = 0;
    while (start <= prefix.size()) {
      const auto end = prefix.find('.', start);
      const auto octet = prefix.substr(start, end == std::string_view::npos ? prefix.size() - start : end - start);
      if (octet.empty() || octet.size() > 3 || ++octets > 3) return false;
      unsigned int value = 0;
      for (const unsigned char ch : octet) {
        if (!std::isdigit(ch)) return false;
        value = value * 10 + static_cast<unsigned int>(ch - '0');
      }
      if (value > 255) return false;
      if (end == std::string_view::npos) break;
      start = end + 1;
    }
    return true;
  } else if (host.find('*') != std::string_view::npos) {
    return false;
  }
  if (host.empty() || host.front() == '.' || host.back() == '.' || host.find("..") != std::string_view::npos) {
    return false;
  }
  const bool bracketed_ipv6 = host.size() >= 4 && host.front() == '[' && host.back() == ']' &&
      host.find(':') != std::string_view::npos;
  if (host.find(':') != std::string_view::npos && !bracketed_ipv6) return false;
  for (const unsigned char ch : host) {
    if (!(std::isalnum(ch) || ch == '.' || ch == '-' || ch == '_' || ch == ':' || ch == '[' || ch == ']')) {
      return false;
    }
  }
  return true;
}

std::string lowercase(std::string value) {
  std::transform(value.begin(), value.end(), value.begin(), [](unsigned char ch) {
    return static_cast<char>(std::tolower(ch));
  });
  return value;
}

bool host_matches_suffix(const std::string& host, const std::string& suffix) {
  return host == suffix || (host.size() > suffix.size() &&
      host.compare(host.size() - suffix.size(), suffix.size(), suffix) == 0 &&
      host[host.size() - suffix.size() - 1] == '.');
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

bool valid_host_key_fingerprint(const std::string& fingerprint) {
  static const std::regex pattern("^SHA256:[A-Za-z0-9+/]{43}$");
  return std::regex_match(fingerprint, pattern);
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
    require_only_keys(entry,
                      {"target_id", "host", "port", "username", "known_hosts_file",
                       "host_key_fingerprint", "auth", "probe_only", "proxy",
                       "allow_server_exec"},
                      label);

    TargetConfig target;
    target.target_id = required_string(entry, "target_id", label);
    target.host = required_string(entry, "host", label);
    target.port = required_integer(entry, "port", 1, 65535, label);
    target.username = required_string(entry, "username", label);
    if (entry.contains("probe_only")) {
      if (!entry["probe_only"].is_boolean()) {
        throw std::runtime_error(label + ".probe_only must be a boolean");
      }
      target.probe_only = entry["probe_only"].get<bool>();
    }
    if (entry.contains("allow_server_exec")) {
      if (!entry["allow_server_exec"].is_boolean()) {
        throw std::runtime_error(label + ".allow_server_exec must be a boolean");
      }
      target.allow_server_exec = entry["allow_server_exec"].get<bool>();
      if (target.allow_server_exec && target.probe_only) {
        throw std::runtime_error(label + ".probe_only targets cannot allow server.exec");
      }
    }
    const bool has_known_hosts_file = entry.contains("known_hosts_file");
    const bool has_host_key_fingerprint = entry.contains("host_key_fingerprint");
    if (target.probe_only) {
      if (has_known_hosts_file || has_host_key_fingerprint) {
        throw std::runtime_error(label + " probe-only targets cannot configure host-key trust");
      }
    } else if (has_known_hosts_file == has_host_key_fingerprint) {
      throw std::runtime_error(label + " must contain exactly one host-key trust source");
    } else if (has_known_hosts_file) {
      target.known_hosts_file = resolve_path(required_string(entry, "known_hosts_file", label), base_directory);
    } else if (!target.probe_only) {
      target.host_key_fingerprint = required_string(entry, "host_key_fingerprint", label);
      if (!valid_host_key_fingerprint(target.host_key_fingerprint)) {
        throw std::runtime_error(label + ".host_key_fingerprint must be a SHA256 host-key fingerprint");
      }
    }
    if (!valid_target_id(target.target_id)) throw std::runtime_error(label + ".target_id is invalid");
    if (!valid_host(target.host)) throw std::runtime_error(label + ".host is invalid");
    if (!valid_username(target.username)) throw std::runtime_error(label + ".username is invalid");

    if (entry.contains("proxy")) {
      const Json& proxy = entry["proxy"];
      require_object(proxy, label + ".proxy");
      require_only_keys(proxy, {"type", "host", "port", "auth_env", "no_proxy"}, label + ".proxy");
      if (required_string(proxy, "type", label + ".proxy") != "http_connect") {
        throw std::runtime_error(label + ".proxy.type must be http_connect");
      }
      target.proxy_host = required_string(proxy, "host", label + ".proxy");
      target.proxy_port = required_integer(proxy, "port", 1, 65535, label + ".proxy");
      if (!valid_host(target.proxy_host)) {
        throw std::runtime_error(label + ".proxy.host is invalid");
      }
      if (proxy.contains("auth_env")) {
        target.proxy_auth_env = required_string(proxy, "auth_env", label + ".proxy");
        if (!valid_proxy_credential_env_name(target.proxy_auth_env)) {
          throw std::runtime_error(label + ".proxy.auth_env is invalid");
        }
      }
      if (proxy.contains("no_proxy")) {
        const Json& bypasses = proxy["no_proxy"];
        if (!bypasses.is_array() || bypasses.size() > 256) {
          throw std::runtime_error(label + ".proxy.no_proxy must be an array of at most 256 entries");
        }
        for (const auto& bypass : bypasses) {
          if (!bypass.is_string() || !valid_proxy_bypass_entry(bypass.get<std::string>())) {
            throw std::runtime_error(label + ".proxy.no_proxy contains an unsupported entry");
          }
          target.proxy_no_proxy.push_back(bypass.get<std::string>());
        }
      }
    }

    const auto auth_it = entry.find("auth");
    if (auth_it == entry.end()) throw std::runtime_error(label + ".auth is required");
    require_object(*auth_it, label + ".auth");
    require_only_keys(*auth_it,
                      {"type", "private_key_file", "public_key_file", "private_key_env",
                       "password_env", "passphrase_env"},
                      label + ".auth");
    const std::string auth_type = required_string(*auth_it, "type", label + ".auth");
    if (target.probe_only) {
      if (auth_type != "probe" || auth_it->size() != 1) {
        throw std::runtime_error(label + ".auth probe-only configuration accepts only type=probe");
      }
    } else if (auth_type == "private_key") {
      if (auth_it->contains("password_env") ||
          auth_it->contains("private_key_file") == auth_it->contains("private_key_env")) {
        throw std::runtime_error(label + ".auth has conflicting or missing private-key fields");
      }
      if (auth_it->contains("private_key_file")) {
        target.private_key_file = resolve_path(required_string(*auth_it, "private_key_file", label + ".auth"), base_directory);
      } else {
        target.private_key_env = required_string(*auth_it, "private_key_env", label + ".auth");
        if (!valid_env_name(target.private_key_env)) {
          throw std::runtime_error(label + ".auth.private_key_env is invalid");
        }
      }
      if (auth_it->contains("public_key_file")) {
        target.public_key_file = resolve_path(
            required_string(*auth_it, "public_key_file", label + ".auth"), base_directory);
        if (target.public_key_file.size() > 4096 ||
            target.public_key_file.find('\0') != std::string::npos) {
          throw std::runtime_error(label + ".auth.public_key_file is invalid");
        }
      }
      if (auth_it->contains("passphrase_env")) {
        target.passphrase_env = required_string(*auth_it, "passphrase_env", label + ".auth");
        if (!valid_env_name(target.passphrase_env)) throw std::runtime_error(label + ".auth.passphrase_env is invalid");
      }
    } else if (auth_type == "password") {
      if (auth_it->contains("private_key_file") || auth_it->contains("private_key_env") ||
          auth_it->contains("public_key_file") || auth_it->contains("passphrase_env")) {
        throw std::runtime_error(label + ".auth has conflicting credential fields");
      }
      target.password_env = required_string(*auth_it, "password_env", label + ".auth");
      if (!valid_env_name(target.password_env)) throw std::runtime_error(label + ".auth.password_env is invalid");
    } else if (auth_type == "agent") {
      if (auth_it->size() != 1) {
        throw std::runtime_error(label + ".auth agent configuration accepts only type");
      }
      target.agent_auth = true;
    } else {
      throw std::runtime_error(label + ".auth.type must be private_key, password, or agent");
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

bool proxy_bypasses_target(const TargetConfig& target) {
  std::string host = lowercase(target.host);
  if (host.size() >= 2 && host.front() == '[' && host.back() == ']') {
    host = host.substr(1, host.size() - 2);
  }
  for (const auto& raw_entry : target.proxy_no_proxy) {
    const std::string entry = lowercase(raw_entry);
    if (entry == "*") return true;
    if (entry.find('/') != std::string::npos) {
      if (ipv4_matches_cidr(host, entry)) return true;
      continue;
    }
    if (entry.rfind("*.", 0) == 0) {
      if (host_matches_suffix(host, entry.substr(2))) return true;
      continue;
    }
    if (entry.front() == '.') {
      if (host_matches_suffix(host, entry.substr(1))) return true;
      continue;
    }
    if (entry.size() > 2 && entry.substr(entry.size() - 2) == ".*") {
      if (host.rfind(entry.substr(0, entry.size() - 1), 0) == 0) return true;
      continue;
    }
    std::string exact = entry;
    if (exact.size() >= 2 && exact.front() == '[' && exact.back() == ']') {
      exact = exact.substr(1, exact.size() - 2);
    }
    const bool numeric_address = !exact.empty() && std::all_of(exact.begin(), exact.end(), [](unsigned char ch) {
      return std::isdigit(ch) || ch == '.';
    });
    if ((numeric_address || exact.find(':') != std::string::npos) ? host == exact
                                                                  : host_matches_suffix(host, exact)) {
      return true;
    }
  }
  return false;
}

}  // namespace yukinal::ssh_service
