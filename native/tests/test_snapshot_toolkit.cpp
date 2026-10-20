#include <set>
#include "test_support.hpp"
#include "yukinal/snapshot_toolkit.hpp"

using namespace yukinal::ssh_service;

namespace {
const std::string nonce(32, 'a');
std::string frame(const std::string& id, const std::string& status = "0") {
  const auto prefix = "__YK_SNAPSHOT_" + nonce + "_" + id;
  return prefix + "_BEGIN__\nvalue\n" + prefix + "_END_" + status + "__\n";
}
std::string required_frames() {
  std::string text;
  for (const auto& p : snapshot_toolkit_catalog().at("probes"))
    if (p.at("required").get<bool>()) text += frame(p.at("id"));
  return text;
}
}

YUKINAL_TEST(snapshot_toolkit_batches_unique_read_only_probes_with_deadlines) {
  std::set<std::string> ids;
  int required = 0;
  int docker_info = 0;
  for (const auto& p : snapshot_toolkit_catalog().at("probes")) {
    YUKINAL_REQUIRE(ids.insert(p.at("id")).second);
    required += p.at("required").get<bool>();
    docker_info += p.at("command").get<std::string>().find("docker info") != std::string::npos;
  }
  YUKINAL_REQUIRE(required == 22);
  YUKINAL_REQUIRE(docker_info == 1);
  const auto command = build_system_snapshot_command(nonce);
  YUKINAL_REQUIRE(command.find("timeout -k 1 2 sh -c") != std::string::npos);
  YUKINAL_REQUIRE(command.find("yk_snapshot_remaining=49152") != std::string::npos);
  YUKINAL_REQUIRE(command.find("export yk_docker_status") != std::string::npos);
  bool rejected = false;
  try { (void)build_system_snapshot_command("'; touch /tmp/invalid"); }
  catch (const std::invalid_argument&) { rejected = true; }
  YUKINAL_REQUIRE(rejected);
}

YUKINAL_TEST(snapshot_toolkit_accepts_legacy_and_keeps_optional_network_gpu_security) {
  auto text = required_frames();
  YUKINAL_REQUIRE(parse_system_snapshot_outputs(text, nonce).size() == 22);
  text += frame("network_addresses") + frame("gpu_nvidia", "127") + frame("security_identity");
  const auto parsed = parse_system_snapshot_outputs(text, nonce);
  YUKINAL_REQUIRE(parsed.size() == 25);
  YUKINAL_REQUIRE(parsed.at("gpu_nvidia").at("exitCode") == 127);
  YUKINAL_REQUIRE(parsed.at("network_addresses").at("stdout") == "value");
}

YUKINAL_TEST(snapshot_toolkit_rejects_partial_duplicate_and_invalid_status_extensions) {
  for (const auto& tail : {frame("gpu_nvidia", "0oops"), frame("gpu_nvidia", "256"),
                           frame("gpu_nvidia") + frame("gpu_nvidia"),
                           "__YK_SNAPSHOT_" + nonce + "_gpu_nvidia_END_0__\n"}) {
    bool rejected = false;
    try { (void)parse_system_snapshot_outputs(required_frames() + tail, nonce); }
    catch (const std::runtime_error&) { rejected = true; }
    YUKINAL_REQUIRE(rejected);
  }
}
