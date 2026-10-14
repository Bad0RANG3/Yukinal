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
