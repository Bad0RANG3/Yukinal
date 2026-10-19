#include <chrono>
#include <string>
#include <vector>

#include "test_support.hpp"
#include "yukinal/http_service.hpp"
#include "yukinal/policy.hpp"
#include "yukinal/ssh.hpp"

using nlohmann::json;
using yukinal::ssh_service::ApiError;
using yukinal::ssh_service::ExecRequest;
using yukinal::ssh_service::ExecResult;
using yukinal::ssh_service::parse_exec_request;
using yukinal::ssh_service::parse_server_exec_denylist;
using yukinal::ssh_service::server_exec_command_is_denied;
using yukinal::ssh_service::TargetConfig;

namespace {

/// A `server.exec` request aimed at a host that cannot be reached, so any
/// accidental connection attempt fails fast (`127.0.0.1:1`) or burns the whole
/// timeout (`203.0.113.1`, TEST-NET-3, unroutable). `timeout_ms` is deliberately
/// generous: a denial path that returns in well under it proves the blacklist
/// short-circuits before `run_ssh_command_with_limits` is ever entered.
ExecRequest server_exec_request(const std::string& command, std::uint32_t timeout_ms = 5000) {
  ExecRequest request;
  request.operation = "server_exec";
  request.command = command;
  request.timeout_ms = timeout_ms;
  request.max_output_bytes = 4096;
  return request;
}

TargetConfig unreachable_target() {
  TargetConfig target;
  target.target_id = "policy-target";
  target.host = "203.0.113.1";
  target.port = 22;
  target.username = "ops";
  target.allow_server_exec = true;
  return target;
}

}  // namespace

