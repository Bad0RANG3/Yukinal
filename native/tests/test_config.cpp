#include <filesystem>
#include <stdexcept>

#include "test_support.hpp"
#include "yukinal/config.hpp"

using nlohmann::json;
using yukinal::ssh_service::parse_config;

namespace {

json valid_config() {
  return json{{"listen_port", 8765},
              {"api_token_env", "YUKINAL_SSH_API_TOKEN"},
              {"targets", json::array({json{{"target_id", "staging-web-01"},
                                               {"host", "stage.example.test"},
                                               {"port", 2222},
                                               {"username", "ops"},
                                               {"known_hosts_file", "known_hosts"},
                                               {"auth", {{"type", "password"},
                                                        {"password_env", "STAGING_SSH_PASSWORD"}}}}})}};
}

}  // namespace

YUKINAL_TEST(config_resolves_paths_relative_to_config) {
  const std::filesystem::path base_directory("config-root");
  const auto config = parse_config(valid_config(), base_directory.string());
  YUKINAL_REQUIRE(config.listen_port == 8765);
  YUKINAL_REQUIRE(config.targets.size() == 1);
  YUKINAL_REQUIRE(config.targets.at("staging-web-01").host == "stage.example.test");
  const auto expected = (base_directory / "known_hosts").lexically_normal().string();
  YUKINAL_REQUIRE(config.targets.at("staging-web-01").known_hosts_file == expected);
  YUKINAL_REQUIRE(config.targets.at("staging-web-01").password_env == "STAGING_SSH_PASSWORD");
}

