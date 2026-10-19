#include <filesystem>
#include <stdexcept>

#include "test_support.hpp"
#include "yukinal/config.hpp"

using nlohmann::json;
using yukinal::ssh_service::kMaxServerExecDenyEntries;
using yukinal::ssh_service::kMaxServerExecDenyPatternBytes;
using yukinal::ssh_service::parse_config;
using yukinal::ssh_service::parse_server_exec_denylist;
using yukinal::ssh_service::server_exec_command_is_denied;
using yukinal::ssh_service::server_exec_denylist_to_json;
using yukinal::ssh_service::ServerExecDenyList;

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

bool config_rejected(const json& value) {
  try {
    (void)parse_config(value);
  } catch (const std::runtime_error&) {
    return true;
  }
  return false;
}

bool denylist_rejected(const json& value) {
  try {
    (void)parse_server_exec_denylist(value, "config.server_exec_denylist");
  } catch (const std::runtime_error&) {
    return true;
  }
  return false;
}

std::string denylist_error(const json& value) {
  try {
    (void)parse_server_exec_denylist(value, "config.server_exec_denylist");
  } catch (const std::runtime_error& error) {
    return error.what();
  }
  return {};
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

YUKINAL_TEST(config_parses_server_exec_denylist_literal_and_regex_entries) {
  const auto list = parse_server_exec_denylist(
      json::array({"rm -rf /",
                   json{{"pattern", "shutdown"}, {"regex", true}, {"case_insensitive", true}},
                   json{{"pattern", "sudo systemctl stop"}, {"regex", false}}}),
      "config.server_exec_denylist");
  YUKINAL_REQUIRE(list.entries.size() == 3);

  YUKINAL_REQUIRE(list.entries[0].pattern == "rm -rf /");
  YUKINAL_REQUIRE(!list.entries[0].is_regex);
  YUKINAL_REQUIRE(!list.entries[0].case_insensitive);
  YUKINAL_REQUIRE(list.entries[0].expression == nullptr);

  YUKINAL_REQUIRE(list.entries[1].pattern == "shutdown");
  YUKINAL_REQUIRE(list.entries[1].is_regex);
  YUKINAL_REQUIRE(list.entries[1].case_insensitive);
  YUKINAL_REQUIRE(list.entries[1].expression != nullptr);

  YUKINAL_REQUIRE(list.entries[2].pattern == "sudo systemctl stop");
  YUKINAL_REQUIRE(!list.entries[2].is_regex);
  YUKINAL_REQUIRE(!list.entries[2].case_insensitive);
  YUKINAL_REQUIRE(list.entries[2].expression == nullptr);

  const auto round_trip = server_exec_denylist_to_json(list);
  YUKINAL_REQUIRE(round_trip.is_array());
  YUKINAL_REQUIRE(round_trip.size() == 3);
  const json expected_literal = {{"pattern", "rm -rf /"}, {"regex", false}, {"case_insensitive", false}};
  const json expected_regex = {{"pattern", "shutdown"}, {"regex", true}, {"case_insensitive", true}};
  const json expected_plain = {
      {"pattern", "sudo systemctl stop"}, {"regex", false}, {"case_insensitive", false}};
  YUKINAL_REQUIRE(round_trip[0] == expected_literal);
  YUKINAL_REQUIRE(round_trip[1] == expected_regex);
  YUKINAL_REQUIRE(round_trip[2] == expected_plain);

  const auto reparsed = parse_server_exec_denylist(round_trip, "config.server_exec_denylist");
  YUKINAL_REQUIRE(reparsed.entries.size() == 3);
  YUKINAL_REQUIRE(reparsed.entries[1].is_regex);
  YUKINAL_REQUIRE(reparsed.entries[1].case_insensitive);
}

YUKINAL_TEST(config_normalizes_server_exec_denylist_literal_and_regex_sources) {
  const auto list = parse_server_exec_denylist(
      json::array({"  rm \t\n -rf   /  ",
                   json{{"pattern", "a   b"}, {"regex", true}},
                   json{{"pattern", "  ^sudo\\s+reboot$  "}, {"regex", true}}}),
      "config.server_exec_denylist");
  YUKINAL_REQUIRE(list.entries.size() == 3);

  // The normalized source is both what is compiled and what is reported.
  YUKINAL_REQUIRE(list.entries[0].pattern == "rm -rf /");
  YUKINAL_REQUIRE(!list.entries[0].is_regex);
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "rm -rf / --no-preserve-root"));

  // A regex source with internal whitespace collapses too, and still matches the
  // collapsed command text.
  YUKINAL_REQUIRE(list.entries[1].pattern == "a b");
  YUKINAL_REQUIRE(list.entries[1].is_regex);
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "a b"));
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "xxa   bxx"));

  YUKINAL_REQUIRE(list.entries[2].pattern == "^sudo\\s+reboot$");
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "sudo   reboot"));

  const auto round_trip = server_exec_denylist_to_json(list);
  YUKINAL_REQUIRE(round_trip[0]["pattern"] == "rm -rf /");
  YUKINAL_REQUIRE(round_trip[1]["pattern"] == "a b");
  YUKINAL_REQUIRE(round_trip[2]["pattern"] == "^sudo\\s+reboot$");
}

