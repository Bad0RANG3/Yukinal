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
  YUKINAL_REQUIRE(!config.targets.at("staging-web-01").allow_server_exec);
}

YUKINAL_TEST(config_allows_server_exec_only_when_explicitly_enabled) {
  auto value = valid_config();
  value["targets"][0]["allow_server_exec"] = true;
  const auto enabled = parse_config(value);
  YUKINAL_REQUIRE(enabled.targets.at("staging-web-01").allow_server_exec);

  value["targets"][0]["probe_only"] = true;
  value["targets"][0].erase("known_hosts_file");
  value["targets"][0]["auth"] = {{"type", "probe"}};
  bool rejected = false;
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);
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

YUKINAL_TEST(config_accepts_host_certificate_authority_trust) {
  auto value = valid_config();
  auto& target = value["targets"][0];
  target.erase("known_hosts_file");
  target["host_certificate_authority"] = {
      {"ca_public_key", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA test-ca"},
      {"principals", json::array({"*.example.test", "api.internal"})}};
  const auto config = parse_config(value);
  const auto& parsed = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(parsed.host_ca_public_key.find("ssh-ed25519 ") == 0);
  YUKINAL_REQUIRE(parsed.host_principals.size() == 2);
  YUKINAL_REQUIRE(parsed.host_principals[0] == "*.example.test");
}

YUKINAL_TEST(config_rejects_ambiguous_or_invalid_host_certificate_authority_trust) {
  auto value = valid_config();
  value["targets"][0]["host_certificate_authority"] = {
      {"ca_public_key", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"},
      {"principals", json::array({"*.example.test"})}};
  bool rejected = false;
  try { (void)parse_config(value); } catch (const std::runtime_error&) { rejected = true; }
  YUKINAL_REQUIRE(rejected);

  value["targets"][0].erase("known_hosts_file");
  value["targets"][0]["host_certificate_authority"]["ca_public_key"] = "not-an-openssh-key";
  rejected = false;
  try { (void)parse_config(value); } catch (const std::runtime_error&) { rejected = true; }
  YUKINAL_REQUIRE(rejected);

  value["targets"][0]["host_certificate_authority"]["ca_public_key"] =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
  value["targets"][0]["host_certificate_authority"]["principals"] = json::array({""});
  rejected = false;
  try { (void)parse_config(value); } catch (const std::runtime_error&) { rejected = true; }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(config_accepts_host_certificate_krl_path_or_url) {
  auto value = valid_config();
  auto& target = value["targets"][0];
  target.erase("known_hosts_file");
  target["host_certificate_authority"] = {
      {"ca_public_key", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA test-ca"},
      {"principals", json::array({"*.example.test"})},
      {"revocation_list_url", "https://example.test/revoked.krl"},
      {"revocation_list_signers", json::array()}};
  auto parsed = parse_config(value);
  YUKINAL_REQUIRE(parsed.targets.at("staging-web-01").host_ca_revocation_list_url ==
                  "https://example.test/revoked.krl");
  YUKINAL_REQUIRE(parsed.targets.at("staging-web-01").host_ca_revocation_list_signers.empty());

  target["host_certificate_authority"].erase("revocation_list_url");
  target["host_certificate_authority"]["revocation_list_path"] =
      (std::filesystem::temp_directory_path() / "yukinal-test.krl").string();
  parsed = parse_config(value);
  YUKINAL_REQUIRE(parsed.targets.at("staging-web-01").host_ca_revocation_list_path ==
                  (std::filesystem::temp_directory_path() / "yukinal-test.krl").string());

  target["host_certificate_authority"]["revocation_list_url"] = "https://example.test/revoked.krl";
  bool rejected = false;
  try { (void)parse_config(value); } catch (const std::runtime_error&) { rejected = true; }
  YUKINAL_REQUIRE(rejected);

  target["host_certificate_authority"].erase("revocation_list_path");
  target["host_certificate_authority"]["revocation_list_signers"] = json::array({
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA other-comment"});
  rejected = false;
  try { (void)parse_config(value); } catch (const std::runtime_error&) { rejected = true; }
  YUKINAL_REQUIRE(rejected);
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

YUKINAL_TEST(config_accepts_tls_http_connect_proxy) {
  auto value = valid_config();
  value["targets"][0]["proxy"] = {
      {"type", "https_connect"}, {"host", "proxy.example.test"}, {"port", 8443}};

  const auto config = parse_config(value);
  const auto& target = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(target.proxy_host == "proxy.example.test");
  YUKINAL_REQUIRE(target.proxy_port == 8443);
  YUKINAL_REQUIRE(target.proxy_protocol == yukinal::ssh_service::ProxyProtocol::https_connect);
}

YUKINAL_TEST(config_accepts_socks5_proxy_routes) {
  auto value = valid_config();
  value["targets"][0]["proxy"] = {{"url", "socks5h://proxy.example.test"}};
  auto config = parse_config(value);
  const auto& remote_dns = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(remote_dns.proxy_host == "proxy.example.test");
  YUKINAL_REQUIRE(remote_dns.proxy_port == 1080);
  YUKINAL_REQUIRE(remote_dns.proxy_protocol == yukinal::ssh_service::ProxyProtocol::socks5_remote_dns);

  value["targets"][0]["proxy"] = {
      {"type", "socks5"}, {"host", "proxy.example.test"}, {"port", 1080}};
  config = parse_config(value);
  YUKINAL_REQUIRE(config.targets.at("staging-web-01").proxy_protocol ==
                  yukinal::ssh_service::ProxyProtocol::socks5_local_dns);
}

YUKINAL_TEST(config_applies_supported_no_proxy_host_patterns) {
  auto value = valid_config();
  value["targets"][0]["proxy"] = {
      {"type", "http_connect"}, {"host", "proxy.example.test"}, {"port", 8080},
      {"no_proxy", json::array({"*.corp", ".internal", "10.*", "[::1]", "localhost", "example.test", "192.168.0.0/16", "2001:db8:abcd::/48", "::ffff:192.0.2.0/120"})}};
  auto config = parse_config(value);
  auto& target = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(target.proxy_no_proxy.size() == 9);

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
  target.host = "192.168.42.1";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "192.169.42.1";
  YUKINAL_REQUIRE(!yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "[::1]";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "[2001:db8:abcd:42::1]";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "2001:db8:abce::1";
  YUKINAL_REQUIRE(!yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "[::ffff:192.0.2.4]";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "stage.example.test";
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.host = "badexample.test";
  YUKINAL_REQUIRE(!yukinal::ssh_service::proxy_bypasses_target(target));
}

YUKINAL_TEST(config_applies_no_proxy_port_constraints) {
  auto value = valid_config();
  value["targets"][0]["proxy"] = {
      {"type", "http_connect"}, {"host", "proxy.example.test"}, {"port", 8080},
      {"no_proxy", json::array({"localhost:2200", "*.corp:22", "[::1]:2022", "*:2222"})}};
  auto config = parse_config(value);
  auto& target = config.targets.at("staging-web-01");

  target.host = "localhost";
  target.port = 22;
  YUKINAL_REQUIRE(!yukinal::ssh_service::proxy_bypasses_target(target));
  target.port = 2200;
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));

  target.host = "api.corp";
  target.port = 22;
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.port = 2200;
  YUKINAL_REQUIRE(!yukinal::ssh_service::proxy_bypasses_target(target));

  target.host = "[::1]";
  target.port = 2022;
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));

  target.host = "other.example.test";
  target.port = 2222;
  YUKINAL_REQUIRE(yukinal::ssh_service::proxy_bypasses_target(target));
  target.port = 22;
  YUKINAL_REQUIRE(!yukinal::ssh_service::proxy_bypasses_target(target));
}

YUKINAL_TEST(config_rejects_invalid_no_proxy_patterns) {
  auto value = valid_config();
  value["targets"][0]["proxy"] = {
      {"type", "http_connect"}, {"host", "proxy.example.test"}, {"port", 8080},
      {"no_proxy", json::array({"2001:db8::/129"})}};
  bool rejected = false;
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    rejected = true;
  }
  YUKINAL_REQUIRE(rejected);

  for (const auto& invalid_entry : {"localhost:0", "localhost:65536", "localhost:abc", "[::1]:22:23"}) {
    value["targets"][0]["proxy"]["no_proxy"] = json::array({invalid_entry});
    rejected = false;
    try {
      (void)parse_config(value);
    } catch (const std::runtime_error&) {
      rejected = true;
    }
    YUKINAL_REQUIRE(rejected);
  }

  value["targets"][0]["proxy"]["no_proxy"] = json::array({"10.0.0.0/33"});
  rejected = false;
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
      {"type", "socks4"}, {"host", "proxy.example.test"}, {"port", 1080}};
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