YUKINAL_TEST(config_rejects_unknown_fields) {
  auto value = valid_config();
  value["targets"][0]["command"] = "uname -a";
  bool rejected = false;
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(config_rejects_duplicate_target_ids) {
  auto value = valid_config();
  value["targets"].push_back(value["targets"][0]);
  bool rejected = false;
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(config_rejects_ambiguous_authentication) {
  auto value = valid_config();
  value["targets"][0]["auth"]["private_key_file"] = "id_ed25519";
  bool rejected = false;
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(config_accepts_desktop_host_key_pin_and_memory_private_key) {
  auto value = valid_config();
  auto& target = value["targets"][0];
  target.erase("known_hosts_file");
  target["host_key_fingerprint"] = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
  target["auth"] = {{"type", "private_key"}, {"private_key_env", "YK_SSH_KEY_0"}};

  const auto config = parse_config(value);
  const auto& parsed = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(parsed.known_hosts_file.empty());
  YUKINAL_REQUIRE(parsed.host_key_fingerprint == "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
  YUKINAL_REQUIRE(parsed.private_key_env == "YK_SSH_KEY_0");
}

YUKINAL_TEST(config_accepts_public_certificate_with_memory_private_key) {
  auto value = valid_config();
  value["targets"][0]["auth"] = {
      {"type", "private_key"},
      {"private_key_env", "YK_SSH_KEY_0"},
      {"public_key_file", "keys/id_ed25519-cert.pub"}};

  const auto config = parse_config(value, "config-root");
  const auto& parsed = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(parsed.private_key_env == "YK_SSH_KEY_0");
  YUKINAL_REQUIRE(parsed.public_key_file ==
                  (std::filesystem::path("config-root") / "keys/id_ed25519-cert.pub")
                      .lexically_normal()
                      .string());
}

YUKINAL_TEST(config_rejects_public_certificate_with_password_authentication) {
  auto value = valid_config();
  value["targets"][0]["auth"]["public_key_file"] = "id_ed25519-cert.pub";
  bool rejected = false;
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(config_accepts_ssh_agent_authentication) {
  auto value = valid_config();
  value["targets"][0]["auth"] = {{"type", "agent"}};

  const auto config = parse_config(value);
  YUKINAL_REQUIRE(config.targets.at("staging-web-01").agent_auth);
}

YUKINAL_TEST(config_rejects_ssh_agent_credential_fields) {
  auto value = valid_config();
  value["targets"][0]["auth"] = {{"type", "agent"}, {"password_env", "SSH_PASSWORD"}};

  bool rejected = false;
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(config_accepts_isolated_probe_only_target) {
  auto value = valid_config();
  auto& target = value["targets"][0];
  target.erase("known_hosts_file");
  target["probe_only"] = true;
  target["auth"] = {{"type", "probe"}};

  const auto config = parse_config(value);
  YUKINAL_REQUIRE(config.targets.at("staging-web-01").probe_only);
}

YUKINAL_TEST(config_accepts_authenticated_http_connect_proxy) {
  auto value = valid_config();
  value["targets"][0]["proxy"] = {
      {"type", "http_connect"}, {"host", "proxy.example.test"}, {"port", 8080},
      {"auth_env", "YK_SSH_PROXY_CREDENTIAL"}};

  const auto config = parse_config(value);
  const auto& target = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(target.proxy_host == "proxy.example.test");
  YUKINAL_REQUIRE(target.proxy_port == 8080);
  YUKINAL_REQUIRE(target.proxy_auth_env == "YK_SSH_PROXY_CREDENTIAL");
}

YUKINAL_TEST(config_applies_supported_no_proxy_host_patterns) {
  auto value = valid_config();
  value["targets"][0]["proxy"] = {
      {"type", "http_connect"}, {"host", "proxy.example.test"}, {"port", 8080},
      {"no_proxy", json::array({"*.corp", ".internal", "10.*", "[::1]", "localhost", "example.test"})}};
  auto config = parse_config(value);
  auto& target = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(target.proxy_no_proxy.size() == 6);

  target.host = "api.corp";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "corp";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "badcorp";
  YUKINAL_REQUIRE(!yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "node.internal";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "10.42.0.8";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "[::1]";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "stage.example.test";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "badexample.test";
  YUKINAL_REQUIRE(!yukinal::ssh_service::proxy_bypasses_target(target));
}

YUKINAL_TEST(config_rejects_unimplemented_no_proxy_patterns) {
  auto value = valid_config();
  value["targets"][0]["proxy"] = {
      {"type", "http_connect"}, {"host", "proxy.example.test"}, {"port", 8080},
      {"no_proxy", json::array({"10.0.0.0/8"})}};
  bool rejected = false;
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(config_rejects_unsupported_or_malformed_proxy) {
  auto unsupported = valid_config();
  unsupported["targets"][0]["proxy"] = {
      {"type", "socks5"}, {"host", "proxy.example.test"}, {"port", 1080}};
  bool unsupported_rejected = false;
  try {
    (void)parse_config(unsupported);
  } catch (const std::runtime_error&) {
    unsupported_rejected = true;
  }
  YUKINAL_REQUIRE(unsupported_rejected);

  auto invalid_environment = valid_config();
  invalid_environment["targets"][0]["proxy"] = {
      {"type", "http_connect"}, {"host", "proxy.example.test"}, {"port", 8080},
      {"auth_env", "INVALID-NAME"}};
  bool environment_rejected = false;
  try {
    (void)parse_config(invalid_environment);
  } catch (const std::runtime_error&) {
    environment_rejected = true;
  }
  YUKINAL_REQUIRE(environment_rejected);

  auto reserved_environment = valid_config();
  reserved_environment["targets"][0]["proxy"] = {
      {"type", "http_connect"}, {"host", "proxy.example.test"}, {"port", 8080},
      {"auth_env", "PATH"}};
  bool reserved_rejected = false;
  try {
    (void)parse_config(reserved_environment);
  } catch (const std::runtime_error&) {
    reserved_rejected = true;
  }
  YUKINAL_REQUIRE(reserved_rejected);
}

YUKINAL_TEST(config_rejects_probe_target_credentials_and_trust) {
  auto auth_rejected = valid_config();
  auto& target = auth_rejected["targets"][0];
  target.erase("known_hosts_file");
  target["probe_only"] = true;
  target["auth"] = {{"type", "password"}, {"password_env", "SSH_PASSWORD"}};
  bool rejected = false;
  try {
    (void)parse_config(auth_rejected);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);

  auto trust_rejected = valid_config();
  trust_rejected["targets"][0]["probe_only"] = true;
  trust_rejected["targets"][0]["auth"] = {{"type", "probe"}};
  rejected = false;
  try {
    (void)parse_config(trust_rejected);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);

  auto regular_auth_rejected = valid_config();
  regular_auth_rejected["targets"][0]["auth"] = {{"type", "probe"}};
  rejected = false;
  try {
    (void)parse_config(regular_auth_rejected);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(config_rejects_missing_or_ambiguous_host_key_trust) {
  auto missing = valid_config();
  missing["targets"][0].erase("known_hosts_file");
  bool missing_rejected = false;
  try {
    (void)parse_config(missing);
  } catch (const std::runtime_error&) {
    missing_rejected = true;
  }
  YUKINAL_REQUIRE(missing_rejected);

  auto ambiguous = valid_config();
  ambiguous["targets"][0]["host_key_fingerprint"] =
      "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
  bool ambiguous_rejected = false;
  try {
    (void)parse_config(ambiguous);
  } catch (const std::runtime_error&) {
    ambiguous_rejected = true;
  }
  YUKINAL_REQUIRE(ambiguous_rejected);
}

YUKINAL_TEST(config_rejects_invalid_host_key_fingerprint) {
  auto value = valid_config();
  value["targets"][0].erase("known_hosts_file");
  value["targets"][0]["host_key_fingerprint"] = "SHA256:short";
  bool rejected = false;
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(config_rejects_missing_or_ambiguous_private_key_source) {
  auto missing = valid_config();
  missing["targets"][0]["auth"] = {{"type", "private_key"}};
  bool missing_rejected = false;
  try {
    (void)parse_config(missing);
  } catch (const std::runtime_error&) {
    missing_rejected = true;
  }
  YUKINAL_REQUIRE(missing_rejected);

  auto ambiguous = valid_config();
  ambiguous["targets"][0]["auth"] = {
      {"type", "private_key"}, {"private_key_file", "id_ed25519"}, {"private_key_env", "YK_SSH_KEY_0"}};
  bool ambiguous_rejected = false;
  try {
    (void)parse_config(ambiguous);
  } catch (const std::runtime_error&) {
    ambiguous_rejected = true;
  }
  YUKINAL_REQUIRE(ambiguous_rejected);
}
