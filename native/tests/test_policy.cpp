#include "test_support.hpp"
#include "yukinal/policy.hpp"

using nlohmann::json;
using yukinal::ssh_service::ApiError;
using yukinal::ssh_service::ExecRequest;
using yukinal::ssh_service::parse_exec_request;

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