YUKINAL_TEST(config_rejects_malformed_server_exec_denylist_entries) {
  YUKINAL_REQUIRE(denylist_rejected(json{{"pattern", "rm -rf /"}}));
  YUKINAL_REQUIRE(denylist_rejected(json("rm -rf /")));
  YUKINAL_REQUIRE(denylist_rejected(json::array({42})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json::array({"rm -rf /"})})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"pattern", "rm -rf /"}, {"unknown", true}}})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"regex", true}}})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"pattern", ""}}})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"pattern", "   \t\n"}}})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"pattern", 7}}})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"pattern", nullptr}}})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({std::string("rm -rf /\0hidden", 15)})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"pattern", "shutdown"}, {"regex", "yes"}}})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"pattern", "shutdown"}, {"case_insensitive", 1}}})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"pattern", "("}, {"regex", true}}})));
  YUKINAL_REQUIRE(denylist_rejected(json::array({json{{"pattern", "[unterminated"}, {"regex", true}}})));

  json too_many = json::array();
  for (std::size_t i = 0; i < kMaxServerExecDenyEntries + 1; ++i) {
    too_many.push_back("command-" + std::to_string(i));
  }
  YUKINAL_REQUIRE(denylist_rejected(too_many));

  json at_limit = json::array();
  for (std::size_t i = 0; i < kMaxServerExecDenyEntries; ++i) {
    at_limit.push_back("command-" + std::to_string(i));
  }
  YUKINAL_REQUIRE(parse_server_exec_denylist(at_limit, "config.server_exec_denylist").entries.size() ==
                  kMaxServerExecDenyEntries);

  YUKINAL_REQUIRE(denylist_rejected(json::array({std::string(kMaxServerExecDenyPatternBytes + 1, 'a')})));
  YUKINAL_REQUIRE(parse_server_exec_denylist(
                      json::array({std::string(kMaxServerExecDenyPatternBytes, 'a')}),
                      "config.server_exec_denylist")
                      .entries.size() == 1);

  // An empty array is a valid deny list that denies nothing.
  YUKINAL_REQUIRE(parse_server_exec_denylist(json::array(), "config.server_exec_denylist")
                      .entries.empty());
}