YUKINAL_TEST(policy_accepts_only_structured_service_status) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "service_status"},
       {"parameters", {{"service", "nginx.service"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(accepted);
  YUKINAL_REQUIRE(request.target_id == "staging-web-01");
  YUKINAL_REQUIRE(request.service == "nginx.service");
  YUKINAL_REQUIRE(request.timeout_seconds == 8);
}

YUKINAL_TEST(policy_accepts_service_inventory_without_parameters) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "service_inventory"},
       {"parameters", json::object()},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(accepted);
  YUKINAL_REQUIRE(request.operation == "service_inventory");
  YUKINAL_REQUIRE(request.service.empty());
  YUKINAL_REQUIRE(request.timeout_seconds == 8);

  const bool parameter_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "service_inventory"},
       {"parameters", {{"command", "id"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!parameter_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_accepts_disk_usage_without_parameters) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "disk_usage"},
       {"parameters", json::object()},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(accepted);
  YUKINAL_REQUIRE(request.operation == "disk_usage");
  YUKINAL_REQUIRE(request.timeout_seconds == 8);

  const bool parameter_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "disk_usage"},
       {"parameters", {{"path", "/etc"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!parameter_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_accepts_only_a_bounded_system_snapshot_nonce) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "system_snapshot"},
       {"parameters", {{"nonce", "0123456789abcdef0123456789abcdef"}}},
       {"timeout_seconds", 30}},
      &request, &error);
  YUKINAL_REQUIRE(accepted);
  YUKINAL_REQUIRE(request.operation == "system_snapshot");
  YUKINAL_REQUIRE(request.nonce == "0123456789abcdef0123456789abcdef");

  const bool injected_nonce_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "system_snapshot"},
       {"parameters", {{"nonce", "0123456789abcdef0123456789abcdef;id"}}},
       {"timeout_seconds", 30}},
      &request, &error);
  YUKINAL_REQUIRE(!injected_nonce_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool extra_parameter_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "system_snapshot"},
       {"parameters", {{"nonce", "0123456789abcdef0123456789abcdef"}, {"command", "id"}}},
       {"timeout_seconds", 30}},
      &request, &error);
  YUKINAL_REQUIRE(!extra_parameter_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_requires_a_boolean_for_docker_ps) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_ps"},
       {"parameters", {{"all", true}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(accepted);
  YUKINAL_REQUIRE(request.operation == "docker_ps");
  YUKINAL_REQUIRE(request.include_all_containers);

  const bool string_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_ps"},
       {"parameters", {{"all", "true"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!string_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool extra_parameter_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_ps"},
       {"parameters", {{"all", false}, {"command", "id"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!extra_parameter_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_bounds_docker_logs_and_inspect_references) {
  ExecRequest request;
  ApiError error;
  const bool logs_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_logs"},
       {"parameters", {{"container", "api_1.2-3"}, {"tail", 120}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(logs_accepted);
  YUKINAL_REQUIRE(request.container == "api_1.2-3");
  YUKINAL_REQUIRE(request.tail == 120);

  const bool injected_container_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_logs"},
       {"parameters", {{"container", "api;id"}, {"tail", 120}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(!injected_container_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool oversized_tail_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_logs"},
       {"parameters", {{"container", "api"}, {"tail", 501}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(!oversized_tail_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool inspect_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_inspect"},
       {"parameters", {{"container", "api"}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(inspect_accepted);
  YUKINAL_REQUIRE(request.container == "api");

  const bool inspect_extra_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_inspect"},
       {"parameters", {{"container", "api"}, {"format", "{{json .}}"}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(!inspect_extra_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_bounds_systemd_and_package_inspection) {
  ExecRequest request;
  ApiError error;
  const bool systemd_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "systemd_inspect"},
       {"parameters", {{"service", "nginx.service"}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(systemd_accepted);
  YUKINAL_REQUIRE(request.unit == "nginx.service");

  const bool unsafe_systemd_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "systemd_inspect"},
       {"parameters", {{"service", "nginx.service;id"}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(!unsafe_systemd_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool package_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "package_inspect"},
       {"parameters", {{"manager", "apt"}, {"package", "python3.12-dev"}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(package_accepted);
  YUKINAL_REQUIRE(request.package_manager == "apt");
  YUKINAL_REQUIRE(request.package == "python3.12-dev");

  const bool unsupported_manager_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "package_inspect"},
       {"parameters", {{"manager", "apk"}, {"package", "nginx"}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(!unsupported_manager_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool injected_package_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "package_inspect"},
       {"parameters", {{"manager", "dnf"}, {"package", "nginx;id"}}},
       {"timeout_seconds", 10}},
      &request, &error);
  YUKINAL_REQUIRE(!injected_package_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_bounds_effectful_docker_systemd_and_package_operations) {
  ExecRequest request;
  ApiError error;
  const bool docker_restart_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_restart"},
       {"parameters", {{"container", "api"}, {"grace_period_seconds", 15}}},
       {"timeout_seconds", 30}},
      &request, &error);
  YUKINAL_REQUIRE(docker_restart_accepted);
  YUKINAL_REQUIRE(request.container == "api");
  YUKINAL_REQUIRE(request.grace_period_seconds == 15);

  const bool unsafe_docker_restart_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "docker_restart"},
       {"parameters", {{"container", "api;id"}, {"grace_period_seconds", 15}}},
       {"timeout_seconds", 30}},
      &request, &error);
  YUKINAL_REQUIRE(!unsafe_docker_restart_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool systemd_restart_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "systemd_restart"},
       {"parameters", {{"service", "nginx.service"}, {"timeout_seconds", 120}}},
       {"timeout_seconds", 120}},
      &request, &error);
  YUKINAL_REQUIRE(systemd_restart_accepted);
  YUKINAL_REQUIRE(request.unit == "nginx.service");
  YUKINAL_REQUIRE(request.timeout_seconds == 120);

  const bool mismatched_timeout_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "systemd_restart"},
       {"parameters", {{"service", "nginx.service"}, {"timeout_seconds", 120}}},
       {"timeout_seconds", 30}},
      &request, &error);
  YUKINAL_REQUIRE(!mismatched_timeout_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_timeout");

  const bool package_install_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "package_install"},
       {"parameters", {{"manager", "apt"},
                        {"package", "nginx"},
                        {"version", "1.27.0"},
                        {"timeout_seconds", 180}}},
       {"timeout_seconds", 180}},
      &request, &error);
  YUKINAL_REQUIRE(package_install_accepted);
  YUKINAL_REQUIRE(request.package_manager == "apt");
  YUKINAL_REQUIRE(request.package == "nginx");
  YUKINAL_REQUIRE(request.package_version == "1.27.0");
  YUKINAL_REQUIRE(request.timeout_seconds == 180);

  const bool invalid_version_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "package_install"},
       {"parameters", {{"manager", "dnf"},
                        {"package", "nginx"},
                        {"version", "1;id"},
                        {"timeout_seconds", 180}}},
       {"timeout_seconds", 180}},
      &request, &error);
  YUKINAL_REQUIRE(!invalid_version_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_accepts_host_key_probe_without_parameters) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "host_key_probe"},
       {"parameters", json::object()},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(accepted);
  YUKINAL_REQUIRE(request.operation == "host_key_probe");

  const bool extra_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "host_key_probe"},
       {"parameters", {{"host", "attacker.example"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!extra_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_accepts_bounded_sftp_directory_paths) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_list"},
       {"parameters", {{"path", "/var/log"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(accepted);
  YUKINAL_REQUIRE(request.operation == "sftp_list");
  YUKINAL_REQUIRE(request.path == "/var/log");

  const bool nul_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_list"},
       {"parameters", {{"path", std::string("/var\0/log", 9)}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!nul_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool extra_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_list"},
       {"parameters", {{"path", "/var/log"}, {"command", "id"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!extra_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_validates_sftp_read_limits_and_metadata_parameters) {
  ExecRequest request;
  ApiError error;
  const bool read_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_read"},
       {"parameters", {{"path", "/var/log/app.log"}, {"max_bytes", 1048576}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(read_accepted);
  YUKINAL_REQUIRE(request.path == "/var/log/app.log");
  YUKINAL_REQUIRE(request.max_bytes == 1048576);

  const bool oversized_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_read"},
       {"parameters", {{"path", "/var/log/app.log"}, {"max_bytes", 1048577}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!oversized_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool negative_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_read"},
       {"parameters", {{"path", "/var/log/app.log"}, {"max_bytes", -1}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!negative_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool stat_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_stat"},
       {"parameters", {{"path", "/var/log/app.log"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(stat_accepted);
  YUKINAL_REQUIRE(request.operation == "sftp_stat");
}

YUKINAL_TEST(policy_validates_sftp_mutations_and_binary_payload_limits) {
  ExecRequest request;
  ApiError error;
  const bool write_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_write"},
       {"parameters", {{"path", "/srv/app/config.bin"}, {"content_base64", "AAEC/w=="}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(write_accepted);
  YUKINAL_REQUIRE(request.operation == "sftp_write");
  YUKINAL_REQUIRE(request.content == std::vector<unsigned char>({0, 1, 2, 255}));

  const bool invalid_base64_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_write"},
       {"parameters", {{"path", "/srv/app/config.bin"}, {"content_base64", "AA=A"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!invalid_base64_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const std::string too_large_write = std::string(((512 * 1024 + 2) / 3) * 4, 'A');
  const bool oversized_write_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_write"},
       {"parameters", {{"path", "/srv/app/config.bin"}, {"content_base64", too_large_write}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!oversized_write_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  std::string at_backup_limit(((1024 * 1024 + 2) / 3) * 4, 'A');
  at_backup_limit.replace(at_backup_limit.size() - 2, 2, "==");
  const bool exclusive_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_create_exclusive"},
       {"parameters", {{"path", "/srv/app/config.backup"}, {"content_base64", at_backup_limit}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(exclusive_accepted);
  YUKINAL_REQUIRE(request.content.size() == 1024 * 1024);

  const bool remove_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_remove_file"},
       {"parameters", {{"path", "/srv/app/config.backup"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(remove_accepted);
  YUKINAL_REQUIRE(request.operation == "sftp_remove_file");

  const bool link_count_line_break_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_link_count"},
       {"parameters", {{"path", "/srv/app/config\nbackup"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!link_count_line_break_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_accepts_only_sibling_paths_for_sftp_no_replace_publication) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_publish_no_replace"},
       {"parameters", {{"source", "/srv/app/.config.part"},
                       {"destination", "/srv/app/config"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(accepted);
  YUKINAL_REQUIRE(request.source_path == "/srv/app/.config.part");
  YUKINAL_REQUIRE(request.destination_path == "/srv/app/config");

  const bool different_parent_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_publish_no_replace"},
       {"parameters", {{"source", "/srv/app/.config.part"},
                       {"destination", "/srv/other/config"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!different_parent_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool traversal_path_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_publish_no_replace"},
       {"parameters", {{"source", "/srv/app/../stage"},
                        {"destination", "/srv/app/config"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!traversal_path_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");

  const bool repeated_separator_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "sftp_publish_no_replace"},
       {"parameters", {{"source", "/srv//stage"},
                        {"destination", "/srv/file"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!repeated_separator_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_rejects_arbitrary_shell_fields) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "exec"},
       {"parameters", {{"command", "id"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!accepted);
  YUKINAL_REQUIRE(error.code == "unsupported_operation");
}

YUKINAL_TEST(policy_accepts_server_exec_only_with_bounded_limits) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "server_exec"},
       {"parameters", {{"command", "/bin/sh -c 'printf ok'"},
                        {"max_output_bytes", 4096},
                        {"timeout_ms", 1500}}},
       {"timeout_seconds", 2}},
      &request, &error);
  YUKINAL_REQUIRE(accepted);
  YUKINAL_REQUIRE(request.command == "/bin/sh -c 'printf ok'");
  YUKINAL_REQUIRE(request.max_output_bytes == 4096);
  YUKINAL_REQUIRE(request.timeout_ms == 1500);

  const bool mismatched_timeout_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "server_exec"},
       {"parameters", {{"command", "id"}, {"max_output_bytes", 4096}, {"timeout_ms", 1500}}},
       {"timeout_seconds", 3}},
      &request, &error);
  YUKINAL_REQUIRE(!mismatched_timeout_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_timeout");

  const bool extra_parameter_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "server_exec"},
       {"parameters", {{"command", "id"}, {"max_output_bytes", 4096},
                        {"timeout_ms", 1500}, {"host", "other.example"}}},
       {"timeout_seconds", 2}},
      &request, &error);
  YUKINAL_REQUIRE(!extra_parameter_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_rejects_shell_metacharacters_and_path_parameters) {
  ExecRequest request;
  ApiError error;
  const bool accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "service_status"},
       {"parameters", {{"service", "nginx;id"}}},
       {"timeout_seconds", 8}},
      &request, &error);
  YUKINAL_REQUIRE(!accepted);
  YUKINAL_REQUIRE(error.code == "invalid_parameters");
}

YUKINAL_TEST(policy_rejects_invalid_timeout_and_unmapped_host_input) {
  ExecRequest request;
  ApiError error;
  const bool timeout_accepted = parse_exec_request(
      {{"target_id", "staging-web-01"},
       {"operation", "service_status"},
       {"parameters", {{"service", "nginx"}}},
       {"timeout_seconds", 31}},
      &request, &error);
  YUKINAL_REQUIRE(!timeout_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_timeout");

  const bool host_accepted = parse_exec_request(
      {{"target_id", "127.0.0.1"},
       {"host", "127.0.0.1"},
       {"operation", "service_status"},
       {"parameters", {{"service", "nginx"}}},
       {"timeout_seconds", 5}},
      &request, &error);
  YUKINAL_REQUIRE(!host_accepted);
  YUKINAL_REQUIRE(error.code == "invalid_request");
}

// ---------------------------------------------------------------------------
// server.exec command blacklist (denylist) enforcement.
//
// The blacklist is a pure additional deny layer inside the SSH service: it runs
// after the one-shot `allow_server_exec` capability gate and before the shell
// command string is built or any SSH connection is attempted. Every case below
// drives the real `run_operation` entry point, which is what the HTTP service
// calls, so the ordering claim is proven at the same seam the Agent reaches.
// ---------------------------------------------------------------------------

YUKINAL_TEST(policy_denylist_denies_server_exec_before_any_connection_attempt) {
  TargetConfig target = unreachable_target();
  target.server_exec_denylist = parse_server_exec_denylist(json::array({"rm -rf /"}), "test");

  const ExecRequest request = server_exec_request("rm -rf /");
  const auto started = std::chrono::steady_clock::now();
  const ExecResult result = yukinal::ssh_service::run_operation(target, request);
  const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
      std::chrono::steady_clock::now() - started);

  // The denial is an immediate, fail-closed return: exact error code, no
  // completed result, and no round trip to the (unroutable) target.
  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "denied_by_policy");
  YUKINAL_REQUIRE(elapsed < std::chrono::milliseconds(1000));

  // The message is a policy fact, never a command echo.
  YUKINAL_REQUIRE(result.error_message.find("rm -rf /") == std::string::npos);
  YUKINAL_REQUIRE(result.error_message.find("rm") == std::string::npos);
  YUKINAL_REQUIRE(result.error_message.find("blacklist") != std::string::npos);
}

YUKINAL_TEST(policy_empty_denylist_does_not_deny_server_exec) {
  TargetConfig target = unreachable_target();
  // Whitespace and a fixed port give a deterministic, fast transport failure;
  // only the absence of the policy denial is asserted, never the code itself.
  target.host = "127.0.0.1";
  target.port = 1;
  target.server_exec_denylist = parse_server_exec_denylist(json::array(), "test");

  const ExecRequest request = server_exec_request("id");
  const ExecResult result = yukinal::ssh_service::run_operation(target, request);

  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code != "denied_by_policy");
}

YUKINAL_TEST(policy_denylist_denies_whitespace_obfuscated_command) {
  TargetConfig target = unreachable_target();
  target.server_exec_denylist = parse_server_exec_denylist(json::array({"rm -rf /"}), "test");

  // Runs of whitespace collapse before matching, so the padded form is denied.
  const ExecRequest request = server_exec_request("rm   -rf   /");
  const ExecResult result = yukinal::ssh_service::run_operation(target, request);

  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "denied_by_policy");
  YUKINAL_REQUIRE(result.error_message.find("rm") == std::string::npos);
}

YUKINAL_TEST(policy_denylist_denies_through_a_regex_entry) {
  TargetConfig target = unreachable_target();
  target.server_exec_denylist = parse_server_exec_denylist(
      json::array({json{{"pattern", "^shutdown\\b"}, {"regex", true}}, std::string("dnf remove")}),
      "test");

  const ExecRequest request = server_exec_request("shutdown -h now");
  const ExecResult result = yukinal::ssh_service::run_operation(target, request);

  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "denied_by_policy");
  YUKINAL_REQUIRE(result.error_message.find("shutdown") == std::string::npos);
}

YUKINAL_TEST(policy_denylist_never_masks_the_one_shot_capability_gate) {
  TargetConfig target = unreachable_target();
  target.allow_server_exec = false;  // capability gate must stay authoritative
  target.server_exec_denylist = parse_server_exec_denylist(json::array({"rm -rf /"}), "test");

  const ExecRequest request = server_exec_request("rm -rf /");
  const ExecResult result = yukinal::ssh_service::run_operation(target, request);

  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "operation_not_allowed");
  YUKINAL_REQUIRE(result.error_message == "server.exec requires a one-shot host authorization");
}

YUKINAL_TEST(policy_denylist_only_inspects_the_command_not_workdir_or_env) {
  // The same denylist that denies `rm -rf /` as a command must NOT deny when the
  // blacklisted text only appears in `workdir`/`env`. The target points at a
  // closed loopback port so the non-denial falls through to a fast transport
  // failure -- but the denylist is genuinely present on this target, so the
  // `!= denied_by_policy` assertion below would fail if the matcher ever started
  // reading workdir/env.
  TargetConfig target = unreachable_target();
  target.host = "127.0.0.1";
  target.port = 1;
  target.server_exec_denylist = parse_server_exec_denylist(json::array({"rm -rf /"}), "test");

  ExecRequest request = server_exec_request("id");
  request.workdir = "/srv/rm -rf /";
  request.environment = {{"PAYLOAD", "rm -rf /"}};
  const ExecResult result = yukinal::ssh_service::run_operation(target, request);

  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code != "denied_by_policy");

  // Positive control on the very same target+list: the identical text as the
  // command IS denied, so the negative result above is not an inert list.
  const ExecResult denied =
      yukinal::ssh_service::run_operation(target, server_exec_request("rm -rf /"));
  YUKINAL_REQUIRE(denied.error_code == "denied_by_policy");
}

YUKINAL_TEST(policy_denylist_reports_the_matched_pattern_to_its_caller) {
  const auto list = parse_server_exec_denylist(json::array({"rm -rf /"}), "test");
  std::string matched;
  YUKINAL_REQUIRE(server_exec_command_is_denied(list, "rm  -rf  /", &matched));
  YUKINAL_REQUIRE(matched == "rm -rf /");
  // The out-parameter is only written on a match, so a non-match leaves it
  // untouched (the caller must not read it as a pattern in that case).
  matched.clear();
  YUKINAL_REQUIRE(!server_exec_command_is_denied(list, "df -h", &matched));
  YUKINAL_REQUIRE(matched.empty());
}

// The export must not silently become a second, divergent mapping: this guards
// the generic fall-through that the internal call site still shares.
YUKINAL_TEST(policy_error_status_mapping_shares_the_generic_fallback) {
  ExecResult completed;
  completed.completed = true;
  // `error_status_for` is pure and never inspects `completed`, so it is safe to
  // call for a succeeded result too; the endpoint only calls it for failures.
  YUKINAL_REQUIRE(yukinal::ssh_service::error_status_for(completed) == 502);
}

// ---------------------------------------------------------------------------
// HTTP status mapping for `denied_by_policy` (now a testable free function).
// ---------------------------------------------------------------------------

YUKINAL_TEST(policy_error_status_maps_denied_by_policy_to_403) {
  ExecResult result;
  result.error_code = "denied_by_policy";
  YUKINAL_REQUIRE(yukinal::ssh_service::error_status_for(result) == 403);
}

YUKINAL_TEST(policy_error_status_keeps_every_pre_existing_mapping) {
  const auto status_of = [](const char* code) {
    ExecResult result;
    result.error_code = code;
    return yukinal::ssh_service::error_status_for(result);
  };
  YUKINAL_REQUIRE(status_of("invalid_parameters") == 422);
  YUKINAL_REQUIRE(status_of("timeout") == 504);
  YUKINAL_REQUIRE(status_of("concurrent_change") == 409);
  YUKINAL_REQUIRE(status_of("already_exists") == 409);
  YUKINAL_REQUIRE(status_of("resolver_busy") == 503);
  YUKINAL_REQUIRE(status_of("resolver_unavailable") == 503);
  YUKINAL_REQUIRE(status_of("replace_unsupported") == 501);
  // Unmapped codes and an empty code still fall through to the generic 502.
  YUKINAL_REQUIRE(status_of("ssh_error") == 502);
  YUKINAL_REQUIRE(status_of("") == 502);
}
