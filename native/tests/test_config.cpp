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