YUKINAL_TEST(config_server_exec_denylist_errors_are_stable_and_label_prefixed) {
  const std::string pattern_error =
      "config.server_exec_denylist[3].pattern must be a non-empty string of at most 1024 bytes";
  const std::string first_pattern_error =
      "config.server_exec_denylist[0].pattern must be a non-empty string of at most 1024 bytes";
  YUKINAL_REQUIRE(denylist_error(json::array({"ok", "ok", "ok", ""})) == pattern_error);
  YUKINAL_REQUIRE(denylist_error(json::array({"ok", "ok", "ok", "   \t\n"})) == pattern_error);
  YUKINAL_REQUIRE(denylist_error(json::array({"ok", "ok", "ok", std::string("a\0b", 3)})) == pattern_error);
  // A bare non-string, non-object item is rejected as an item, not as a pattern.
  YUKINAL_REQUIRE(denylist_error(json::array({"ok", "ok", "ok", 7})) ==
                  "config.server_exec_denylist[3] must be a string or an object");
  YUKINAL_REQUIRE(denylist_error(json::array({"ok", 7})) ==
                  "config.server_exec_denylist[1] must be a string or an object");
  YUKINAL_REQUIRE(denylist_error(json::array({json{{"regex", true}}})) == first_pattern_error);
  YUKINAL_REQUIRE(denylist_error(json::array({json{{"pattern", false}}})) == first_pattern_error);

  // Every message is label-prefixed and never echoes a command or a pattern.
  for (const auto& malformed :
       json::array({json{{"pattern", "rm -rf /"}}, json("rm -rf /"), json::array({42}),
                    json::array({json{{"pattern", "x"}, {"unknown", true}}}),
                    json::array({json{{"pattern", "(unclosed"}, {"regex", true}}})})) {
    const auto message = denylist_error(malformed);
    YUKINAL_REQUIRE(message.rfind("config.server_exec_denylist", 0) == 0);
    YUKINAL_REQUIRE(message.find("rm -rf /") == std::string::npos);
  }
  YUKINAL_REQUIRE(denylist_error(json::array({json{{"pattern", "x"}, {"regex", 1}}})) ==
                  "config.server_exec_denylist[0].regex must be a boolean");
  YUKINAL_REQUIRE(denylist_error(json::array({json{{"pattern", "x"}, {"case_insensitive", "no"}}})) ==
                  "config.server_exec_denylist[0].case_insensitive must be a boolean");
  YUKINAL_REQUIRE(denylist_error(json::array({json{{"pattern", "("}, {"regex", true}}})) ==
                  "config.server_exec_denylist[0].pattern is not a valid regular expression");
}

YUKINAL_TEST(config_rejects_malformed_server_exec_denylist_in_config) {
  auto unknown_root_key = valid_config();
  unknown_root_key["server_exec_denylists"] = json::array();
  YUKINAL_REQUIRE(config_rejected(unknown_root_key));

  auto non_array = valid_config();
  non_array["server_exec_denylist"] = "rm -rf /";
  YUKINAL_REQUIRE(config_rejected(non_array));

  auto unknown_entry_key = valid_config();
  unknown_entry_key["server_exec_denylist"] =
      json::array({json{{"pattern", "shutdown"}, {"icase", true}}});
  YUKINAL_REQUIRE(config_rejected(unknown_entry_key));

  auto malformed_regex = valid_config();
  malformed_regex["targets"][0]["server_exec_denylist"] =
      json::array({json{{"pattern", "(unclosed"}, {"regex", true}}});
  YUKINAL_REQUIRE(config_rejected(malformed_regex));

  auto too_many = valid_config();
  too_many["targets"][0]["server_exec_denylist"] = json::array();
  for (std::size_t i = 0; i < kMaxServerExecDenyEntries + 1; ++i) {
    too_many["targets"][0]["server_exec_denylist"].push_back("command-" + std::to_string(i));
  }
  YUKINAL_REQUIRE(config_rejected(too_many));
}

