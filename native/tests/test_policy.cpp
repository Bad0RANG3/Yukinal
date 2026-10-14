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