YUKINAL_TEST(config_merges_service_level_denylist_into_every_target) {
  auto value = valid_config();
  json service_level = json::array();
  service_level.push_back("shutdown");
  service_level.push_back(json{{"pattern", "^sudo\\s+reboot"}, {"regex", true}});
  value["server_exec_denylist"] = std::move(service_level);
  value["targets"][0]["server_exec_denylist"] = json::array({"rm -rf /"});
  value["targets"].push_back(json{{"target_id", "prod-api-02"},
                                  {"host", "prod.example.test"},
                                  {"port", 22},
                                  {"username", "ops"},
                                  {"known_hosts_file", "known_hosts"},
                                  {"auth", {{"type", "password"}, {"password_env", "PROD_SSH_PASSWORD"}}}});

  const auto config = parse_config(value);
  // ServiceConfig keeps the service-level list unmerged so callers can distinguish.
  YUKINAL_REQUIRE(config.server_exec_denylist.entries.size() == 2);
  YUKINAL_REQUIRE(config.server_exec_denylist.entries[0].pattern == "shutdown");
  YUKINAL_REQUIRE(config.server_exec_denylist.entries[1].pattern == "^sudo\\s+reboot");
  YUKINAL_REQUIRE(config.server_exec_denylist.entries[1].is_regex);

  const auto& first = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(first.server_exec_denylist.entries.size() == 3);
  YUKINAL_REQUIRE(first.server_exec_denylist.entries[0].pattern == "shutdown");
  YUKINAL_REQUIRE(first.server_exec_denylist.entries[1].pattern == "^sudo\\s+reboot");
  YUKINAL_REQUIRE(first.server_exec_denylist.entries[2].pattern == "rm -rf /");
  YUKINAL_REQUIRE(!first.server_exec_denylist.entries[2].is_regex);

  // A target without its own key still receives the service-level entries.
  const auto& second = config.targets.at("prod-api-02");
  YUKINAL_REQUIRE(second.server_exec_denylist.entries.size() == 2);
  YUKINAL_REQUIRE(second.server_exec_denylist.entries[0].pattern == "shutdown");
  YUKINAL_REQUIRE(second.server_exec_denylist.entries[1].pattern == "^sudo\\s+reboot");

  std::string matched;
  YUKINAL_REQUIRE(server_exec_command_is_denied(second.server_exec_denylist, "sudo   reboot now", &matched));
  YUKINAL_REQUIRE(matched == "^sudo\\s+reboot");
}

YUKINAL_TEST(config_probe_only_target_may_carry_a_denylist) {
  auto value = valid_config();
  value["server_exec_denylist"] = json::array({"shutdown"});
  auto& target = value["targets"][0];
  target.erase("known_hosts_file");
  target["probe_only"] = true;
  target["auth"] = {{"type", "probe"}};
  target["server_exec_denylist"] = json::array({"reboot"});

  const auto config = parse_config(value);
  const auto& parsed = config.targets.at("staging-web-01");
  YUKINAL_REQUIRE(parsed.probe_only);
  YUKINAL_REQUIRE(parsed.server_exec_denylist.entries.size() == 2);
  YUKINAL_REQUIRE(parsed.server_exec_denylist.entries[0].pattern == "shutdown");
  YUKINAL_REQUIRE(parsed.server_exec_denylist.entries[1].pattern == "reboot");
}

YUKINAL_TEST(server_exec_matcher_denies_literal_substrings_after_whitespace_collapse) {
  const auto list = parse_server_exec_denylist(json::array({"rm -rf /"}), "config.server_exec_denylist");
  std::string matched;
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "rm -rf /", &matched));
  YUKINAL_REQUIRE(matched == "rm -rf /");
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "sudo rm -rf / --no-preserve-root"));
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "rm   -rf   /"));
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "\trm\n-rf\f/  "));
  // Literal entries match as substrings of the normalized command.
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "rm -rf /tmp/cache"));
  YUKINAL_REQUIRE(!server_exec_command_is_denied(list, "rsync -a /tmp/cache"));
}

YUKINAL_TEST(server_exec_matcher_honours_case_sensitivity_per_entry) {
  const auto sensitive =
      parse_server_exec_denylist(json::array({"shutdown"}), "config.server_exec_denylist");
  YUKINAL_REQUIRE(server_exec_command_is_denied(sensitive, "shutdown -h now"));
  YUKINAL_REQUIRE(!server_exec_command_is_denied(sensitive, "SHUTDOWN -h now"));

  const auto insensitive = parse_server_exec_denylist(
      json::array({json{{"pattern", "shutdown"}, {"case_insensitive", true}}}), "config.server_exec_denylist");
  std::string matched;
  YUKINAL_REQUIRE(server_exec_command_is_denied(insensitive, "SHUTDOWN -h now", &matched));
  YUKINAL_REQUIRE(matched == "shutdown");
  YUKINAL_REQUIRE(server_exec_command_is_denied(insensitive, "Sudo ShUtDoWn"));

  const auto regex_sensitive = parse_server_exec_denylist(
      json::array({json{{"pattern", "^shutdown$"}, {"regex", true}}}), "config.server_exec_denylist");
  YUKINAL_REQUIRE(!server_exec_command_is_denied(regex_sensitive, "SHUTDOWN"));

  const auto regex_insensitive = parse_server_exec_denylist(
      json::array({json{{"pattern", "^shutdown$"}, {"regex", true}, {"case_insensitive", true}}}),
      "config.server_exec_denylist");
  YUKINAL_REQUIRE(server_exec_command_is_denied(regex_insensitive, "SHUTDOWN"));
}

YUKINAL_TEST(server_exec_matcher_handles_regex_entries) {
  const auto list = parse_server_exec_denylist(
      json::array({json{{"pattern", "^sudo\\s+systemctl\\s+(stop|restart)\\s+sshd$"}, {"regex", true}}}),
      "config.server_exec_denylist");
  std::string matched;
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "sudo   systemctl   stop   sshd", &matched));
  YUKINAL_REQUIRE(matched == "^sudo\\s+systemctl\\s+(stop|restart)\\s+sshd$");
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "sudo systemctl restart sshd"));
  YUKINAL_REQUIRE(!server_exec_command_is_denied(list, "systemctl status sshd"));
  YUKINAL_REQUIRE(!server_exec_command_is_denied(list, "sudo systemctl start sshd"));
}

YUKINAL_TEST(server_exec_matcher_reports_the_first_matching_pattern) {
  const json regex_entry = {{"pattern", "rm\\s+-rf\\s+/"}, {"regex", true}};
  const auto list = parse_server_exec_denylist(
      json::array({"shutdown", "reboot", regex_entry}), "config.server_exec_denylist");
  std::string matched;
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "shutdown now", &matched));
  YUKINAL_REQUIRE(matched == "shutdown");

  matched.clear();
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "sudo   rm   -rf   /", &matched));
  YUKINAL_REQUIRE(matched == "rm\\s+-rf\\s+/");

  matched = "untouched";
  YUKINAL_REQUIRE(!server_exec_command_is_denied(list, "uname -a", &matched));
  YUKINAL_REQUIRE(matched == "untouched");

  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "systemctl reboot"));
  YUKINAL_REQUIRE(!server_exec_command_is_denied(list, "now do a REBOOT"));
}

YUKINAL_TEST(server_exec_matcher_allows_everything_for_an_empty_list) {
  const ServerExecDenyList empty;
  YUKINAL_REQUIRE(empty.entries.empty());
  std::string matched = "untouched";
  YUKINAL_REQUIRE(!server_exec_command_is_denied(empty, "rm -rf /", &matched));
  YUKINAL_REQUIRE(matched == "untouched");
  YUKINAL_REQUIRE(!server_exec_command_is_denied(empty, ""));

  const auto from_json = parse_server_exec_denylist(json::array(), "config.server_exec_denylist");
  YUKINAL_REQUIRE(!server_exec_command_is_denied(from_json, "shutdown -h now"));
  std::string untouched = "untouched";
  YUKINAL_REQUIRE(!server_exec_command_is_denied(from_json, "rm -rf /", &untouched));
  YUKINAL_REQUIRE(untouched == "untouched");

  auto value = valid_config();
  const auto config = parse_config(value);
  YUKINAL_REQUIRE(config.server_exec_denylist.entries.empty());
  YUKINAL_REQUIRE(config.targets.at("staging-web-01").server_exec_denylist.entries.empty());
  YUKINAL_REQUIRE(server_exec_denylist_to_json(config.server_exec_denylist).empty());
}
